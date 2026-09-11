//! The pure detectors the auto-tune procedure is decided by
//! (`auto-tune/detectors.js`).
//!
//! Each one reads a window of telemetry and returns a verdict. Nothing here
//! touches the UI, the renderer or the clock, which is what makes the whole
//! procedure testable from a table of samples.

/// Which regime the renderer's resampler reported being in. Only the recovery
/// state is named: it is the one that says the source went away, and the rest
/// are alike for every purpose here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Other,
    LowRecover,
}

/// One telemetry sample.
#[derive(Clone, Copy, Debug, Default)]
pub struct Sample {
    /// Milliseconds since the run started.
    pub t: f64,
    pub latency_smoothed_ms: Option<f64>,
    pub latency_target_ms: Option<f64>,
    pub resample_ratio: Option<f64>,
    pub phase: Phase,
}

/// `TUNE_THRESHOLDS.oscillation`.
#[derive(Clone, Copy, Debug)]
pub struct Oscillation {
    /// The first stretch of a palier is the response to the patch that started
    /// it, not the regime it is measuring.
    pub palier_warmup_ms: f64,
    /// Dead band around the mean, so noise does not count as crossings.
    pub hysteresis_ppm: f64,
    pub min_crossings_absolute: u32,
    pub min_absolute_peak_to_peak_ppm: f64,
    pub peak_to_peak_jump_ratio: f64,
    pub crossing_jump_ratio: f64,
    pub baseline_paliers: usize,
    pub min_baseline_paliers: usize,
}

impl Default for Oscillation {
    fn default() -> Self {
        Self {
            palier_warmup_ms: 10_000.0,
            hysteresis_ppm: 200.0,
            min_crossings_absolute: 4,
            min_absolute_peak_to_peak_ppm: 1500.0,
            peak_to_peak_jump_ratio: 3.0,
            crossing_jump_ratio: 2.0,
            baseline_paliers: 3,
            min_baseline_paliers: 1,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Saturation {
    pub hold_ms: f64,
    /// Fraction of the limit that counts as being at it.
    pub threshold: f64,
}

impl Default for Saturation {
    fn default() -> Self {
        Self {
            hold_ms: 3000.0,
            threshold: 0.98,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Convergence {
    /// The error band as a fraction of the target, so it scales with the
    /// operating point: 0.02 % of 200 ms is 0.04 ms, of 500 ms is 0.10 ms.
    pub err_fraction: f64,
    pub err_floor_ms: f64,
    pub hold_ms: f64,
}

impl Default for Convergence {
    fn default() -> Self {
        Self {
            err_fraction: 0.0002,
            err_floor_ms: 0.0,
            hold_ms: 10_000.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SourceLoss {
    pub window_ms: f64,
    pub min_low_recover_events: u32,
}

impl Default for SourceLoss {
    fn default() -> Self {
        Self {
            window_ms: 10_000.0,
            min_low_recover_events: 2,
        }
    }
}

/// The rate adjustment in parts per million, the unit the controller's own
/// limits are written in.
pub fn rate_adjust_ppm(sample: &Sample) -> Option<f64> {
    sample
        .resample_ratio
        .filter(|r| r.is_finite())
        .map(|r| (r - 1.0) * 1e6)
}

/// How far the smoothed latency is from its target.
pub fn error_ms(sample: &Sample) -> Option<f64> {
    Some(sample.latency_smoothed_ms? - sample.latency_target_ms?)
}

/// The trailing `window_ms` of samples.
fn slice_by_window(samples: &[Sample], window_ms: f64) -> &[Sample] {
    let Some(last) = samples.last() else {
        return samples;
    };
    let cutoff = last.t - window_ms;
    let start = samples.partition_point(|s| s.t < cutoff);
    &samples[start..]
}

/// What one palier of the kp sweep looked like.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PalierStats {
    pub peak_to_peak_ppm: f64,
    pub crossings: u32,
    /// Crossings per second, so paliers of unequal length compare.
    pub crossing_rate: f64,
    pub mean_ppm: f64,
    pub samples: usize,
    pub stable_duration_ms: f64,
}

/// Descriptive statistics for one palier, over the rate adjustment and after
/// the warm-up. `None` when the palier holds too little to describe.
pub fn compute_palier_stats(
    samples: &[Sample],
    palier_start_ms: f64,
    cfg: &Oscillation,
) -> Option<PalierStats> {
    let from = palier_start_ms + cfg.palier_warmup_ms;
    let stable = || {
        samples
            .iter()
            .filter(move |s| s.t >= from)
            .filter_map(|s| rate_adjust_ppm(s).map(|v| (s.t, v)))
    };
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut sum = 0.0;
    let mut n = 0usize;
    let mut first_t = None;
    let mut last_t = 0.0;
    for (t, v) in stable() {
        first_t.get_or_insert(t);
        last_t = t;
        min = min.min(v);
        max = max.max(v);
        sum += v;
        n += 1;
    }
    let first_t = first_t?;
    if n < 4 {
        return None;
    }
    let mean = sum / n as f64;
    // Crossings of the mean, with a dead band: the question is how often the
    // signal swings across its own centre, not how often it wobbles.
    let mut state = 0i8;
    let mut crossings = 0u32;
    for (_, v) in stable() {
        if v > mean + cfg.hysteresis_ppm {
            if state == -1 {
                crossings += 1;
            }
            state = 1;
        } else if v < mean - cfg.hysteresis_ppm {
            if state == 1 {
                crossings += 1;
            }
            state = -1;
        }
    }
    let stable_duration_ms = last_t - first_t;
    Some(PalierStats {
        peak_to_peak_ppm: max - min,
        crossings,
        crossing_rate: if stable_duration_ms > 0.0 {
            crossings as f64 / stable_duration_ms * 1000.0
        } else {
            0.0
        },
        mean_ppm: mean,
        samples: n,
        stable_duration_ms,
    })
}

/// Why a palier was not called oscillating.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotOscillating {
    InsufficientSamples,
    CrossingsBelowFloor,
    AmplitudeBelowFloor,
    BaselineTooShort,
    JumpBelowRatio,
}

/// "Is this palier oscillating?" on absolute floors alone, for the phases
/// that have no kp sweep behind them to compare against.
pub fn detect_oscillation_absolute(
    samples: &[Sample],
    palier_start_ms: f64,
    cfg: &Oscillation,
) -> Result<PalierStats, NotOscillating> {
    let Some(stats) = compute_palier_stats(samples, palier_start_ms, cfg) else {
        return Err(NotOscillating::InsufficientSamples);
    };
    if stats.crossings < cfg.min_crossings_absolute {
        return Err(NotOscillating::CrossingsBelowFloor);
    }
    if stats.peak_to_peak_ppm < cfg.min_absolute_peak_to_peak_ppm {
        return Err(NotOscillating::AmplitudeBelowFloor);
    }
    Ok(stats)
}

/// The jump the kp sweep is looking for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Jump {
    pub peak_to_peak: f64,
    pub crossing_rate: f64,
}

/// Declare oscillation by comparing a palier against the ones before it.
///
/// The moment a loop starts ringing is a sharp jump in *both* amplitude and
/// crossing rate at once. Comparing against the run's own earlier paliers
/// rather than against fixed numbers is what makes the same thresholds work on
/// hardware whose quiet noise floor is ten times another's.
pub fn detect_oscillation_by_jump(
    current: Option<&PalierStats>,
    baselines: &[PalierStats],
    cfg: &Oscillation,
) -> Result<Jump, NotOscillating> {
    let Some(current) = current else {
        return Err(NotOscillating::InsufficientSamples);
    };
    if current.crossings < cfg.min_crossings_absolute {
        return Err(NotOscillating::CrossingsBelowFloor);
    }
    if current.peak_to_peak_ppm < cfg.min_absolute_peak_to_peak_ppm {
        return Err(NotOscillating::AmplitudeBelowFloor);
    }
    if baselines.len() < cfg.min_baseline_paliers {
        return Err(NotOscillating::BaselineTooShort);
    }
    let max_pp = baselines
        .iter()
        .map(|s| s.peak_to_peak_ppm)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_cr = baselines
        .iter()
        .map(|s| s.crossing_rate)
        .fold(f64::NEG_INFINITY, f64::max);
    // A baseline that never moved makes any movement an infinite jump, which
    // is the right answer rather than a division to guard against.
    let jump = Jump {
        peak_to_peak: if max_pp > 0.0 {
            current.peak_to_peak_ppm / max_pp
        } else {
            f64::INFINITY
        },
        crossing_rate: if max_cr > 0.0 {
            current.crossing_rate / max_cr
        } else {
            f64::INFINITY
        },
    };
    if jump.peak_to_peak >= cfg.peak_to_peak_jump_ratio
        && jump.crossing_rate >= cfg.crossing_jump_ratio
    {
        Ok(jump)
    } else {
        Err(NotOscillating::JumpBelowRatio)
    }
}

/// How long the rate adjustment has been sitting at its limit.
pub fn detect_saturation(
    samples: &[Sample],
    max_adjust_ratio: f64,
    cfg: &Saturation,
) -> (bool, f64) {
    if samples.is_empty() || max_adjust_ratio == 0.0 {
        return (false, 0.0);
    }
    let limit = cfg.threshold * max_adjust_ratio.abs() * 1e6;
    let now = samples[samples.len() - 1].t;
    let mut start = None;
    for sample in samples.iter().rev() {
        match rate_adjust_ppm(sample) {
            Some(v) if v.abs() >= limit => start = Some(sample.t),
            _ => break,
        }
    }
    let Some(start) = start else {
        return (false, 0.0);
    };
    let duration = now - start;
    (duration >= cfg.hold_ms, duration)
}

#[derive(Clone, Copy, Debug)]
pub struct Converged {
    pub converged: bool,
    pub duration_ms: f64,
    pub limit_ms: Option<f64>,
}

/// How long the latency has been within its band of the target.
pub fn detect_convergence(samples: &[Sample], cfg: &Convergence) -> Converged {
    if samples.is_empty() {
        return Converged {
            converged: false,
            duration_ms: 0.0,
            limit_ms: None,
        };
    }
    let now = samples[samples.len() - 1].t;
    let mut start = now;
    let mut limit_used = None;
    for sample in samples.iter().rev() {
        let Some(err) = error_ms(sample) else { break };
        let limit = match sample.latency_target_ms {
            Some(target) => cfg.err_floor_ms.max(target.abs() * cfg.err_fraction),
            None => cfg.err_floor_ms,
        };
        if limit <= 0.0 || err.abs() >= limit {
            break;
        }
        limit_used = Some(limit);
        start = sample.t;
    }
    let duration_ms = now - start;
    Converged {
        converged: duration_ms >= cfg.hold_ms,
        duration_ms,
        limit_ms: limit_used,
    }
}

/// Entries into the recovery regime in the trailing window: one per
/// transition, so a long outage counts once rather than once per sample.
pub fn detect_source_loss(samples: &[Sample], cfg: &SourceLoss) -> (bool, u32) {
    if samples.is_empty() {
        return (false, 0);
    }
    let mut events = 0;
    let mut inside = false;
    for sample in slice_by_window(samples, cfg.window_ms) {
        let low = sample.phase == Phase::LowRecover;
        if low && !inside {
            events += 1;
        }
        inside = low;
    }
    (events >= cfg.min_low_recover_events, events)
}

/// What the long run says about the rate adjustment, which is how the final
/// limit is sized.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RateStats {
    pub peak_abs_ppm: f64,
    pub mean_ppm: f64,
    pub std_ppm: f64,
    pub samples: usize,
}

pub fn compute_rate_stats(samples: &[Sample], window_ms: Option<f64>) -> RateStats {
    let window = match window_ms {
        Some(ms) => slice_by_window(samples, ms),
        None => samples,
    };
    let mut stats = RateStats::default();
    let mut sum = 0.0;
    let mut sum_sq = 0.0;
    for value in window.iter().filter_map(rate_adjust_ppm) {
        stats.peak_abs_ppm = stats.peak_abs_ppm.max(value.abs());
        sum += value;
        sum_sq += value * value;
        stats.samples += 1;
    }
    if stats.samples == 0 {
        return RateStats::default();
    }
    let n = stats.samples as f64;
    stats.mean_ppm = sum / n;
    stats.std_ppm = (sum_sq / n - stats.mean_ppm * stats.mean_ppm)
        .max(0.0)
        .sqrt();
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A palier of a sine at `ppm` amplitude, sampled every 100 ms.
    fn palier(start_ms: f64, duration_ms: f64, amplitude_ppm: f64, hz: f64) -> Vec<Sample> {
        let mut out = Vec::new();
        let mut t = start_ms;
        while t <= start_ms + duration_ms {
            let phase = 2.0 * std::f64::consts::PI * hz * (t - start_ms) / 1000.0;
            out.push(Sample {
                t,
                resample_ratio: Some(1.0 + amplitude_ppm * phase.sin() / 1e6),
                latency_smoothed_ms: Some(40.0),
                latency_target_ms: Some(40.0),
                phase: Phase::Other,
            });
            t += 100.0;
        }
        out
    }

    /// The warm-up is discarded, so the transient that follows a patch cannot
    /// be mistaken for the regime the palier is measuring.
    #[test]
    fn the_warm_up_is_not_part_of_the_palier() {
        let mut samples = palier(0.0, 30_000.0, 100.0, 1.0);
        // A violent excursion inside the warm-up window.
        samples[10].resample_ratio = Some(1.5);
        let cfg = Oscillation::default();
        let stats = compute_palier_stats(&samples, 0.0, &cfg).expect("stats");
        assert!(
            stats.peak_to_peak_ppm < 1000.0,
            "the transient leaked into the palier: {} ppm",
            stats.peak_to_peak_ppm
        );
        assert!(stats.stable_duration_ms > 19_000.0);
    }

    #[test]
    fn a_quiet_palier_is_not_oscillating_and_a_loud_one_is() {
        let cfg = Oscillation::default();
        // Swinging often enough to count, but well under the amplitude floor.
        let quiet = palier(0.0, 30_000.0, 400.0, 1.0);
        assert_eq!(
            detect_oscillation_absolute(&quiet, 0.0, &cfg),
            Err(NotOscillating::AmplitudeBelowFloor)
        );
        // Inside the dead band it never crosses at all, which is the first
        // thing asked and the cheaper answer.
        let still = palier(0.0, 30_000.0, 100.0, 1.0);
        assert_eq!(
            detect_oscillation_absolute(&still, 0.0, &cfg),
            Err(NotOscillating::CrossingsBelowFloor)
        );
        // Over the floor, and swinging often enough.
        let loud = palier(0.0, 30_000.0, 4000.0, 1.0);
        assert!(detect_oscillation_absolute(&loud, 0.0, &cfg).is_ok());
        // Loud but barely crossing: one slow swing is not a ringing loop.
        let slow = palier(0.0, 30_000.0, 4000.0, 0.03);
        assert_eq!(
            detect_oscillation_absolute(&slow, 0.0, &cfg),
            Err(NotOscillating::CrossingsBelowFloor)
        );
    }

    /// The sweep declares oscillation on a jump against the run's own earlier
    /// paliers, so a machine whose noise floor is ten times another's needs no
    /// different numbers.
    #[test]
    fn oscillation_is_a_jump_against_the_paliers_before_it() {
        let cfg = Oscillation::default();
        let baseline = compute_palier_stats(&palier(0.0, 30_000.0, 400.0, 1.0), 0.0, &cfg)
            .expect("a baseline");
        let ringing =
            compute_palier_stats(&palier(0.0, 30_000.0, 4000.0, 3.0), 0.0, &cfg).expect("a palier");
        assert!(detect_oscillation_by_jump(Some(&ringing), &[baseline], &cfg).is_ok());
        // The same palier with nothing to compare against keeps the sweep
        // going rather than declaring on absolute numbers alone.
        assert_eq!(
            detect_oscillation_by_jump(Some(&ringing), &[], &cfg),
            Err(NotOscillating::BaselineTooShort)
        );
        // A palier no louder than the baseline is not a jump.
        let same =
            compute_palier_stats(&palier(0.0, 30_000.0, 4000.0, 3.0), 0.0, &cfg).expect("a palier");
        assert_eq!(
            detect_oscillation_by_jump(Some(&ringing), &[same], &cfg),
            Err(NotOscillating::JumpBelowRatio)
        );
    }

    #[test]
    fn saturation_is_measured_from_the_end_backwards() {
        let cfg = Saturation::default();
        let at_limit = |t: f64| Sample {
            t,
            resample_ratio: Some(1.1),
            ..Default::default()
        };
        let free = |t: f64| Sample {
            t,
            resample_ratio: Some(1.0),
            ..Default::default()
        };
        // Five seconds pinned at a limit of 10 %.
        let samples: Vec<Sample> = (0..10)
            .map(|i| free(i as f64 * 500.0))
            .chain((10..21).map(|i| at_limit(i as f64 * 500.0)))
            .collect();
        let (saturated, duration) = detect_saturation(&samples, 0.10, &cfg);
        assert!(saturated && (duration - 5000.0).abs() < 1.0);
        // An earlier stretch at the limit does not count once it has been left.
        let recovered: Vec<Sample> = samples.iter().copied().chain([free(10_500.0)]).collect();
        assert_eq!(detect_saturation(&recovered, 0.10, &cfg), (false, 0.0));
    }

    /// The convergence band is a fraction of the target, so the same setting
    /// means the same thing at 40 ms and at 500 ms.
    #[test]
    fn convergence_scales_with_the_target() {
        let cfg = Convergence::default();
        let run = |target: f64, error: f64| -> Converged {
            let samples: Vec<Sample> = (0..=200)
                .map(|i| Sample {
                    t: i as f64 * 100.0,
                    latency_smoothed_ms: Some(target + error),
                    latency_target_ms: Some(target),
                    ..Default::default()
                })
                .collect();
            detect_convergence(&samples, &cfg)
        };
        // 0.02 % of 500 ms is 0.1 ms: an 0.05 ms error is inside it.
        assert!(run(500.0, 0.05).converged);
        // The same error against a 40 ms target is outside its 0.008 ms band.
        assert!(!run(40.0, 0.05).converged);
    }

    #[test]
    fn one_outage_counts_once_however_long_it_lasts() {
        let sample = |t: f64, phase: Phase| Sample {
            t,
            phase,
            ..Default::default()
        };
        let mut samples = vec![sample(0.0, Phase::Other)];
        samples.extend((1..40).map(|i| sample(i as f64 * 100.0, Phase::LowRecover)));
        let cfg = SourceLoss::default();
        assert_eq!(detect_source_loss(&samples, &cfg), (false, 1));
        // A second entry is what makes it a lost source rather than a hiccup.
        samples.push(sample(4000.0, Phase::Other));
        samples.push(sample(4100.0, Phase::LowRecover));
        assert_eq!(detect_source_loss(&samples, &cfg), (true, 2));
    }

    #[test]
    fn the_long_run_statistics_are_taken_over_the_window_asked_for() {
        let samples: Vec<Sample> = (0..100)
            .map(|i| Sample {
                t: i as f64 * 100.0,
                // The first half swings hard, the last half barely moves.
                resample_ratio: Some(1.0 + if i < 50 { 500.0 } else { 10.0 } / 1e6),
                ..Default::default()
            })
            .collect();
        let all = compute_rate_stats(&samples, None);
        assert!((all.peak_abs_ppm - 500.0).abs() < 1e-6);
        let recent = compute_rate_stats(&samples, Some(2000.0));
        assert!((recent.peak_abs_ppm - 10.0).abs() < 1e-6);
        assert_eq!(compute_rate_stats(&[], None), RateStats::default());
    }
}
