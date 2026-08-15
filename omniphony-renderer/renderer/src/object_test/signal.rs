//! The waveforms an object test can be made of.
//!
//! Continuous pink noise is the right default — it excites everything and makes
//! colouration obvious — but it is one of the *weaker* stimuli for judging where
//! a source is. The ear localises **onsets**, and a steady stream offers none,
//! so the image sits in a fuzzy region and a positional error of a few degrees
//! hides inside it. Each signal here exists because it exposes something that
//! one cannot:
//!
//! - **Bursts** restore the onsets. Every burst is a fresh "where is it?"
//!   judgement, so the image collapses to a point; on an orbit they also sample
//!   the trajectory, which is what makes per-block position stepping audible.
//! - **Low** (below ~1.5 kHz) carries interaural *time* differences and almost
//!   nothing else; **high** (above ~3 kHz) carries level and spectral cues. Run
//!   separately they say *which* cue is broken — blind on a broadband signal.
//! - **Band** is the ~8 kHz directional band, where elevation lives. A
//!   broadband source hides a flat height response; this one does not.
//! - **Tone** is a poor localiser and an excellent level meter for the ear: any
//!   ripple as the source moves is immediately audible, which is what catches
//!   panning artefacts, hull crossings and out-of-hull behaviour.
//! - **Clicks** expose comb filtering between speakers summing a phantom, and
//!   any pre-echo. Unpleasant, deliberately.
//! - **Clip** plays a file, because at some point the question stops being
//!   "does the geometry work" and becomes "does music land where it should".
//!
//! All of them are scaled to roughly the same loudness for a given level
//! setting, so switching signals mid-listen changes what you are judging and
//! not how loud it is.

use crate::crossover::filter::{BiquadState, biquad, butterworth2_hp, butterworth2_lp};
use crate::live_params::ObjectTestSignal;
use crate::speaker_test::PinkNoise;

use super::clip::ObjectTestClip;

/// Burst length and repetition, in seconds. 30 ms is long enough to carry a
/// direction and short enough to be a single event; four a second leaves room
/// for the ear to settle between them without becoming a slideshow.
const BURST_ON_S: f32 = 0.030;
const BURST_PERIOD_S: f32 = 0.250;
/// Raised-cosine edge on each burst. A rectangular gate would splatter a click
/// across the spectrum at both ends, and a click is a different test.
const BURST_EDGE_S: f32 = 0.005;

/// Tone frequency. Low enough to be a clean level meter for the ear, high
/// enough not to disappear on small speakers.
const TONE_HZ: f32 = 500.0;
/// Click repetition. Same rate as the bursts, for comparability.
const CLICK_PERIOD_S: f32 = 0.250;

/// Band edges. The split is at the classic ITD/ILD crossover: below ~1.5 kHz
/// the head is small against the wavelength and phase carries the direction;
/// above ~3 kHz it shadows and level does. The gap between them is deliberate —
/// the region where both cues operate belongs to neither test.
const LOW_HZ: f32 = 1500.0;
const HIGH_HZ: f32 = 3000.0;
/// Directional band: a third-octave around 8 kHz, expressed as the LP/HP pair
/// that brackets it.
const BAND_LO_HZ: f32 = 7100.0;
const BAND_HI_HZ: f32 = 9000.0;

/// Makeup applied to the filtered variants so they land near the same loudness
/// as the unfiltered noise. Pink noise carries equal power per octave, so a
/// band keeps roughly `octaves_in_band / octaves_total` of the power — a third
/// octave at 8 kHz keeps very little of it. Calibrated by measurement and
/// pinned by `every_signal_lands_near_the_same_loudness`.
const MAKEUP_LOW: f32 = 1.228;
const MAKEUP_HIGH: f32 = 2.207;
const MAKEUP_BAND: f32 = 9.755;

/// What the level control is divided by, per signal.
///
/// The level is a *peak* figure — it exists to answer "will this clip", and the
/// bound is what lets the panned result be clamped safely. Pink noise's crest
/// factor turns that peak into a generator gain.
///
/// Every unit-RMS generator uses the *same* divisor, including the tone, whose
/// own crest is only √2. Dividing each signal by its own crest would put them
/// all on the same ceiling and therefore at wildly different loudnesses: a live
/// sweep measured the 500 Hz tone landing 10 dB above the noise at the same
/// setting, which is a nasty surprise in headphones when the control that
/// changed was labelled "signal". Sharing pink noise's divisor means a
/// lower-crest signal simply sits further below the ceiling — quieter than it
/// could be, and exactly as loud as the reference.
///
/// The two peak-normalised signals are the exception: an impulse train and a
/// clip *are* their own peak, and an impulse held 13 dB below the ceiling for
/// the sake of an RMS match would be pointless — a click's loudness comes from
/// its height, and its average level is meaningless either way.
pub fn level_divisor_of(signal: ObjectTestSignal) -> f32 {
    match signal {
        ObjectTestSignal::PinkNoise
        | ObjectTestSignal::PinkBursts
        | ObjectTestSignal::PinkLow
        | ObjectTestSignal::PinkHigh
        | ObjectTestSignal::PinkBand
        | ObjectTestSignal::Tone => PinkNoise::CREST,
        ObjectTestSignal::Clicks | ObjectTestSignal::Clip => 1.0,
    }
}

/// Generator state for every signal, kept in one place so switching between
/// them is a reset rather than a swap of objects.
pub struct SignalGen {
    noise: PinkNoise,
    /// Two cascaded Butterworth sections per branch: 24 dB/octave, enough to
    /// keep the "low" signal genuinely free of the level cues the "high" one is
    /// there to test.
    filt: [BiquadState; 4],
    sample_rate: u32,
    /// Sample counter within the burst/click period.
    tick: u32,
    /// Tone phase in radians, wrapped every cycle.
    phase: f32,
    /// Playback cursor into the clip, in samples.
    clip_pos: usize,
}

impl SignalGen {
    pub fn new(seed: u32, sample_rate: u32) -> Self {
        Self {
            noise: PinkNoise::new(seed),
            filt: Default::default(),
            sample_rate: sample_rate.max(1),
            tick: 0,
            phase: 0.0,
            clip_pos: 0,
        }
    }

    /// Start again from the top: new run of the test.
    pub fn reset(&mut self, sample_rate: u32) {
        self.noise.reset();
        self.filt = Default::default();
        self.sample_rate = sample_rate.max(1);
        self.tick = 0;
        self.phase = 0.0;
        self.clip_pos = 0;
    }

    /// One sample, nominally unit-RMS; the caller scales by
    /// [`level_divisor_of`] and clamps.
    ///
    /// `clip` is only read by [`ObjectTestSignal::Clip`]; every other signal
    /// ignores it, and a `Clip` with nothing loaded is silent rather than an
    /// error — the file is chosen in one message and the test armed in another,
    /// so the two orders both have to work.
    pub fn next_sample(&mut self, signal: ObjectTestSignal, clip: Option<&ObjectTestClip>) -> f32 {
        let rate = self.sample_rate as f32;
        match signal {
            ObjectTestSignal::PinkNoise => self.noise.next_sample(),
            ObjectTestSignal::PinkBursts => {
                let period = (BURST_PERIOD_S * rate) as u32;
                let on = (BURST_ON_S * rate) as u32;
                let edge = (BURST_EDGE_S * rate).max(1.0);
                let t = self.tick;
                self.tick += 1;
                if self.tick >= period.max(1) {
                    self.tick = 0;
                }
                // The generator runs continuously through the gaps: restarting
                // its poles every burst would put a transient of the filter's
                // own making on top of the one being tested.
                let raw = self.noise.next_sample();
                if t >= on {
                    return 0.0;
                }
                let into = t as f32;
                let out_of = (on - t) as f32;
                let env = (into / edge).min(out_of / edge).clamp(0.0, 1.0);
                // Raised cosine, so the edge has no corner to splatter.
                let env = 0.5 - 0.5 * (std::f32::consts::PI * env).cos();
                raw * env
            }
            ObjectTestSignal::PinkLow => {
                let raw = self.noise.next_sample();
                let c = butterworth2_lp(LOW_HZ, self.sample_rate);
                let y = biquad(raw, c, &mut self.filt[0]);
                biquad(y, c, &mut self.filt[1]) * MAKEUP_LOW
            }
            ObjectTestSignal::PinkHigh => {
                let raw = self.noise.next_sample();
                let c = butterworth2_hp(HIGH_HZ, self.sample_rate);
                let y = biquad(raw, c, &mut self.filt[0]);
                biquad(y, c, &mut self.filt[1]) * MAKEUP_HIGH
            }
            ObjectTestSignal::PinkBand => {
                let raw = self.noise.next_sample();
                let hp = butterworth2_hp(BAND_LO_HZ, self.sample_rate);
                let lp = butterworth2_lp(BAND_HI_HZ, self.sample_rate);
                let y = biquad(raw, hp, &mut self.filt[0]);
                let y = biquad(y, hp, &mut self.filt[1]);
                let y = biquad(y, lp, &mut self.filt[2]);
                biquad(y, lp, &mut self.filt[3]) * MAKEUP_BAND
            }
            ObjectTestSignal::Tone => {
                let s = self.phase.sin() * std::f32::consts::SQRT_2;
                self.phase += std::f32::consts::TAU * TONE_HZ / rate;
                if self.phase >= std::f32::consts::TAU {
                    self.phase -= std::f32::consts::TAU;
                }
                s
            }
            ObjectTestSignal::Clicks => {
                let period = (CLICK_PERIOD_S * rate) as u32;
                let t = self.tick;
                self.tick += 1;
                if self.tick >= period.max(1) {
                    self.tick = 0;
                }
                // A single unit sample. Nothing band-limits it on purpose: the
                // point is to hand the renderer the widest possible spectrum in
                // the shortest possible time and hear what comes back.
                if t == 0 { 1.0 } else { 0.0 }
            }
            ObjectTestSignal::Clip => {
                let Some(clip) = clip else { return 0.0 };
                if clip.samples.is_empty() {
                    return 0.0;
                }
                if self.clip_pos >= clip.samples.len() {
                    self.clip_pos = 0;
                }
                let s = clip.samples[self.clip_pos];
                self.clip_pos += 1;
                s
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(signal: ObjectTestSignal, n: usize) -> Vec<f32> {
        let mut g = SignalGen::new(0x85EB_CA6B, 48_000);
        (0..n).map(|_| g.next_sample(signal, None)).collect()
    }

    fn rms(v: &[f32]) -> f32 {
        (v.iter().map(|s| (*s as f64) * (*s as f64)).sum::<f64>() / v.len() as f64).sqrt() as f32
    }

    /// The level control is one number across every signal, so the signals have
    /// to agree on what it means. Without the makeup gains a third-octave band
    /// keeps a thirtieth of pink noise's power and switching to it sounds like
    /// the test stopped.
    ///
    /// Judged on the continuous signals at their working level, and **after the
    /// level scaling** — that is what reaches the ear, and it is where the first
    /// version of this got it wrong: the generators matched, the divisors did
    /// not, and the tone came out 10 dB hot on a live render. The sparse signals
    /// (bursts, clicks) are meant to average lower; what matters there is that
    /// the event itself matches.
    #[test]
    fn every_signal_lands_near_the_same_loudness() {
        let level = 0.5f32;
        let scaled = |signal| {
            let g = level / level_divisor_of(signal);
            rms(&run(signal, 480_000)) * g
        };
        let reference = scaled(ObjectTestSignal::PinkNoise);
        for signal in [
            ObjectTestSignal::PinkLow,
            ObjectTestSignal::PinkHigh,
            ObjectTestSignal::PinkBand,
            ObjectTestSignal::Tone,
        ] {
            let got = scaled(signal);
            let db = 20.0 * (got / reference).log10();
            assert!(
                db.abs() < 2.0,
                "{signal:?} is {db:.1} dB from pink noise — one level control \
                 cannot mean two things"
            );
        }
    }

    /// Every signal must respect the peak bound its crest claims, because that
    /// bound is what stops the panned result clipping.
    #[test]
    fn no_signal_exceeds_the_peak_its_crest_promises() {
        for signal in [
            ObjectTestSignal::PinkNoise,
            ObjectTestSignal::PinkBursts,
            ObjectTestSignal::PinkLow,
            ObjectTestSignal::PinkHigh,
            ObjectTestSignal::PinkBand,
            ObjectTestSignal::Tone,
            ObjectTestSignal::Clicks,
        ] {
            let level = 1.0f32;
            let gain = level / level_divisor_of(signal);
            let peak = run(signal, 480_000)
                .iter()
                .map(|s| (s * gain).abs())
                .fold(0.0f32, f32::max);
            // Pink noise is Gaussian-ish, so its peak has no bound and the
            // caller clamps; the deterministic signals must not need to.
            let bound = match signal {
                ObjectTestSignal::Tone | ObjectTestSignal::Clicks => 1.001,
                _ => 1.4,
            };
            assert!(
                peak <= bound,
                "{signal:?} peaked at {peak} for a level of {level}"
            );
        }
    }

    /// Bursts must actually be bursts: sound, then silence, on the stated clock.
    #[test]
    fn bursts_are_gated_on_the_stated_clock() {
        let v = run(ObjectTestSignal::PinkBursts, 48_000);
        let on = (BURST_ON_S * 48_000.0) as usize;
        let period = (BURST_PERIOD_S * 48_000.0) as usize;
        // Middle of the first burst: sound. Middle of the first gap: silence.
        assert!(rms(&v[on / 4..on * 3 / 4]) > 0.1, "the burst was silent");
        assert_eq!(
            v[on + 10..period].iter().filter(|s| **s != 0.0).count(),
            0,
            "the gap between bursts was not silent"
        );
        // And it repeats.
        assert!(
            rms(&v[period + on / 4..period + on * 3 / 4]) > 0.1,
            "the second burst never came"
        );
    }

    /// The two band-limited signals must genuinely be band-limited, or they
    /// cannot separate the cue they exist to separate. Asserted by measuring
    /// how much of each lands on the other's side of the split.
    #[test]
    fn the_band_limited_signals_reject_each_others_range() {
        // A crude one-pole probe is enough to tell 24 dB/oct rejection from none.
        let energy_below = |v: &[f32], fc: f32| {
            let a = (-std::f32::consts::TAU * fc / 48_000.0).exp();
            let mut z = 0.0f32;
            let mut sum = 0.0f64;
            for s in v {
                z += (s - z) * (1.0 - a);
                sum += (z as f64) * (z as f64);
            }
            (sum / v.len() as f64).sqrt() as f32
        };
        let low = run(ObjectTestSignal::PinkLow, 240_000);
        let high = run(ObjectTestSignal::PinkHigh, 240_000);
        // Below 500 Hz the low signal should dominate by a wide margin.
        let l = energy_below(&low, 500.0);
        let h = energy_below(&high, 500.0);
        assert!(
            l > h * 4.0,
            "the high signal still carries low-frequency energy (low={l}, high={h}) \
             — it cannot isolate the level cue"
        );
    }
}
