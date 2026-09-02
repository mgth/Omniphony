//! First-order shoebox early reflections for the binaural stage.
//!
//! Externalization aid: an anechoic HRTF render almost always sounds
//! "inside the head" regardless of calibration — the missing cue is room
//! acoustics. This module adds the six first-order image sources of a
//! shoebox room (listener at the room centre, world-fixed walls): each
//! reflection is a delayed, attenuated, broadband-ILD-panned copy of the
//! channel signal. No per-reflection HRIR convolution — per reflection and
//! ear the steady-state cost is one smoothed fractional ring-buffer read
//! and one multiply, which keeps the whole bank cheap enough for many
//! channels on constrained hardware.
//!
//! The direct path keeps zero propagation delay (A/V sync unchanged);
//! reflection delays are *relative* to the direct path
//! (`(d_image − d_direct) / c ≥ 0`). The direct/reflected energy ratio then
//! falls naturally with source distance, which is exactly the distance cue
//! we are after.

/// Speed of sound (m/s), matching `itd.rs`.
const SPEED_OF_SOUND: f32 = 343.0;

/// Ring capacity in seconds. Bounds the relative reflection delay; with room
/// dimensions clamped to [`MAX_ROOM_M`] the longest first-order detour stays
/// well below this.
pub const RING_CAPACITY_S: f32 = 0.25;

/// Per-axis room size clamp (m).
pub const MIN_ROOM_M: f32 = 1.0;
pub const MAX_ROOM_M: f32 = 20.0;

/// Margin (m) keeping the (clamped) source strictly inside the room so an
/// image can never coincide with the listener.
const WALL_MARGIN_M: f32 = 0.05;

/// Delay ramp speed in delay-samples per output sample (same policy as
/// [`crate::delay_line::DelayLine`]): no discontinuities, a full-scale change
/// completes in at most the delay span itself.
const DELAY_RAMP_RATE: f32 = 1.0;

/// One-pole smoothing coefficient for tap gains (~1.5 ms at 48 kHz).
const GAIN_SMOOTH: f32 = 0.015;

/// Number of first-order images of a shoebox (one per wall).
pub const NUM_REFLECTIONS: usize = 6;

/// Mirror `src` (listener-relative metres, listener at the room centre)
/// across each of the six walls of a `room` (full extents, metres).
///
/// Sources outside the room are first clamped just inside the walls — the
/// geometry stays valid for any `unit_scale_m`.
pub fn first_order_images(src_m: [f32; 3], room_m: [f32; 3]) -> [[f32; 3]; NUM_REFLECTIONS] {
    let mut half = [0.0f32; 3];
    let mut s = src_m;
    for a in 0..3 {
        half[a] = (room_m[a].clamp(MIN_ROOM_M, MAX_ROOM_M)) * 0.5;
        s[a] = s[a].clamp(-(half[a] - WALL_MARGIN_M), half[a] - WALL_MARGIN_M);
    }
    let mut out = [[0.0f32; 3]; NUM_REFLECTIONS];
    for a in 0..3 {
        let mut pos = s;
        pos[a] = 2.0 * half[a] - s[a];
        out[a * 2] = pos;
        let mut neg = s;
        neg[a] = -2.0 * half[a] - s[a];
        out[a * 2 + 1] = neg;
    }
    out
}

/// One smoothed fractional read tap (delay in samples + linear gain).
#[derive(Clone, Copy, Default)]
struct Tap {
    delay: f32,
    delay_target: f32,
    gain: f32,
    gain_target: f32,
}

impl Tap {
    #[inline]
    fn step(&mut self) {
        let d = self.delay_target - self.delay;
        if d.abs() <= DELAY_RAMP_RATE {
            self.delay = self.delay_target;
        } else {
            self.delay += DELAY_RAMP_RATE * d.signum();
        }
        self.gain += (self.gain_target - self.gain) * GAIN_SMOOTH;
    }
}

/// Per-channel reflection bank: one shared ring buffer written once per
/// sample, read by `NUM_REFLECTIONS × 2` smoothed taps (left/right ear).
pub struct ReflectionBank {
    ring: Vec<f32>,
    write_pos: usize,
    taps_l: [Tap; NUM_REFLECTIONS],
    taps_r: [Tap; NUM_REFLECTIONS],
    sample_rate: u32,
}

impl ReflectionBank {
    pub fn new(sample_rate: u32) -> Self {
        let cap = (RING_CAPACITY_S * sample_rate as f32).ceil() as usize + 2;
        Self {
            ring: vec![0.0; cap],
            write_pos: 0,
            taps_l: Default::default(),
            taps_r: Default::default(),
            sample_rate,
        }
    }

    /// Update one reflection's targets: relative delay (s) and per-ear gains.
    /// Called once per block per reflection.
    pub fn set_targets(&mut self, idx: usize, delay_s: f32, gain_l: f32, gain_r: f32) {
        let max = (self.ring.len() - 2) as f32;
        let d = (delay_s * self.sample_rate as f32).clamp(0.0, max);
        for (tap, gain) in [
            (&mut self.taps_l[idx], gain_l),
            (&mut self.taps_r[idx], gain_r),
        ] {
            tap.delay_target = d;
            tap.gain_target = gain;
            // While the tap is (near) silent a delay jump is inaudible — snap
            // instead of sweeping, so a fresh tap doesn't chirp its way from
            // delay 0 to the target while tracking the live signal.
            if tap.gain.abs() < 1e-4 {
                tap.delay = d;
            }
        }
    }

    /// Fade every tap out (e.g. when the channel goes silent) so re-enabling
    /// does not click.
    pub fn mute_targets(&mut self) {
        for t in self.taps_l.iter_mut().chain(self.taps_r.iter_mut()) {
            t.gain_target = 0.0;
        }
    }

    /// Write one input sample and return the summed (left, right) reflection
    /// contribution.
    #[inline]
    pub fn process(&mut self, input: f32) -> (f32, f32) {
        let cap = self.ring.len();
        self.ring[self.write_pos] = input;

        let mut l = 0.0f32;
        let mut r = 0.0f32;
        for i in 0..NUM_REFLECTIONS {
            let tl = &mut self.taps_l[i];
            tl.step();
            l += tl.gain * read_frac(&self.ring, cap, self.write_pos, tl.delay);
            let tr = &mut self.taps_r[i];
            tr.step();
            r += tr.gain * read_frac(&self.ring, cap, self.write_pos, tr.delay);
        }

        self.write_pos += 1;
        if self.write_pos >= cap {
            self.write_pos = 0;
        }
        (l, r)
    }
}

/// Linear-interpolated read at `delay` samples behind `write_pos` (which still
/// points at the sample just written).
#[inline]
fn read_frac(ring: &[f32], cap: usize, write_pos: usize, delay: f32) -> f32 {
    let lo = delay.floor();
    let frac = delay - lo;
    let lo = lo as usize;
    let idx0 = (write_pos + cap - lo % cap) % cap;
    let idx1 = (idx0 + cap - 1) % cap;
    ring[idx0] * (1.0 - frac) + ring[idx1] * frac
}

/// Speed of sound accessor so callers share one constant.
#[inline]
pub fn speed_of_sound() -> f32 {
    SPEED_OF_SOUND
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_source_images_sit_one_room_dimension_away() {
        let room = [4.0, 6.0, 3.0];
        let images = first_order_images([0.0, 0.0, 0.0], room);
        // For a centred source the image across wall ±a sits at ±room[a].
        assert_eq!(images[0][0], 4.0);
        assert_eq!(images[1][0], -4.0);
        assert_eq!(images[2][1], 6.0);
        assert_eq!(images[3][1], -6.0);
        assert_eq!(images[4][2], 3.0);
        assert_eq!(images[5][2], -3.0);
    }

    #[test]
    fn outside_source_is_clamped_inside() {
        let room = [4.0, 4.0, 4.0];
        let images = first_order_images([100.0, 0.0, 0.0], room);
        // Clamped to x = 2 - margin; +x image at 2*2 - x stays just inside 2+margin.
        assert!(images[0][0] > 2.0 && images[0][0] < 2.1);
        for img in images {
            assert!(img.iter().all(|v| v.is_finite()));
        }
    }

    #[test]
    fn bank_delays_and_attenuates() {
        let mut bank = ReflectionBank::new(48_000);
        // One active tap: 10-sample delay, gain 0.5 on the left only. Pre-set
        // current = target by letting it settle on silence first.
        bank.set_targets(0, 10.0 / 48_000.0, 0.5, 0.0);
        for _ in 0..4_000 {
            bank.process(0.0);
        }
        // Impulse, then read where it must come out.
        let mut outs = Vec::new();
        outs.push(bank.process(1.0));
        for _ in 0..20 {
            outs.push(bank.process(0.0));
        }
        let (l10, r10) = outs[10];
        assert!((l10 - 0.5).abs() < 1e-3, "left tap at 10 smp: {l10}");
        assert!(r10.abs() < 1e-6, "right must stay silent: {r10}");
        // Nothing significant elsewhere.
        for (i, &(l, _)) in outs.iter().enumerate() {
            if i != 10 {
                assert!(l.abs() < 1e-3, "leak at {i}: {l}");
            }
        }
    }

    #[test]
    fn gain_changes_are_smoothed() {
        let mut bank = ReflectionBank::new(48_000);
        bank.set_targets(0, 0.0, 1.0, 1.0);
        for _ in 0..4_000 {
            bank.process(1.0); // settle: DC input, gain 1
        }
        let (settled, _) = bank.process(1.0);
        assert!((settled - 1.0).abs() < 1e-2);
        // Drop the gain target to 0: output must move gradually, not jump.
        bank.set_targets(0, 0.0, 0.0, 0.0);
        let (next, _) = bank.process(1.0);
        assert!(next > 0.9, "gain jumped instead of smoothing: {next}");
    }
}
