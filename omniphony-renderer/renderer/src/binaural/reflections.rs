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

/// Ring capacity in seconds. Bounds the relative reflection delay; with room
/// dimensions clamped to [`MAX_ROOM_M`] the longest first-order detour stays
/// well below this.
pub const RING_CAPACITY_S: f32 = 0.25;

/// Per-axis room size clamp (m).
pub const MIN_ROOM_M: f32 = 1.0;
pub const MAX_ROOM_M: f32 = 20.0;

/// Half-extent margin (m) the automatic room floor keeps beyond the scene
/// (see [`room_containing_scene`]): an object at the ADM boundary is then
/// this far from its wall, and the wall's image trails the direct sound by
/// twice that — 0.7 m, ≈2 ms — instead of sitting on top of it. 0.35 makes
/// the default 2.7 m ceiling exactly the floor at unit scale 1.
pub const ROOM_FLOOR_MARGIN_M: f32 = 0.35;

/// Margin (m) keeping the (clamped) source strictly inside the room so an
/// image can never coincide with the listener.
const WALL_MARGIN_M: f32 = 0.05;

/// Delay ramp speed in delay-samples per output sample (same policy as
/// [`crate::delay_line::DelayLine`]): no discontinuities, a full-scale change
/// completes in at most the delay span itself.
const DELAY_RAMP_RATE: f32 = 1.0;

/// One-pole smoothing coefficient for tap gains at 48 kHz (~1.5 ms). Other
/// rates get the coefficient with the same time constant, see
/// [`gain_smooth_for`].
const GAIN_SMOOTH_48K: f32 = 0.015;

/// Per-sample gain smoothing coefficient at `sample_rate`, with the time
/// constant of [`GAIN_SMOOTH_48K`]: the pole is raised to the rate ratio,
/// so the fade takes the same milliseconds at 96 kHz as at 48.
fn gain_smooth_for(sample_rate: u32) -> f32 {
    1.0 - crate::dsp::iir::pole_at_rate(1.0 - GAIN_SMOOTH_48K, sample_rate)
}

/// Number of first-order images of a shoebox (one per wall).
pub const NUM_REFLECTIONS: usize = 6;

/// Range of the wall high-frequency cutoff (Hz). At [`MAX_WALL_CUTOFF_HZ`]
/// the wall filter is bypassed exactly (bare plaster); lower values absorb
/// the treble the way carpet and curtains do.
pub const MIN_WALL_CUTOFF_HZ: f32 = 1_000.0;
pub const MAX_WALL_CUTOFF_HZ: f32 = 20_000.0;

/// One-pole low-pass coefficient for `cutoff_hz` at `sample_rate`, in the
/// `state += (x − state)·(1 − a)` form: `a = exp(−2π·fc/fs)`. A cutoff at or
/// above [`MAX_WALL_CUTOFF_HZ`] gives exactly 0, i.e. the filter passes its
/// input untouched, so "no absorption" is bit-transparent rather than a
/// 20 kHz roll-off that still shaves the top octave at 48 kHz.
pub fn lowpass_coeff(cutoff_hz: f32, sample_rate: u32) -> f32 {
    if cutoff_hz >= MAX_WALL_CUTOFF_HZ {
        0.0
    } else {
        crate::dsp::iir::one_pole_pole(cutoff_hz.max(1.0), sample_rate)
    }
}

/// Half-extents of a `room` (full extents, metres), each axis clamped to
/// [`MIN_ROOM_M`]..[`MAX_ROOM_M`].
fn half_extents(room_m: [f32; 3]) -> [f32; 3] {
    let mut half = [0.0f32; 3];
    for a in 0..3 {
        half[a] = (room_m[a].clamp(MIN_ROOM_M, MAX_ROOM_M)) * 0.5;
    }
    half
}

/// `room` (full extents, metres) grown, axis by axis, to contain the scene:
/// the ADM cube spans `±unit_scale_m` on every world axis, and the walls are
/// world-fixed (the head rotates, the scene does not), so each half-extent
/// is floored at `unit_scale_m + ROOM_FLOOR_MARGIN_M`, within
/// [`MAX_ROOM_M`]. An axis the user already sized larger is left alone.
///
/// Without the floor, a scene larger than the room had every boundary
/// object pulled back to the wall by [`clamp_into_room`] — valid geometry,
/// but the reflections of the wall rather than of the object, and from the
/// nearest wall a near-coincident copy of the direct sound. The configured
/// dimensions are therefore a *minimum*: a room smaller than the scene it
/// holds has no physical reading anyway.
pub fn room_containing_scene(room_m: [f32; 3], unit_scale_m: f32) -> [f32; 3] {
    let floor = 2.0 * (unit_scale_m.max(0.0) + ROOM_FLOOR_MARGIN_M);
    let mut out = room_m;
    for extent in &mut out {
        *extent = extent.max(floor).min(MAX_ROOM_M);
    }
    out
}

/// `src` (listener-relative metres, listener at the room centre) pulled just
/// inside the walls of `room`, by `WALL_MARGIN_M`. The image-source
/// geometry is only meaningful for a source inside the room; this is the
/// source [`first_order_images`] actually mirrors, and therefore the one the
/// direct-path reference (distance, hence the relative delays) must use too
/// — a source that is outside the room and left there makes its own images
/// arrive *before* it.
pub fn clamp_into_room(src_m: [f32; 3], room_m: [f32; 3]) -> [f32; 3] {
    let half = half_extents(room_m);
    let mut s = src_m;
    for a in 0..3 {
        s[a] = s[a].clamp(-(half[a] - WALL_MARGIN_M), half[a] - WALL_MARGIN_M);
    }
    s
}

/// Mirror `src` (listener-relative metres, listener at the room centre)
/// across each of the six walls of a `room` (full extents, metres).
///
/// Sources outside the room are first clamped just inside the walls (see
/// [`clamp_into_room`]) — the geometry stays valid for any `unit_scale_m`.
pub fn first_order_images(src_m: [f32; 3], room_m: [f32; 3]) -> [[f32; 3]; NUM_REFLECTIONS] {
    let half = half_extents(room_m);
    let s = clamp_into_room(src_m, room_m);
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
#[derive(Clone, Copy)]
struct Tap {
    delay: f32,
    delay_target: f32,
    gain: f32,
    gain_target: f32,
    /// One-pole low-pass state: the wall's absorption plus the air along
    /// the image path, both of which take the treble out of a reflection.
    lp: f32,
    /// Share of its input the low-pass takes in per sample: `1 − a` with
    /// `a = exp(−2π·fc/fs)` (1 = bypass), set per block.
    lp_in: f32,
    /// Where `delay` reads the ring: whole samples behind the write position,
    /// then the weights of that sample and of the one before it. A function
    /// of `delay` alone, kept with it rather than derived on every read: a
    /// delay sits on its target for all but the first samples of a block
    /// whose geometry moved, and splitting it into integer and fraction is
    /// half of what a tap costs.
    read_lo: usize,
    read_w0: f32,
    read_w1: f32,
}

impl Default for Tap {
    /// A silent tap at delay 0, with the read position that goes with it.
    fn default() -> Self {
        Self {
            delay: 0.0,
            delay_target: 0.0,
            gain: 0.0,
            gain_target: 0.0,
            lp: 0.0,
            lp_in: 1.0,
            read_lo: 0,
            read_w0: 1.0,
            read_w1: 0.0,
        }
    }
}

impl Tap {
    #[inline]
    fn step(&mut self, gain_smooth: f32) {
        // A delay on its target stays there: the ramp below would compute a
        // zero difference and assign the target again. Compared as bits, so
        // that a step it skips is one that changes nothing, signed zero
        // included.
        if self.delay.to_bits() != self.delay_target.to_bits() {
            let d = self.delay_target - self.delay;
            if d.abs() <= DELAY_RAMP_RATE {
                self.delay = self.delay_target;
            } else {
                self.delay += DELAY_RAMP_RATE * d.signum();
            }
            self.place_read();
        }
        self.gain += (self.gain_target - self.gain) * gain_smooth;
    }

    /// Derive the read position from `delay`; to be called whenever `delay`
    /// changes. The same split as [`crate::delay_line::read_linear`].
    #[inline]
    fn place_read(&mut self) {
        debug_assert!(self.delay >= 0.0, "negative delay {}", self.delay);
        // Truncation is the floor here: `delay` is non-negative.
        let lo = self.delay as usize;
        let frac = self.delay - lo as f32;
        self.read_lo = lo;
        self.read_w0 = 1.0 - frac;
        self.read_w1 = frac;
    }

    /// Linear-interpolated read `delay` samples behind `write_pos`, which
    /// still points at the sample just written: what
    /// [`crate::delay_line::read_linear`] returns for `delay`, bit for bit.
    #[inline]
    fn read(&self, ring: &[f32], write_pos: usize) -> f32 {
        let cap = ring.len();
        let lo = self.read_lo;
        debug_assert!(lo < cap);
        let idx0 = if write_pos >= lo {
            write_pos - lo
        } else {
            write_pos + cap - lo
        };
        let idx1 = if idx0 == 0 { cap - 1 } else { idx0 - 1 };
        ring[idx0] * self.read_w0 + ring[idx1] * self.read_w1
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
    /// Per-sample gain smoothing coefficient for this rate.
    gain_smooth: f32,
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
            gain_smooth: gain_smooth_for(sample_rate),
        }
    }

    /// Update one reflection's targets: per-ear relative delays (s), per-ear
    /// gains, and the high-frequency cutoff (Hz) of this reflection — the
    /// wall's absorption combined with the air along the image path. Called
    /// once per block per reflection. The two delays differ by the
    /// interaural time difference of the image's direction — the taps are
    /// separate per ear precisely so that a reflection can be lateralised by
    /// time, the cue an ILD pan alone cannot give.
    pub fn set_targets(
        &mut self,
        idx: usize,
        delay_l_s: f32,
        delay_r_s: f32,
        gain_l: f32,
        gain_r: f32,
        cutoff_hz: f32,
    ) {
        let max = (self.ring.len() - 2) as f32;
        let lp_a = lowpass_coeff(cutoff_hz, self.sample_rate);
        for (tap, delay_s, gain) in [
            (&mut self.taps_l[idx], delay_l_s, gain_l),
            (&mut self.taps_r[idx], delay_r_s, gain_r),
        ] {
            let d = (delay_s * self.sample_rate as f32).clamp(0.0, max);
            tap.delay_target = d;
            tap.gain_target = gain;
            tap.lp_in = 1.0 - lp_a;
            // While the tap is (near) silent a delay jump is inaudible — snap
            // instead of sweeping, so a fresh tap doesn't chirp its way from
            // delay 0 to the target while tracking the live signal.
            if tap.gain.abs() < 1e-4 {
                tap.delay = d;
                tap.place_read();
            }
        }
    }

    /// Write one input sample without reading any tap: the write-only
    /// counterpart of [`process`](Self::process), for the blocks during
    /// which reflections are switched off. Keeping the ring current costs a
    /// store per sample and means that switching them back on reads the
    /// last quarter second of what actually played, not whatever was in the
    /// ring when they were switched off.
    #[inline]
    pub fn push(&mut self, input: f32) {
        self.ring[self.write_pos] = input;
        self.write_pos += 1;
        if self.write_pos >= self.ring.len() {
            self.write_pos = 0;
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
        let gain_smooth = self.gain_smooth;
        for i in 0..NUM_REFLECTIONS {
            let tl = &mut self.taps_l[i];
            tl.step(gain_smooth);
            let xl = tl.read(&self.ring, self.write_pos);
            tl.lp += (xl - tl.lp) * tl.lp_in;
            l += tl.gain * tl.lp;
            let tr = &mut self.taps_r[i];
            tr.step(gain_smooth);
            let xr = tr.read(&self.ring, self.write_pos);
            tr.lp += (xr - tr.lp) * tr.lp_in;
            r += tr.gain * tr.lp;
        }

        self.write_pos += 1;
        if self.write_pos >= cap {
            self.write_pos = 0;
        }
        (l, r)
    }
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

    /// Every image of a source is farther from the listener than the
    /// (clamped) source itself, so every relative delay is positive — for a
    /// source well outside the room included, where the raw distance would
    /// exceed the near-wall image's.
    #[test]
    fn images_are_never_closer_than_the_clamped_source() {
        let room = [4.0, 5.0, 2.7];
        let dist = |p: [f32; 3]| (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
        for src in [
            [0.0f32, 1.0, 0.0],
            [3.0, 0.0, 0.0],
            [0.0, 4.0, 0.0],
            [2.0, 3.0, 1.0],
        ] {
            let clamped = clamp_into_room(src, room);
            let d_src = dist(clamped);
            for (i, img) in first_order_images(src, room).iter().enumerate() {
                let d_img = dist(*img);
                // Mirroring pushes one coordinate outward (|2h − s| > |s|
                // for |s| < h), so the image is strictly farther — by 2 ×
                // the margin along the axis for a source against that wall,
                // less in norm when the source is off that axis.
                assert!(
                    d_img > d_src + 1e-4,
                    "src {src:?} image {i}: {d_img} not beyond the source at {d_src}"
                );
            }
            if dist(src) > d_src {
                // The raw distance would have put the near-wall image *ahead*.
                let nearest = first_order_images(src, room)
                    .iter()
                    .map(|img| dist(*img))
                    .fold(f32::MAX, f32::min);
                assert!(
                    nearest < dist(src),
                    "{src:?}: the premise of the test fails"
                );
            }
        }
    }

    /// The floor contains the ADM cube with its margin on every axis and
    /// leaves a generous room alone.
    #[test]
    fn room_floor_contains_the_scene() {
        let default = [4.0, 5.0, 2.7];
        // Unit scale 1: the default room is already the floor on its height
        // (2 × (1 + 0.35) = 2.7) and above it elsewhere — unchanged.
        assert_eq!(room_containing_scene(default, 1.0), default);
        // Unit scale 3: every axis grows to 6.7 m.
        let grown = room_containing_scene(default, 3.0);
        for (a, &g) in grown.iter().enumerate() {
            assert!((g - 6.7).abs() < 1e-5, "axis {a}: {g}");
        }
        // A room the user sized larger keeps its dimensions.
        let big = [12.0, 15.0, 8.0];
        assert_eq!(room_containing_scene(big, 3.0), big);
        // Capped at the largest room the bank models.
        assert_eq!(room_containing_scene(default, 12.0), [MAX_ROOM_M; 3]);
        // Every ADM-cube corner sits inside the grown room by the margin.
        let s = 2.5;
        let room = room_containing_scene(default, s);
        for corner in [[s, s, s], [-s, s, -s], [s, -s, s]] {
            let clamped = clamp_into_room(corner, room);
            assert_eq!(clamped, corner, "corner {corner:?} was clamped");
        }
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
        bank.set_targets(
            0,
            10.0 / 48_000.0,
            10.0 / 48_000.0,
            0.5,
            0.0,
            MAX_WALL_CUTOFF_HZ,
        );
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

    /// The gain fade is a time constant, not a sample count: the tap
    /// reaches the same fraction of its target after the same milliseconds
    /// at 48 and 96 kHz.
    #[test]
    fn gain_smoothing_is_rate_invariant() {
        let settle_after_ms = |sample_rate: u32| -> f32 {
            let mut bank = ReflectionBank::new(sample_rate);
            bank.set_targets(0, 0.0, 0.0, 1.0, 1.0, MAX_WALL_CUTOFF_HZ);
            let n = (sample_rate as f32 * 0.003) as usize; // 3 ms
            let mut last = 0.0;
            for _ in 0..n {
                last = bank.process(1.0).0;
            }
            last
        };
        let (a, b) = (settle_after_ms(48_000), settle_after_ms(96_000));
        assert!((a - b).abs() < 0.01, "48 kHz {a} vs 96 kHz {b} after 3 ms");
        assert!(a > 0.8 && a < 0.95, "unexpected settling {a}");
        // `1 − 0.985` in f32 lands one ulp under the literal 0.015: the
        // 48 kHz coefficient is the published one to 1e-8.
        assert!((gain_smooth_for(48_000) - GAIN_SMOOTH_48K).abs() < 1e-6);
    }

    /// The two ears of one reflection read the ring at their own delays: an
    /// impulse comes out of the left tap at its delay and of the right tap
    /// at the other — the interaural difference of the reflection.
    #[test]
    fn ears_read_at_their_own_delays() {
        let mut bank = ReflectionBank::new(48_000);
        bank.set_targets(
            0,
            10.0 / 48_000.0,
            24.0 / 48_000.0,
            0.5,
            0.5,
            MAX_WALL_CUTOFF_HZ,
        );
        for _ in 0..4_000 {
            bank.process(0.0);
        }
        let mut outs = Vec::new();
        outs.push(bank.process(1.0));
        for _ in 0..30 {
            outs.push(bank.process(0.0));
        }
        assert!(
            (outs[10].0 - 0.5).abs() < 1e-3,
            "left at 10: {}",
            outs[10].0
        );
        assert!(
            (outs[24].1 - 0.5).abs() < 1e-3,
            "right at 24: {}",
            outs[24].1
        );
        assert!(
            outs[10].1.abs() < 1e-3 && outs[24].0.abs() < 1e-3,
            "ears bled"
        );
    }

    /// A tap's low-pass takes the treble out of its reflection and leaves
    /// the bass: at the maximum cutoff it is bit-transparent.
    #[test]
    fn taps_absorb_treble_at_the_wall_cutoff() {
        assert_eq!(lowpass_coeff(MAX_WALL_CUTOFF_HZ, 48_000), 0.0);
        let energy_at = |cutoff: f32, period: usize| -> f32 {
            let mut bank = ReflectionBank::new(48_000);
            bank.set_targets(0, 0.0, 0.0, 1.0, 0.0, cutoff);
            let mut e = 0.0f32;
            for n in 0..4_000 {
                // Square wave of the given period; measure after settling.
                let x = if (n / period) % 2 == 0 { 1.0 } else { -1.0 };
                let (l, _) = bank.process(x);
                if n >= 2_000 {
                    e += l * l;
                }
            }
            e
        };
        // Nyquist-rate alternation: crushed by a 2 kHz wall, untouched at max.
        let bright = energy_at(MAX_WALL_CUTOFF_HZ, 1);
        let dull = energy_at(2_000.0, 1);
        assert!(
            dull < 0.1 * bright,
            "treble not absorbed: {dull} vs {bright}"
        );
        // 100 Hz square (period 240 samples): the bass gets through either way.
        let bass_bright = energy_at(MAX_WALL_CUTOFF_HZ, 240);
        let bass_dull = energy_at(2_000.0, 240);
        assert!(
            bass_dull > 0.8 * bass_bright,
            "bass lost: {bass_dull} vs {bass_bright}"
        );
    }

    #[test]
    fn gain_changes_are_smoothed() {
        let mut bank = ReflectionBank::new(48_000);
        bank.set_targets(0, 0.0, 0.0, 1.0, 1.0, MAX_WALL_CUTOFF_HZ);
        for _ in 0..4_000 {
            bank.process(1.0); // settle: DC input, gain 1
        }
        let (settled, _) = bank.process(1.0);
        assert!((settled - 1.0).abs() < 1e-2);
        // Drop the gain target to 0: output must move gradually, not jump.
        bank.set_targets(0, 0.0, 0.0, 0.0, 0.0, MAX_WALL_CUTOFF_HZ);
        let (next, _) = bank.process(1.0);
        assert!(next > 0.9, "gain jumped instead of smoothing: {next}");
    }

    /// A tap as the bank defines it, with nothing kept between samples but
    /// its state: the delay ramp runs on every sample and the read goes
    /// through [`read_linear`](crate::delay_line::read_linear), which splits
    /// the delay each time. The reference [`ReflectionBank::process`] has to
    /// match bit for bit.
    #[derive(Clone, Copy, Default)]
    struct ReferenceTap {
        delay: f32,
        delay_target: f32,
        gain: f32,
        gain_target: f32,
        lp: f32,
        lp_a: f32,
    }

    struct ReferenceBank {
        ring: Vec<f32>,
        write_pos: usize,
        taps_l: [ReferenceTap; NUM_REFLECTIONS],
        taps_r: [ReferenceTap; NUM_REFLECTIONS],
        sample_rate: u32,
    }

    impl ReferenceBank {
        fn new(sample_rate: u32) -> Self {
            Self {
                ring: vec![0.0; ReflectionBank::new(sample_rate).ring.len()],
                write_pos: 0,
                taps_l: Default::default(),
                taps_r: Default::default(),
                sample_rate,
            }
        }

        fn set_targets(&mut self, idx: usize, delays_s: [f32; 2], gains: [f32; 2], cutoff_hz: f32) {
            let max = (self.ring.len() - 2) as f32;
            let lp_a = lowpass_coeff(cutoff_hz, self.sample_rate);
            for (tap, delay_s, gain) in [
                (&mut self.taps_l[idx], delays_s[0], gains[0]),
                (&mut self.taps_r[idx], delays_s[1], gains[1]),
            ] {
                let d = (delay_s * self.sample_rate as f32).clamp(0.0, max);
                tap.delay_target = d;
                tap.gain_target = gain;
                tap.lp_a = lp_a;
                if tap.gain.abs() < 1e-4 {
                    tap.delay = d;
                }
            }
        }

        fn process(&mut self, input: f32) -> (f32, f32) {
            use crate::delay_line::read_linear;
            let gain_smooth = gain_smooth_for(self.sample_rate);
            self.ring[self.write_pos] = input;
            let mut out = [0.0f32; 2];
            for i in 0..NUM_REFLECTIONS {
                for (tap, acc) in [&mut self.taps_l[i], &mut self.taps_r[i]]
                    .into_iter()
                    .zip(&mut out)
                {
                    let d = tap.delay_target - tap.delay;
                    if d.abs() <= DELAY_RAMP_RATE {
                        tap.delay = tap.delay_target;
                    } else {
                        tap.delay += DELAY_RAMP_RATE * d.signum();
                    }
                    tap.gain += (tap.gain_target - tap.gain) * gain_smooth;
                    let x = read_linear(&self.ring, self.write_pos, tap.delay);
                    tap.lp += (x - tap.lp) * (1.0 - tap.lp_a);
                    *acc += tap.gain * tap.lp;
                }
            }
            self.write_pos += 1;
            if self.write_pos >= self.ring.len() {
                self.write_pos = 0;
            }
            (out[0], out[1])
        }
    }

    /// The read position kept with each tap gives the bits of a tap that
    /// splits its delay on every read, through everything a delay does:
    /// standing still, snapping to a target less than a sample away (a slowly
    /// moving source, every block), ramping up and down over many samples (a
    /// jump), snapping while the tap is silent, sitting at zero and at the
    /// end of the ring — over several laps of the ring, so every read wraps.
    #[test]
    fn kept_read_position_matches_a_tap_that_splits_its_delay_every_sample() {
        for sample_rate in [8_000u32, 48_000, 96_000] {
            let mut bank = ReflectionBank::new(sample_rate);
            let mut reference = ReferenceBank::new(sample_rate);
            let ring_s = (bank.ring.len() - 2) as f32 / sample_rate as f32;
            let mut state = 0x0bad_5eedu32;
            let mut noise = move || {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                state as f32 / u32::MAX as f32 - 0.5
            };
            const BLOCK: usize = 40;
            let blocks = 3 * bank.ring.len() / BLOCK + 50;
            let mut ramping = 0usize;
            for b in 0..blocks {
                for i in 0..NUM_REFLECTIONS {
                    let t = b as f32 * 0.01 + i as f32;
                    // Seconds: a slow drift around a few milliseconds…
                    let mut delay = 0.004 + 0.003 * i as f32 + 0.0002 * t.sin();
                    let mut gains = [0.3 + 0.1 * t.cos(), 0.3 - 0.1 * t.cos()];
                    let mut cutoff = MAX_WALL_CUTOFF_HZ;
                    match (i, (b / 60) % 4) {
                        // …a tap that jumps far and back, ramping both ways…
                        (1, 1) => delay += 0.02,
                        (1, 3) => delay += 0.001,
                        // …one that goes silent, moves and comes back…
                        (2, 1) => gains = [0.0, 0.0],
                        (2, 2) => delay += 0.03,
                        // …one at delay zero, one pinned to the end of the
                        // ring (the clamp), one with its low-pass engaged.
                        (3, _) => delay = 0.0,
                        (4, 2) => delay = ring_s + 1.0,
                        (5, _) => cutoff = 2_000.0 + 500.0 * i as f32,
                        _ => {}
                    }
                    let itd = 0.0003 * (t * 0.7).sin();
                    let delays = [delay + itd.max(0.0), delay + (-itd).max(0.0)];
                    bank.set_targets(i, delays[0], delays[1], gains[0], gains[1], cutoff);
                    reference.set_targets(i, delays, gains, cutoff);
                }
                for s in 0..BLOCK {
                    let x = noise();
                    let got = bank.process(x);
                    let want = reference.process(x);
                    assert_eq!(
                        (got.0.to_bits(), got.1.to_bits()),
                        (want.0.to_bits(), want.1.to_bits()),
                        "{sample_rate} Hz, block {b}, sample {s}: {got:?} against {want:?}"
                    );
                    ramping += usize::from(
                        bank.taps_l
                            .iter()
                            .any(|t| t.delay.to_bits() != t.delay_target.to_bits()),
                    );
                }
            }
            // The run has to spend time both ramping and settled.
            let total = blocks * BLOCK;
            assert!(
                ramping > total / 20 && total - ramping > total / 20,
                "{sample_rate} Hz: {ramping} ramping samples out of {total}"
            );
            for (tap, want) in bank
                .taps_l
                .iter()
                .chain(&bank.taps_r)
                .zip(reference.taps_l.iter().chain(&reference.taps_r))
            {
                assert_eq!(
                    [tap.delay.to_bits(), tap.gain.to_bits(), tap.lp.to_bits()],
                    [want.delay.to_bits(), want.gain.to_bits(), want.lp.to_bits()]
                );
            }
        }
    }
}
