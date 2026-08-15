//! Per-speaker test signal: band-limited pink noise, for identifying a speaker
//! and checking its crossover assignment by ear.
//!
//! Pink noise rather than a tone: a tone at one frequency tells you almost
//! nothing about a band-limited speaker (it either falls in the band or it does
//! not), while pink noise excites the whole pass-band at a perceptually flat
//! level, so a sub, a mid and a tweeter each sound like a recognisable share of
//! the same signal.
//!
//! The generator is allocation-free and runs in the render path; the state lives
//! on the renderer and is reused every block.

/// Pink noise: white through a three-pole pinking filter (Paul Kellett's
/// "economy" design).
///
/// Chosen over the Voss-McCartney row generator, which was tried first and
/// measured essentially flat here: its rows are each white below their own
/// update rate, so across the octaves that matter most they all still
/// contribute flat power and the slope only appears at the extremes. This
/// filter holds -3 dB/octave from a few Hz to beyond 20 kHz with three state
/// variables and three multiply-adds per sample.
///
/// Output is normalised to unit RMS. Measured, not derived: the raw filter sums
/// to ~1.717 RMS for white in [-1, 1).
///
/// Unit RMS is the generator's internal normalisation, *not* the level contract
/// a caller gets — see [`PinkNoise::CREST`] for what a test level means.
const PINK_RAW_RMS: f32 = 1.717_1;

#[derive(Debug, Clone)]
pub struct PinkNoise {
    /// Pole states, slowest first.
    poles: [f32; 3],
    rng: u32,
}

impl Default for PinkNoise {
    fn default() -> Self {
        Self::new(0x9E3779B9)
    }
}

impl PinkNoise {
    /// Nominal peak-to-RMS ratio, used to turn a *peak*-referenced level into a
    /// generator gain: a caller wanting peak `L` scales by `L / CREST`.
    ///
    /// A test level is peak-referenced rather than RMS-referenced because the
    /// number is there to answer "will this clip", and only a peak figure
    /// answers that. Applying an RMS level directly to this unit-RMS generator
    /// is what made -6 dBFS render peaks near +6 dBFS.
    ///
    /// 4.5 is a *nominal* crest, not a bound — pink noise is Gaussian-ish, so
    /// its peak has no ceiling and grows with run length. Measured over five
    /// seeds, the worst peak was 4.10 after 1 s of 48 kHz, 4.91 after 100 s and
    /// 5.96 after 2.8 h. That is precisely why the caller must also clamp:
    /// scaling by CREST puts the *typical* peak on target, and the clamp makes
    /// the ceiling exact.
    ///
    /// 4.5 is the knee of that trade-off. Measured over 480 M samples, |s|
    /// exceeds it once per 768 k samples — a lone sample every ~16 s at 48 kHz,
    /// inaudible — while a tighter 3.5 would engage 12×/s and audibly dull the
    /// noise, and a looser 5.5 would cost 2 dB of level for nothing.
    pub const CREST: f32 = 4.5;

    pub fn new(seed: u32) -> Self {
        Self {
            poles: [0.0; 3],
            // A zero state would latch xorshift at zero forever.
            rng: if seed == 0 { 0x9E3779B9 } else { seed },
        }
    }

    /// Uniform white noise in [-1, 1). xorshift32: no allocation, no dependency,
    /// and its quality is far beyond what a listening test can resolve.
    #[inline]
    fn white(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        // Map to [-1, 1) through the top 24 bits: the low bits of xorshift are
        // the weakest, and f32 carries 24 bits of mantissa anyway.
        ((self.rng >> 8) as f32) * (2.0 / 16_777_216.0) - 1.0
    }

    /// Next pink sample, roughly unit RMS.
    ///
    /// Each pole is a one-pole lowpass on the same white source, with time
    /// constants an octave-and-a-bit apart; their sum plus a little direct white
    /// traces the -3 dB/octave line across the audible range.
    #[inline]
    pub fn next_sample(&mut self) -> f32 {
        let white = self.white();
        self.poles[0] = 0.997_7 * self.poles[0] + white * 0.099_046;
        self.poles[1] = 0.963_0 * self.poles[1] + white * 0.296_516;
        self.poles[2] = 0.570_0 * self.poles[2] + white * 1.052_691;
        (self.poles[0] + self.poles[1] + self.poles[2] + white * 0.184_8) / PINK_RAW_RMS
    }

    /// Drop the filter state so a restarted test does not begin with the tail of
    /// the previous one.
    pub fn reset(&mut self) {
        self.poles = [0.0; 3];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pink noise carries EQUAL energy per octave — that is its definition, and
    /// what makes it sound balanced. Summing bin power across two adjacent
    /// octaves must therefore give a ratio near 1.0.
    ///
    /// Worth stating because the intuition misleads: the -3 dB/octave figure
    /// describes power *per bin*, and the upper octave holds twice as many bins,
    /// so the two effects cancel. White noise, by the same measure, gives 2.0 —
    /// asserted below so this test cannot pass on a flat spectrum.
    #[test]
    fn pink_noise_falls_about_three_db_per_octave() {
        let sample_rate = 48_000.0_f32;
        let n = 1 << 16;
        let mut noise = PinkNoise::default();
        let samples: Vec<f32> = (0..n).map(|_| noise.next_sample()).collect();

        // Goertzel-style band energy: sum |X(f)|^2 over the bins of an octave.
        let band_energy = |lo: f32, hi: f32| -> f64 {
            let lo_bin = (lo / sample_rate * n as f32).round() as usize;
            let hi_bin = (hi / sample_rate * n as f32).round() as usize;
            let mut total = 0.0f64;
            for k in lo_bin..hi_bin.max(lo_bin + 1) {
                let (mut re, mut im) = (0.0f64, 0.0f64);
                let w = 2.0 * std::f64::consts::PI * k as f64 / n as f64;
                for (i, &s) in samples.iter().enumerate() {
                    re += s as f64 * (w * i as f64).cos();
                    im -= s as f64 * (w * i as f64).sin();
                }
                total += re * re + im * im;
            }
            total
        };

        // Two octaves well inside the audible range, away from DC and Nyquist.
        let low = band_energy(500.0, 1000.0);
        let high = band_energy(1000.0, 2000.0);
        let ratio = high / low;
        assert!(
            (0.75..1.35).contains(&ratio),
            "pink noise must carry about equal energy per octave, got {ratio:.3}"
        );

        // Contrast: the same measurement on white must land near 2.0, so a
        // regression that flattens the spectrum cannot slip through.
        let mut w = PinkNoise::default();
        let white: Vec<f32> = (0..n).map(|_| w.white()).collect();
        let white_energy = |lo: f32, hi: f32| -> f64 {
            let lo_bin = (lo / sample_rate * n as f32).round() as usize;
            let hi_bin = (hi / sample_rate * n as f32).round() as usize;
            let mut total = 0.0f64;
            for k in lo_bin..hi_bin.max(lo_bin + 1) {
                let (mut re, mut im) = (0.0f64, 0.0f64);
                let ang = 2.0 * std::f64::consts::PI * k as f64 / n as f64;
                for (i, &s) in white.iter().enumerate() {
                    re += s as f64 * (ang * i as f64).cos();
                    im -= s as f64 * (ang * i as f64).sin();
                }
                total += re * re + im * im;
            }
            total
        };
        let white_ratio = white_energy(1000.0, 2000.0) / white_energy(500.0, 1000.0);
        assert!(
            white_ratio > 1.6,
            "the measurement must separate pink from white; white gave {white_ratio:.3}"
        );
    }

    /// The generator feeds a speaker, so it must never produce a non-finite
    /// sample or drift out of range no matter how long it runs.
    #[test]
    fn pink_noise_stays_finite_and_bounded() {
        let mut noise = PinkNoise::default();
        let mut peak = 0.0f32;
        let mut sum_sq = 0.0f64;
        let n = 1_000_000;
        for _ in 0..n {
            let s = noise.next_sample();
            assert!(s.is_finite(), "pink noise produced a non-finite sample");
            peak = peak.max(s.abs());
            sum_sq += (s as f64) * (s as f64);
        }
        // Unit RMS is what makes the level control mean dBFS, so pin it.
        let rms = (sum_sq / n as f64).sqrt();
        assert!(
            (0.9..1.1).contains(&rms),
            "pink noise must be normalised to unit RMS for the dBFS level to \
             mean anything, got {rms:.3}"
        );
        // The peak of a Gaussian-ish signal grows with run length and has no
        // ceiling, so this only catches a runaway state, not the crest — see
        // `crest_is_exceeded_rarely_enough_to_clamp_inaudibly` for that.
        assert!(
            peak < 8.0,
            "pink noise peak {peak} is implausible for a unit-RMS signal"
        );
    }

    /// `CREST` is a nominal peak, and the caller clamps to it — so what makes
    /// the clamp inaudible is how *rarely* it engages. Pin that rate.
    ///
    /// This is the assumption the whole level contract rests on: scaling by
    /// CREST puts the typical peak on target and the clamp makes the ceiling
    /// exact, which is only honest if the clamp catches a lone sample now and
    /// then rather than shaving the signal continuously. Measured over 480 M
    /// samples the rate is 1.3e-6 (a sample every ~16 s at 48 kHz); the bound
    /// here is two orders looser so a short run cannot fail on noise, while a
    /// regression that lowered CREST into limiting territory still trips it.
    #[test]
    fn crest_is_exceeded_rarely_enough_to_clamp_inaudibly() {
        let mut noise = PinkNoise::default();
        let n = 10_000_000;
        let mut over = 0u64;
        for _ in 0..n {
            if noise.next_sample().abs() > PinkNoise::CREST {
                over += 1;
            }
        }
        let rate = over as f64 / n as f64;
        assert!(
            rate < 1e-4,
            "the clamp at CREST={} engages on {rate:.2e} of samples — that is \
             limiting, not peak-bounding",
            PinkNoise::CREST
        );
    }

    /// A restart must not carry the previous run's filter state.
    #[test]
    fn reset_clears_the_filter_state() {
        let mut noise = PinkNoise::default();
        for _ in 0..10_000 {
            noise.next_sample();
        }
        noise.reset();
        let fresh = PinkNoise::default();
        assert_eq!(noise.poles, fresh.poles);
    }
}
