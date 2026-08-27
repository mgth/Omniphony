//! Detectors for the PI auto-tune state machine.
//!
//! A faithful port of `src/auto-tune/detectors.js`. These decide when the
//! resampler's control loop is oscillating, saturated, converged, or has lost
//! its source — and those verdicts drive kp/ki changes on a running audio
//! path. A detector that fires a little differently here than it did in the
//! frontend would retune the loop differently, which is not something a reader
//! would catch by eye.
//!
//! So the port is asserted against the frontend rather than against my reading
//! of it: `scripts/dump-auto-tune-goldens.mjs` runs the JS detectors over a set
//! of synthetic telemetry windows and records their verdicts, and
//! `golden_vectors_match_the_frontend` replays every one through this module.
//! The JS is the reference here — it is the implementation that works — which
//! is the opposite direction from the geometry crate, where Rust is canonical.
//!
//! Nothing in this module is wired to the running state machine yet; the FSM
//! that consumes these is a separate step.

/// Thresholds for the oscillation detectors.
#[derive(Debug, Clone, Copy)]
pub struct OscillationThresholds {
    /// Discard this much of each palier — the transient response to the kp
    /// patch is not representative of the steady state.
    pub palier_warmup_ms: f64,
    /// Dead-band around the mean, so noise does not count as a crossing.
    pub hysteresis_ppm: f64,
    pub min_crossings_absolute: u32,
    pub min_absolute_peak_to_peak_ppm: f64,
    /// Current peak-to-peak must reach this multiple of the baseline's.
    pub peak_to_peak_jump_ratio: f64,
    /// And the crossing rate this multiple of the baseline's.
    pub crossing_jump_ratio: f64,
    pub baseline_paliers: usize,
    pub min_baseline_paliers: usize,
}

impl Default for OscillationThresholds {
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

#[derive(Debug, Clone, Copy)]
pub struct SaturationThresholds {
    pub hold_ms: f64,
    pub threshold: f64,
}

impl Default for SaturationThresholds {
    fn default() -> Self {
        Self {
            hold_ms: 3000.0,
            threshold: 0.98,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ConvergenceThresholds {
    /// `|smoothed - target| < |target| * err_fraction`.
    pub err_fraction: f64,
    /// Absolute floor; zero trusts the fraction entirely.
    pub err_floor_ms: f64,
    pub hold_ms: f64,
}

impl Default for ConvergenceThresholds {
    fn default() -> Self {
        Self {
            err_fraction: 0.0002,
            err_floor_ms: 0.0,
            hold_ms: 10_000.0,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SourceLossThresholds {
    pub window_ms: f64,
    pub min_low_recover_events: u32,
}

impl Default for SourceLossThresholds {
    fn default() -> Self {
        Self {
            window_ms: 10_000.0,
            min_low_recover_events: 2,
        }
    }
}

/// One telemetry sample, as the runner polls it.
#[derive(Debug, Clone, Default)]
pub struct Sample {
    pub t: f64,
    pub latency_smoothed_ms: Option<f64>,
    pub latency_target_ms: Option<f64>,
    pub resample_ratio: Option<f64>,
    pub phase: Option<String>,
}

/// Resampler correction in parts per million, or `None` when the sample does
/// not carry a usable ratio.
pub fn rate_adjust_ppm(sample: &Sample) -> Option<f64> {
    match sample.resample_ratio {
        Some(ratio) if ratio.is_finite() => Some((ratio - 1.0) * 1e6),
        _ => None,
    }
}

/// Signed latency error against the target.
pub fn error_ms(sample: &Sample) -> Option<f64> {
    match (sample.latency_smoothed_ms, sample.latency_target_ms) {
        (Some(smoothed), Some(target)) => Some(smoothed - target),
        _ => None,
    }
}

/// Trailing `window_ms` of samples.
///
/// The cutoff is measured from the newest sample, not from wall-clock, so a
/// stalled feed keeps its window rather than emptying it.
fn slice_by_window(samples: &[Sample], window_ms: f64) -> &[Sample] {
    if samples.is_empty() {
        return samples;
    }
    let cutoff = samples[samples.len() - 1].t - window_ms;
    let mut i = 0;
    while i < samples.len() && samples[i].t < cutoff {
        i += 1;
    }
    &samples[i..]
}

/// Descriptive statistics for one kp palier, on `rate_adjust_ppm`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PalierStats {
    pub peak_to_peak_ppm: f64,
    pub crossings: u32,
    /// Crossings per second over the stable part of the palier.
    pub crossing_rate: f64,
    pub mean_ppm: f64,
    pub samples: usize,
    pub stable_duration_ms: f64,
}

/// Statistics for the settled part of a palier, or `None` when too little of
/// it survived the warm-up to say anything.
pub fn compute_palier_stats(
    samples: &[Sample],
    palier_start_ms: f64,
    cfg: &OscillationThresholds,
) -> Option<PalierStats> {
    if samples.is_empty() {
        return None;
    }
    let from_ms = palier_start_ms + cfg.palier_warmup_ms;
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    let mut sum = 0.0;
    let mut n = 0usize;
    let mut first_stable_t: Option<f64> = None;
    let mut last_t = 0.0;

    for s in samples {
        if s.t < from_ms {
            continue;
        }
        let Some(v) = rate_adjust_ppm(s) else {
            continue;
        };
        if first_stable_t.is_none() {
            first_stable_t = Some(s.t);
        }
        last_t = s.t;
        if v < min {
            min = v;
        }
        if v > max {
            max = v;
        }
        sum += v;
        n += 1;
    }

    let first_stable_t = first_stable_t?;
    if n < 4 {
        return None;
    }
    let mean = sum / n as f64;

    // Second pass: count mean crossings outside the dead-band. A crossing is
    // only counted on a sign change of the band state, so noise sitting inside
    // the band contributes nothing.
    let mut state = 0i8;
    let mut crossings = 0u32;
    for s in samples {
        if s.t < from_ms {
            continue;
        }
        let Some(v) = rate_adjust_ppm(s) else {
            continue;
        };
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

    let stable_duration_ms = last_t - first_stable_t;
    let crossing_rate = if stable_duration_ms > 0.0 {
        (crossings as f64 / stable_duration_ms) * 1000.0
    } else {
        0.0
    };

    Some(PalierStats {
        peak_to_peak_ppm: max - min,
        crossings,
        crossing_rate,
        mean_ppm: mean,
        samples: n,
        stable_duration_ms,
    })
}

/// Why a detector declined to fire. Kept as a string to match the frontend's
/// vocabulary exactly — these end up in the wizard's log.
pub type Reason = Option<&'static str>;

#[derive(Debug, Clone)]
pub struct OscillationVerdict {
    pub oscillating: bool,
    pub reason: Reason,
    pub stats: Option<PalierStats>,
}

/// "Is this palier oscillating?" on absolute floors alone.
///
/// Used where there is no kp-sweep baseline to compare against — recovery
/// after a perturbation, and the tightening palier.
pub fn detect_oscillation_absolute(
    samples: &[Sample],
    palier_start_ms: f64,
    cfg: &OscillationThresholds,
) -> OscillationVerdict {
    let Some(stats) = compute_palier_stats(samples, palier_start_ms, cfg) else {
        return OscillationVerdict {
            oscillating: false,
            reason: Some("insufficient-samples"),
            stats: None,
        };
    };
    if stats.crossings < cfg.min_crossings_absolute {
        return OscillationVerdict {
            oscillating: false,
            reason: Some("crossings-below-floor"),
            stats: Some(stats),
        };
    }
    if stats.peak_to_peak_ppm < cfg.min_absolute_peak_to_peak_ppm {
        return OscillationVerdict {
            oscillating: false,
            reason: Some("amplitude-below-floor"),
            stats: Some(stats),
        };
    }
    OscillationVerdict {
        oscillating: true,
        reason: None,
        stats: Some(stats),
    }
}

#[derive(Debug, Clone)]
pub struct JumpVerdict {
    pub oscillating: bool,
    pub reason: Reason,
    pub max_baseline_peak_to_peak_ppm: Option<f64>,
    pub max_baseline_crossing_rate: Option<f64>,
    /// Infinite when the baseline is flat, which is a jump by any measure.
    pub peak_to_peak_jump: Option<f64>,
    pub crossing_jump: Option<f64>,
}

impl JumpVerdict {
    fn rejected(reason: &'static str) -> Self {
        Self {
            oscillating: false,
            reason: Some(reason),
            max_baseline_peak_to_peak_ppm: None,
            max_baseline_crossing_rate: None,
            peak_to_peak_jump: None,
            crossing_jump: None,
        }
    }
}

/// Declare oscillation by comparing a palier against the previous
/// non-saturated ones.
///
/// The transition from quasi-flat noise to real oscillation shows up as a
/// sharp jump in **both** amplitude and crossing rate. Comparing ratios rather
/// than absolute levels is what keeps the detector portable between machines
/// whose noise floors differ.
pub fn detect_oscillation_by_jump(
    current: Option<&PalierStats>,
    baselines: &[Option<PalierStats>],
    cfg: &OscillationThresholds,
) -> JumpVerdict {
    let Some(current) = current else {
        return JumpVerdict::rejected("no-current-stats");
    };
    if current.crossings < cfg.min_crossings_absolute {
        return JumpVerdict::rejected("crossings-below-floor");
    }
    if current.peak_to_peak_ppm < cfg.min_absolute_peak_to_peak_ppm {
        return JumpVerdict::rejected("amplitude-below-floor");
    }
    let present: Vec<&PalierStats> = baselines.iter().flatten().collect();
    if present.len() < cfg.min_baseline_paliers {
        return JumpVerdict::rejected("baseline-too-short");
    }

    let max_pp = present
        .iter()
        .map(|s| s.peak_to_peak_ppm)
        .fold(f64::NEG_INFINITY, f64::max);
    let max_cr = present
        .iter()
        .map(|s| s.crossing_rate)
        .fold(f64::NEG_INFINITY, f64::max);
    // A flat baseline divides to infinity rather than to a finite ratio: any
    // movement at all is an infinite jump from nothing.
    let pp_jump = if max_pp > 0.0 {
        current.peak_to_peak_ppm / max_pp
    } else {
        f64::INFINITY
    };
    let cr_jump = if max_cr > 0.0 {
        current.crossing_rate / max_cr
    } else {
        f64::INFINITY
    };
    let oscillating = pp_jump >= cfg.peak_to_peak_jump_ratio && cr_jump >= cfg.crossing_jump_ratio;

    JumpVerdict {
        oscillating,
        reason: if oscillating {
            None
        } else {
            Some("jump-below-ratio")
        },
        max_baseline_peak_to_peak_ppm: Some(max_pp),
        max_baseline_crossing_rate: Some(max_cr),
        peak_to_peak_jump: Some(pp_jump),
        crossing_jump: Some(cr_jump),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SaturationVerdict {
    pub saturated: bool,
    pub duration_ms: f64,
}

/// Saturation: `|rate_adjust_ppm|` has stayed at the adjustment limit for the
/// whole trailing `hold_ms`.
pub fn detect_saturation(
    samples: &[Sample],
    max_adjust_ratio: f64,
    cfg: &SaturationThresholds,
) -> SaturationVerdict {
    if samples.is_empty() || max_adjust_ratio == 0.0 {
        return SaturationVerdict {
            saturated: false,
            duration_ms: 0.0,
        };
    }
    let limit = cfg.threshold * max_adjust_ratio.abs() * 1e6;
    let now_ms = samples[samples.len() - 1].t;
    let mut start_time: Option<f64> = None;
    // Walk back from the newest sample and stop at the first that is not
    // pinned: saturation has to be unbroken to count.
    for s in samples.iter().rev() {
        match rate_adjust_ppm(s) {
            Some(v) if v.abs() >= limit => start_time = Some(s.t),
            _ => break,
        }
    }
    let Some(start_time) = start_time else {
        return SaturationVerdict {
            saturated: false,
            duration_ms: 0.0,
        };
    };
    let duration_ms = now_ms - start_time;
    SaturationVerdict {
        saturated: duration_ms >= cfg.hold_ms,
        duration_ms,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConvergenceVerdict {
    pub converged: bool,
    pub duration_ms: f64,
    pub limit_ms: Option<f64>,
}

/// Convergence: the latency error has stayed inside the tolerance for the
/// whole trailing `hold_ms`.
///
/// The tolerance is a fraction of the target rather than an absolute figure,
/// so it scales with the operating point — 0.02% is 0.04 ms at a 200 ms target
/// and 0.10 ms at 500 ms.
pub fn detect_convergence(samples: &[Sample], cfg: &ConvergenceThresholds) -> ConvergenceVerdict {
    if samples.is_empty() {
        return ConvergenceVerdict {
            converged: false,
            duration_ms: 0.0,
            limit_ms: None,
        };
    }
    let now_ms = samples[samples.len() - 1].t;
    let mut start_time = now_ms;
    let mut limit_used: Option<f64> = None;

    for s in samples.iter().rev() {
        let Some(err) = error_ms(s) else { break };
        let limit = match s.latency_target_ms {
            Some(target) => cfg.err_floor_ms.max(target.abs() * cfg.err_fraction),
            None => cfg.err_floor_ms,
        };
        // A non-positive limit can never be met, so it ends the run rather
        // than counting as converged-with-zero-tolerance.
        if limit <= 0.0 || err.abs() >= limit {
            break;
        }
        limit_used = Some(limit);
        start_time = s.t;
    }

    let duration_ms = now_ms - start_time;
    ConvergenceVerdict {
        converged: duration_ms >= cfg.hold_ms,
        duration_ms,
        limit_ms: limit_used,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SourceLossVerdict {
    pub lost: bool,
    pub events: u32,
}

/// Source loss: repeated entries into the low-recover phase inside the
/// trailing window.
///
/// Counted per *transition*, so one long low-recover stretch is one event and
/// not one per sample.
pub fn detect_source_loss(samples: &[Sample], cfg: &SourceLossThresholds) -> SourceLossVerdict {
    if samples.is_empty() {
        return SourceLossVerdict {
            lost: false,
            events: 0,
        };
    }
    let window = slice_by_window(samples, cfg.window_ms);
    let mut events = 0u32;
    let mut in_low_recover = false;
    for s in window {
        let lr = s.phase.as_deref() == Some("low-recover");
        if lr && !in_low_recover {
            events += 1;
        }
        in_low_recover = lr;
    }
    SourceLossVerdict {
        lost: events >= cfg.min_low_recover_events,
        events,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RateStats {
    pub peak_abs_ppm: f64,
    pub mean_ppm: f64,
    pub std_ppm: f64,
    pub samples: usize,
}

/// Long-run statistics on `rate_adjust_ppm`, used to size `max_adjust_final`.
///
/// `window_ms` of `None` uses the whole series.
pub fn compute_rate_stats(samples: &[Sample], window_ms: Option<f64>) -> RateStats {
    let window = match window_ms {
        Some(ms) => slice_by_window(samples, ms),
        None => samples,
    };
    let mut peak_abs = 0.0f64;
    let mut sum = 0.0;
    let mut sum_sq = 0.0;
    let mut n = 0usize;
    for s in window {
        let Some(v) = rate_adjust_ppm(s) else {
            continue;
        };
        if v.abs() > peak_abs {
            peak_abs = v.abs();
        }
        sum += v;
        sum_sq += v * v;
        n += 1;
    }
    if n == 0 {
        return RateStats {
            peak_abs_ppm: 0.0,
            mean_ppm: 0.0,
            std_ppm: 0.0,
            samples: 0,
        };
    }
    let mean = sum / n as f64;
    // Clamped at zero: the sum-of-squares form can go slightly negative on
    // near-constant input, and a negative variance has no square root.
    let variance = (sum_sq / n as f64 - mean * mean).max(0.0);
    RateStats {
        peak_abs_ppm: peak_abs,
        mean_ppm: mean,
        std_ppm: variance.sqrt(),
        samples: n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    /// Load the verdicts recorded from the frontend.
    fn goldens() -> Value {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../scripts/golden/auto-tune.json"
        );
        let text = std::fs::read_to_string(path).expect(
            "auto-tune goldens missing — regenerate with \
             `node scripts/dump-auto-tune-goldens.mjs > scripts/golden/auto-tune.json`",
        );
        serde_json::from_str(&text).expect("goldens must be valid JSON")
    }

    fn samples_from(value: &Value) -> Vec<Sample> {
        value
            .as_array()
            .expect("a window is an array")
            .iter()
            .map(|s| Sample {
                t: s["t"].as_f64().unwrap(),
                latency_smoothed_ms: s["latencySmoothedMs"].as_f64(),
                latency_target_ms: s["latencyTargetMs"].as_f64(),
                resample_ratio: s["resampleRatio"].as_f64(),
                phase: s["phase"].as_str().map(str::to_string),
            })
            .collect()
    }

    fn close(actual: f64, expected: f64, what: &str) {
        assert!(
            (actual - expected).abs() < 1e-6,
            "{what}: expected {expected}, got {actual}"
        );
    }

    /// `JSON.stringify` writes a non-finite number as `null`, which is how a
    /// flat baseline's infinite jump ratio arrives.
    fn close_or_infinite(actual: Option<f64>, expected: &Value, what: &str) {
        match (actual, expected.as_f64()) {
            (Some(a), Some(e)) => close(a, e, what),
            (Some(a), None) => assert!(a.is_infinite(), "{what}: expected infinity, got {a}"),
            (None, _) => assert!(expected.is_null(), "{what}: expected a value, got none"),
        }
    }

    /// Replay every recorded verdict.
    ///
    /// Verified to bite: changing the hysteresis band, the crossings floor, or
    /// a hold comparison from `>=` to `>` each fails this test.
    ///
    /// One edge is deliberately *not* covered: `>=` versus `>` on the
    /// convergence error test. Distinguishing them needs `|err|` to equal the
    /// tolerance to the last bit, which no realistic sample does — the fixture
    /// would be pinning a float coincidence rather than a behaviour.
    #[test]
    fn golden_vectors_match_the_frontend() {
        let g = goldens();
        let osc = OscillationThresholds::default();
        let sat = SaturationThresholds::default();
        let conv = ConvergenceThresholds::default();
        let loss = SourceLossThresholds::default();

        let windows: Vec<(String, Vec<Sample>)> = g["windows"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, v)| (name.clone(), samples_from(v)))
            .collect();
        assert!(windows.len() >= 10, "goldens look truncated");

        for (name, samples) in &windows {
            // ── palier stats ────────────────────────────────────────────────
            let stats = compute_palier_stats(samples, 0.0, &osc);
            let expected = &g["palierStats"][name];
            if expected.is_null() {
                assert!(stats.is_none(), "{name}: expected no palier stats");
            } else {
                let s = stats.expect(&format!("{name}: expected palier stats"));
                close(
                    s.peak_to_peak_ppm,
                    expected["peakToPeakPpm"].as_f64().unwrap(),
                    &format!("{name}.peakToPeak"),
                );
                assert_eq!(
                    s.crossings as u64,
                    expected["crossings"].as_u64().unwrap(),
                    "{name}.crossings"
                );
                close(
                    s.crossing_rate,
                    expected["crossingRate"].as_f64().unwrap(),
                    &format!("{name}.crossingRate"),
                );
                close(
                    s.mean_ppm,
                    expected["meanPpm"].as_f64().unwrap(),
                    &format!("{name}.meanPpm"),
                );
                assert_eq!(
                    s.samples as u64,
                    expected["samples"].as_u64().unwrap(),
                    "{name}.samples"
                );
            }

            // ── oscillation, absolute ───────────────────────────────────────
            let verdict = detect_oscillation_absolute(samples, 0.0, &osc);
            let expected = &g["oscillationAbsolute"][name];
            assert_eq!(
                verdict.oscillating,
                expected["oscillating"].as_bool().unwrap(),
                "{name}.oscillating"
            );
            assert_eq!(verdict.reason, expected["reason"].as_str(), "{name}.reason");

            // ── saturation ──────────────────────────────────────────────────
            let verdict = detect_saturation(samples, 0.01, &sat);
            let expected = &g["saturation"][name];
            assert_eq!(
                verdict.saturated,
                expected["saturated"].as_bool().unwrap(),
                "{name}.saturated"
            );
            close(
                verdict.duration_ms,
                expected["durationMs"].as_f64().unwrap(),
                &format!("{name}.satDuration"),
            );

            // ── convergence ─────────────────────────────────────────────────
            let verdict = detect_convergence(samples, &conv);
            let expected = &g["convergence"][name];
            assert_eq!(
                verdict.converged,
                expected["converged"].as_bool().unwrap(),
                "{name}.converged"
            );
            close(
                verdict.duration_ms,
                expected["durationMs"].as_f64().unwrap(),
                &format!("{name}.convDuration"),
            );

            // ── source loss ─────────────────────────────────────────────────
            let verdict = detect_source_loss(samples, &loss);
            let expected = &g["sourceLoss"][name];
            assert_eq!(
                verdict.lost,
                expected["lost"].as_bool().unwrap(),
                "{name}.lost"
            );
            assert_eq!(
                verdict.events as u64,
                expected["events"].as_u64().unwrap(),
                "{name}.events"
            );

            // ── rate stats, whole series and windowed ───────────────────────
            for (key, window) in [("rateStatsAll", None), ("rateStatsWindowed", Some(5000.0))] {
                let stats = compute_rate_stats(samples, window);
                let expected = &g[key][name];
                close(
                    stats.peak_abs_ppm,
                    expected["peakAbsPpm"].as_f64().unwrap(),
                    &format!("{name}.{key}.peak"),
                );
                close(
                    stats.mean_ppm,
                    expected["meanPpm"].as_f64().unwrap(),
                    &format!("{name}.{key}.mean"),
                );
                close(
                    stats.std_ppm,
                    expected["stdPpm"].as_f64().unwrap(),
                    &format!("{name}.{key}.std"),
                );
                assert_eq!(
                    stats.samples as u64,
                    expected["samples"].as_u64().unwrap(),
                    "{name}.{key}.samples"
                );
            }
        }

        // Saturation with no configured limit never fires.
        let saturated = samples_from(&g["windows"]["saturated"]);
        let verdict = detect_saturation(&saturated, 0.0, &sat);
        assert_eq!(
            verdict.saturated,
            g["saturationNoLimit"]["saturated"].as_bool().unwrap()
        );
    }

    #[test]
    fn oscillation_by_jump_matches_the_frontend() {
        let g = goldens();
        let osc = OscillationThresholds::default();
        let stats_for = |name: &str| -> Option<PalierStats> {
            let samples = samples_from(&g["windows"][name]);
            compute_palier_stats(&samples, 0.0, &osc)
        };
        let oscillating = stats_for("oscillating");
        let flat = stats_for("flat");
        assert!(
            oscillating.is_some() && flat.is_some(),
            "fixtures must produce stats"
        );

        let cases: Vec<(&str, Option<PalierStats>, Vec<Option<PalierStats>>)> = vec![
            ("jumpOverFlat", oscillating, vec![flat]),
            ("noJump", oscillating, vec![oscillating]),
            ("noBaseline", oscillating, vec![]),
            ("nullBaseline", oscillating, vec![None, None]),
            ("quietCurrent", flat, vec![flat]),
            ("noCurrent", None, vec![flat]),
        ];

        for (name, current, baselines) in cases {
            let verdict = detect_oscillation_by_jump(current.as_ref(), &baselines, &osc);
            let expected = &g["oscillationByJump"][name];
            assert_eq!(
                verdict.oscillating,
                expected["oscillating"].as_bool().unwrap(),
                "{name}.oscillating"
            );
            assert_eq!(verdict.reason, expected["reason"].as_str(), "{name}.reason");
            if expected.get("peakToPeakJump").is_some() {
                close_or_infinite(
                    verdict.peak_to_peak_jump,
                    &expected["peakToPeakJump"],
                    &format!("{name}.ppJump"),
                );
                close_or_infinite(
                    verdict.crossing_jump,
                    &expected["crossingJump"],
                    &format!("{name}.crJump"),
                );
            }
        }
    }
}
