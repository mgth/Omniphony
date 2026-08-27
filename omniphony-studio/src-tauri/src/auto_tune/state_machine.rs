//! The PI auto-tune state machine.
//!
//! A port of `src/auto-tune/state-machine.js`, driving the Ziegler-Nichols
//! procedure in `PI_TUNING_PROCEDURE.md`. Pure logic: it consumes telemetry and
//! returns events. Feeding it, and acting on what it returns, is the caller's
//! job.
//!
//! Correctness here is judged against the recorded runs in
//! `scripts/golden/auto-tune-runs.json`, not against my reading of the JS —
//! see `replay.rs`. Two implementations reaching the same final kp/ki by
//! different paths is not the same thing as a faithful port, so the test
//! compares the whole sequence of states and events.
//!
//! # Two things deliberately not copied
//!
//! **The clock.** The JS `start()` takes its time as an argument, but
//! `userAck` reads `Date.now()` in three places — for the perturbation start,
//! the long-run start, and the palier restart after a source loss. All three
//! are later compared against `sample.t`, so they only work because the runner
//! happens to timestamp samples with `Date.now()` too. Here every entry point
//! takes `now_ms`, which is what makes the machine replayable.
//!
//! **Implicit context fields.** `longRunCanAbbreviateEmitted` and
//! `lastLongRunEmitMs` are read before ever being assigned in the JS, so they
//! start as `undefined` and rely on falsy comparison. They are explicit here.

use super::detectors::{
    compute_palier_stats, compute_rate_stats, detect_convergence, detect_oscillation_absolute,
    detect_oscillation_by_jump, detect_saturation, detect_source_loss, error_ms,
    ConvergenceThresholds, JumpVerdict, OscillationThresholds, PalierStats, RateStats, Sample,
    SaturationThresholds, SourceLossThresholds,
};

/// Tuning constants. Mirrors `AUTO_TUNE_DEFAULTS`.
#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub initial_kp: f64,
    pub initial_max_adjust: f64,
    pub initial_update_interval: f64,
    pub kp_max: f64,
    pub kp_palier_ms: f64,
    pub kp_baseline_paliers: usize,
    pub ki_palier_ms: f64,
    pub ki_max_iterations: u32,
    pub ki_min: f64,
    pub perturbation_recover_ms: f64,
    pub long_run_default_ms: f64,
    pub long_run_min_abbreviate_ms: f64,
    pub long_run_stats_window_ms: f64,
    pub tightening_palier_ms: f64,
    pub zieger_kp_scale: f64,
    pub initial_ki_from_kp_divisor: f64,
    pub sample_retention_ms: f64,
    pub max_adjust_floor: f64,
    pub max_adjust_safety_margin: f64,
    pub max_adjust_warn_threshold: f64,
    pub update_interval_clean_std_ppm: f64,
    pub update_interval_clean: f64,
    pub update_interval_default: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            initial_kp: 1.0,
            initial_max_adjust: 0.10,
            initial_update_interval: 1.0,
            kp_max: 5000.0,
            kp_palier_ms: 30_000.0,
            kp_baseline_paliers: 3,
            ki_palier_ms: 60_000.0,
            ki_max_iterations: 4,
            ki_min: 1e-3,
            perturbation_recover_ms: 15_000.0,
            long_run_default_ms: 600_000.0,
            long_run_min_abbreviate_ms: 120_000.0,
            long_run_stats_window_ms: 120_000.0,
            tightening_palier_ms: 30_000.0,
            zieger_kp_scale: 0.6,
            initial_ki_from_kp_divisor: 5.0,
            sample_retention_ms: 600_000.0,
            max_adjust_floor: 0.02,
            max_adjust_safety_margin: 1.5,
            max_adjust_warn_threshold: 0.15,
            update_interval_clean_std_ppm: 50.0,
            update_interval_clean: 5.0,
            update_interval_default: 10.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    HoldKp,
    TuningKi,
    AwaitPerturbation,
    PerturbationRecovering,
    LongRun,
    Tightening,
    Suspended,
    Completed,
    Cancelled,
    Error,
}

impl State {
    /// Wire spelling, matching the JS state names the wizard already renders.
    pub fn as_str(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::HoldKp => "holdKp",
            State::TuningKi => "tuningKi",
            State::AwaitPerturbation => "awaitPerturbation",
            State::PerturbationRecovering => "perturbationRecovering",
            State::LongRun => "longRun",
            State::Tightening => "tightening",
            State::Suspended => "suspended",
            State::Completed => "completed",
            State::Cancelled => "cancelled",
            State::Error => "error",
        }
    }

    /// A run that has ended, one way or another.
    fn is_terminal(self) -> bool {
        matches!(self, State::Completed | State::Cancelled | State::Error)
    }
}

/// What the caller must act on. `ApplyParams` is the only one with a side
/// effect on the audio path; the rest are for display.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    /// A parameter change to send to the renderer. Absent fields are unchanged.
    ApplyParams {
        kp_near: Option<f64>,
        ki: Option<f64>,
        max_adjust: Option<f64>,
        update_interval_callbacks: Option<f64>,
    },
    Progress {
        step: &'static str,
        detail: serde_json::Value,
    },
    AwaitUserAction {
        kind: &'static str,
        detail: serde_json::Value,
    },
    SourceLost {
        events: u32,
    },
    SourceRecovered {
        restored_state: &'static str,
    },
    Complete(Box<TuningResult>),
    Cancelled,
    Error {
        kind: &'static str,
        detail: serde_json::Value,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TuningResult {
    pub kp_crit: Option<f64>,
    pub kp_final: Option<f64>,
    pub ki_final: Option<f64>,
    pub max_adjust_final: Option<f64>,
    pub update_interval_final: Option<f64>,
    pub tightening_oscillation: bool,
    pub tightening_converged: bool,
}

// ── payload shapes ──────────────────────────────────────────────────────────
//
// The wizard reads progress payloads field by field
// (`payload.palierStats?.peakToPeakPpm`, `payload.verdict?.reason`), so these
// are part of the contract, not decoration. Non-finite floats serialise to
// `null`, which is what `JSON.stringify` does with `Infinity` on the frontend.

fn palier_stats_json(s: &PalierStats) -> serde_json::Value {
    serde_json::json!({
        "peakToPeakPpm": s.peak_to_peak_ppm,
        "crossings": s.crossings,
        "crossingRate": s.crossing_rate,
        "meanPpm": s.mean_ppm,
        "samples": s.samples,
        "stableDurationMs": s.stable_duration_ms,
    })
}

fn rate_stats_json(s: &RateStats) -> serde_json::Value {
    serde_json::json!({
        "peakAbsPpm": s.peak_abs_ppm,
        "meanPpm": s.mean_ppm,
        "stdPpm": s.std_ppm,
        "samples": s.samples,
    })
}

/// The jump verdict as the frontend emits it: each rejection branch carries
/// only what it had computed by the time it gave up.
fn jump_verdict_json(
    v: &JumpVerdict,
    current: Option<&PalierStats>,
    baselines: &[Option<PalierStats>],
) -> serde_json::Value {
    let mut o = serde_json::Map::new();
    o.insert("oscillating".into(), serde_json::json!(v.oscillating));
    o.insert("reason".into(), serde_json::json!(v.reason));
    if let Some(c) = current {
        o.insert("currentStats".into(), palier_stats_json(c));
    }
    if v.reason == Some("baseline-too-short") {
        let kept: Vec<serde_json::Value> =
            baselines.iter().flatten().map(palier_stats_json).collect();
        o.insert("baselines".into(), serde_json::Value::Array(kept));
    }
    for (key, value) in [
        ("maxBaselinePeakToPeakPpm", v.max_baseline_peak_to_peak_ppm),
        ("maxBaselineCrossingRate", v.max_baseline_crossing_rate),
        ("peakToPeakJump", v.peak_to_peak_jump),
        ("crossingJump", v.crossing_jump),
    ] {
        if let Some(value) = value {
            o.insert(key.into(), serde_json::json!(value));
        }
    }
    serde_json::Value::Object(o)
}

/// What the user can ask for mid-run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ack {
    Perturbation,
    SkipPerturbation,
    ResumeAfterSourceLoss,
}

#[derive(Debug, Clone, Copy)]
struct PalierRecord {
    /// The kp this palier ran at. Only surfaced in the `no-oscillation`
    /// error payload, which reports the whole sweep.
    kp: f64,
    stats: Option<PalierStats>,
    saturated: bool,
}

#[derive(Debug, Default)]
struct Context {
    samples: Vec<Sample>,
    current_kp: f64,
    current_ki: f64,
    kp_crit: Option<f64>,
    kp_final: Option<f64>,
    ki_final: Option<f64>,
    max_adjust_final: Option<f64>,
    update_interval_final: Option<f64>,
    palier_start_ms: f64,
    ki_iteration: u32,
    best_ki: Option<f64>,
    best_ki_err: f64,
    long_run_start_ms: f64,
    long_run_duration_ms: f64,
    abbreviate_requested: bool,
    perturbation_start_ms: f64,
    suspended_from: Option<State>,
    kp_history: Vec<PalierRecord>,
    // Implicit in the JS; explicit here.
    long_run_can_abbreviate_emitted: bool,
    last_long_run_emit_ms: Option<f64>,
}

impl Context {
    fn fresh() -> Self {
        Self {
            best_ki_err: f64::INFINITY,
            ..Default::default()
        }
    }
}

/// A snapshot of the tuning values, for the caller to display.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Progress {
    pub current_kp: f64,
    pub current_ki: f64,
    pub kp_crit: Option<f64>,
    pub kp_final: Option<f64>,
    pub ki_final: Option<f64>,
    pub max_adjust_final: Option<f64>,
    pub update_interval_final: Option<f64>,
    pub ki_iteration: u32,
}

pub struct AutoTune {
    opts: Options,
    state: State,
    ctx: Context,
    osc: OscillationThresholds,
    sat: SaturationThresholds,
    conv: ConvergenceThresholds,
    loss: SourceLossThresholds,
}

impl AutoTune {
    pub fn new(opts: Options) -> Self {
        Self {
            opts,
            state: State::Idle,
            ctx: Context::fresh(),
            osc: OscillationThresholds::default(),
            sat: SaturationThresholds::default(),
            conv: ConvergenceThresholds::default(),
            loss: SourceLossThresholds::default(),
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn progress(&self) -> Progress {
        Progress {
            current_kp: self.ctx.current_kp,
            current_ki: self.ctx.current_ki,
            kp_crit: self.ctx.kp_crit,
            kp_final: self.ctx.kp_final,
            ki_final: self.ctx.ki_final,
            max_adjust_final: self.ctx.max_adjust_final,
            update_interval_final: self.ctx.update_interval_final,
            ki_iteration: self.ctx.ki_iteration,
        }
    }

    /// Begin a run. Refused unless idle or finished.
    pub fn start(&mut self, now_ms: f64) -> Vec<Event> {
        if !matches!(self.state, State::Idle) && !self.state.is_terminal() {
            return Vec::new();
        }
        self.ctx = Context::fresh();
        self.ctx.current_kp = self.opts.initial_kp;
        self.ctx.current_ki = 0.0;
        self.ctx.palier_start_ms = now_ms;
        let mut out = vec![Event::ApplyParams {
            kp_near: Some(self.opts.initial_kp),
            ki: Some(0.0),
            max_adjust: Some(self.opts.initial_max_adjust),
            update_interval_callbacks: Some(self.opts.initial_update_interval),
        }];
        self.set_state(
            State::HoldKp,
            serde_json::json!({ "currentKp": self.opts.initial_kp, "palier": 1 }),
            &mut out,
        );
        out
    }

    /// Feed one telemetry sample.
    pub fn push_sample(&mut self, sample: Sample) -> Vec<Event> {
        let mut out = Vec::new();
        if matches!(self.state, State::Idle) || self.state.is_terminal() {
            return out;
        }
        if !sample.t.is_finite() {
            return out;
        }

        self.ctx.samples.push(sample.clone());
        let cutoff = sample.t - self.opts.sample_retention_ms;
        // Retain from the first sample inside the window; the JS shifts one at
        // a time, which is the same set.
        if let Some(keep_from) = self.ctx.samples.iter().position(|s| s.t >= cutoff) {
            if keep_from > 0 {
                self.ctx.samples.drain(..keep_from);
            }
        }

        // Source-loss watchdog, skipped while the user is doing the manual
        // perturbation — the dropout is expected there.
        if !matches!(
            self.state,
            State::AwaitPerturbation | State::PerturbationRecovering | State::Suspended
        ) {
            let sl = detect_source_loss(&self.ctx.samples, &self.loss);
            if sl.lost {
                self.ctx.suspended_from = Some(self.state);
                self.state = State::Suspended;
                out.push(Event::SourceLost { events: sl.events });
                return out;
            }
        }

        match self.state {
            State::HoldKp => self.tick_hold_kp(&sample, &mut out),
            State::TuningKi => self.tick_tuning_ki(&sample, &mut out),
            State::PerturbationRecovering => self.tick_perturbation_recovering(&sample, &mut out),
            State::LongRun => self.tick_long_run(&sample, &mut out),
            State::Tightening => self.tick_tightening(&sample, &mut out),
            _ => {}
        }
        out
    }

    /// Answer a prompt. `now_ms` is explicit — see the module docs.
    pub fn user_ack(&mut self, kind: Ack, now_ms: f64) -> Vec<Event> {
        let mut out = Vec::new();
        match (kind, self.state) {
            (Ack::Perturbation, State::AwaitPerturbation) => {
                self.ctx.perturbation_start_ms = now_ms;
                self.ctx.samples.clear();
                self.set_state(
                    State::PerturbationRecovering,
                    serde_json::json!({}),
                    &mut out,
                );
            }
            (Ack::SkipPerturbation, State::AwaitPerturbation) => {
                self.ctx.ki_final = Some(self.ctx.current_ki);
                self.ctx.long_run_start_ms = now_ms;
                self.ctx.samples.clear();
                let detail = serde_json::json!({
                    "kpFinal": self.ctx.kp_final,
                    "kiFinal": self.ctx.ki_final,
                    "longRunTargetMs": self.opts.long_run_default_ms,
                    "skippedPerturbation": true,
                });
                self.set_state(State::LongRun, detail, &mut out);
            }
            (Ack::ResumeAfterSourceLoss, State::Suspended) => {
                let restored = self.ctx.suspended_from.take().unwrap_or(State::HoldKp);
                self.ctx.samples.clear();
                // Restart the palier from now, so pre-loss data cannot bias the
                // decision that ends it.
                self.ctx.palier_start_ms = now_ms;
                if matches!(restored, State::LongRun) {
                    self.ctx.long_run_start_ms = now_ms;
                }
                self.state = restored;
                out.push(Event::SourceRecovered {
                    restored_state: restored.as_str(),
                });
            }
            _ => {}
        }
        out
    }

    /// Ask for the long run to end as soon as it may.
    pub fn abbreviate(&mut self) -> bool {
        if !matches!(self.state, State::LongRun) {
            return false;
        }
        self.ctx.abbreviate_requested = true;
        true
    }

    pub fn cancel(&mut self) -> Vec<Event> {
        if self.state.is_terminal() {
            return Vec::new();
        }
        self.state = State::Cancelled;
        vec![Event::Cancelled]
    }

    // ── internals ───────────────────────────────────────────────────────────

    fn set_state(&mut self, next: State, detail: serde_json::Value, out: &mut Vec<Event>) {
        self.state = next;
        out.push(Event::Progress {
            step: next.as_str(),
            detail,
        });
    }

    fn tick_hold_kp(&mut self, sample: &Sample, out: &mut Vec<Event>) {
        if sample.t - self.ctx.palier_start_ms < self.opts.kp_palier_ms {
            return;
        }
        let palier_stats =
            compute_palier_stats(&self.ctx.samples, self.ctx.palier_start_ms, &self.osc);
        let sat = detect_saturation(&self.ctx.samples, self.opts.initial_max_adjust, &self.sat);
        // Baseline: the last few paliers that were neither saturated nor empty.
        // The JS takes `.slice(-N)` of the filtered list: the last N usable
        // paliers, still in order.
        let usable: Vec<Option<PalierStats>> = self
            .ctx
            .kp_history
            .iter()
            .filter(|p| !p.saturated && p.stats.is_some())
            .map(|p| p.stats)
            .collect();
        let start = usable.len().saturating_sub(self.opts.kp_baseline_paliers);
        let baselines = &usable[start..];
        let verdict = detect_oscillation_by_jump(palier_stats.as_ref(), &baselines, &self.osc);

        self.ctx.kp_history.push(PalierRecord {
            kp: self.ctx.current_kp,
            stats: palier_stats,
            saturated: sat.saturated,
        });

        if verdict.oscillating {
            let kp_crit = self.ctx.current_kp;
            let kp_final = self.opts.zieger_kp_scale * kp_crit;
            let initial_ki = kp_final / self.opts.initial_ki_from_kp_divisor;
            self.ctx.kp_crit = Some(kp_crit);
            self.ctx.kp_final = Some(kp_final);
            self.ctx.current_ki = initial_ki;
            self.ctx.ki_iteration = 0;
            self.ctx.best_ki = Some(initial_ki);
            self.ctx.best_ki_err = f64::INFINITY;
            out.push(Event::ApplyParams {
                kp_near: Some(kp_final),
                ki: Some(initial_ki),
                max_adjust: None,
                update_interval_callbacks: None,
            });
            self.restart_palier(sample);
            let detail = serde_json::json!({
                "kpCrit": kp_crit,
                "kpFinal": kp_final,
                "currentKi": initial_ki,
                "kiIteration": 0,
                "verdict": jump_verdict_json(&verdict, palier_stats.as_ref(), baselines),
            });
            self.set_state(State::TuningKi, detail, out);
            return;
        }

        let next_kp = self.ctx.current_kp * 2.0;
        if next_kp > self.opts.kp_max {
            self.state = State::Error;
            out.push(Event::Error {
                kind: "no-oscillation",
                detail: serde_json::json!({
                    "kpReached": self.ctx.current_kp,
                    "lastStats": palier_stats.as_ref().map(palier_stats_json),
                    "history": self
                        .ctx
                        .kp_history
                        .iter()
                        .map(|p| serde_json::json!({
                            "kp": p.kp,
                            "stats": p.stats.as_ref().map(palier_stats_json),
                            "saturated": p.saturated,
                        }))
                        .collect::<Vec<_>>(),
                }),
            });
            return;
        }
        self.ctx.current_kp = next_kp;
        self.restart_palier(sample);
        out.push(Event::ApplyParams {
            kp_near: Some(next_kp),
            ki: None,
            max_adjust: None,
            update_interval_callbacks: None,
        });
        out.push(Event::Progress {
            step: "holdKp",
            detail: serde_json::json!({
                "currentKp": next_kp,
                "saturated": sat.saturated,
                "palierStats": palier_stats.as_ref().map(palier_stats_json),
                "verdict": jump_verdict_json(&verdict, palier_stats.as_ref(), baselines),
            }),
        });
    }

    fn tick_tuning_ki(&mut self, sample: &Sample, out: &mut Vec<Event>) {
        if sample.t - self.ctx.palier_start_ms < self.opts.ki_palier_ms {
            return;
        }

        if detect_convergence(&self.ctx.samples, &self.conv).converged {
            self.ctx.ki_final = Some(self.ctx.current_ki);
            self.await_perturbation(serde_json::json!({}), out);
            return;
        }

        // Out of iterations: settle on the best ki seen rather than whatever
        // the last step happened to leave.
        if self.ctx.ki_iteration >= self.opts.ki_max_iterations {
            self.ctx.ki_final = Some(self.ctx.best_ki.unwrap_or(self.ctx.current_ki));
            self.await_perturbation(serde_json::json!({ "hitIterationCap": true }), out);
            return;
        }

        // Compare the error in the two halves of the palier: still improving,
        // stalled, or diverging.
        let half = self.ctx.palier_start_ms + self.opts.ki_palier_ms / 2.0;
        let first_half = mean_abs_err(&self.ctx.samples, self.ctx.palier_start_ms, half);
        let second_half = mean_abs_err(&self.ctx.samples, half, sample.t);
        let second_peak = peak_abs_err(&self.ctx.samples, half, sample.t);

        // A peak well above the mean is ringing, not noise.
        let overshoot =
            matches!((second_peak, second_half), (Some(p), Some(m)) if p > 2.0 * m && p > 1.0);
        let improving = matches!((first_half, second_half), (Some(f), Some(s)) if s < f * 0.8);
        let worsening = matches!((first_half, second_half), (Some(f), Some(s)) if s > f);

        let (next_ki, reason) = if overshoot || worsening {
            (
                self.ctx.current_ki / 2.0,
                if overshoot { "overshoot" } else { "diverging" },
            )
        } else if !improving {
            (self.ctx.current_ki * 2.0, "too-slow")
        } else {
            (self.ctx.current_ki * 2.0, "still-converging")
        };

        if next_ki < self.opts.ki_min {
            self.ctx.ki_final = Some(self.ctx.best_ki.unwrap_or(self.ctx.current_ki));
            self.await_perturbation(serde_json::json!({ "kiCollapsed": true }), out);
            return;
        }

        if let Some(s) = second_half {
            if s < self.ctx.best_ki_err {
                self.ctx.best_ki_err = s;
                self.ctx.best_ki = Some(self.ctx.current_ki);
            }
        }

        self.ctx.ki_iteration += 1;
        self.ctx.current_ki = next_ki;
        self.restart_palier(sample);
        out.push(Event::ApplyParams {
            kp_near: None,
            ki: Some(next_ki),
            max_adjust: None,
            update_interval_callbacks: None,
        });
        out.push(Event::Progress {
            step: "tuningKi",
            detail: serde_json::json!({
                "currentKi": next_ki,
                "kiIteration": self.ctx.ki_iteration,
                "reason": reason,
                "firstHalfMeanErr": first_half,
                "secondHalfMeanErr": second_half,
            }),
        });
    }

    fn tick_perturbation_recovering(&mut self, sample: &Sample, out: &mut Vec<Event>) {
        if sample.t - self.ctx.perturbation_start_ms < self.opts.perturbation_recover_ms {
            return;
        }
        let osc = detect_oscillation_absolute(
            &self.ctx.samples,
            self.ctx.perturbation_start_ms,
            &self.osc,
        );
        if osc.oscillating {
            // The recovery left the loop ringing: back ki off and re-tune.
            self.ctx.current_ki *= 0.7;
            self.ctx.ki_iteration = self.opts.ki_max_iterations.saturating_sub(1);
            self.restart_palier(sample);
            out.push(Event::ApplyParams {
                kp_near: None,
                ki: Some(self.ctx.current_ki),
                max_adjust: None,
                update_interval_callbacks: None,
            });
            let detail = serde_json::json!({
                "currentKi": self.ctx.current_ki,
                "reason": "perturbation-oscillation",
            });
            self.set_state(State::TuningKi, detail, out);
            return;
        }
        self.ctx.ki_final = Some(self.ctx.current_ki);
        self.ctx.long_run_start_ms = sample.t;
        self.ctx.samples = vec![sample.clone()];
        let detail = serde_json::json!({
            "kpFinal": self.ctx.kp_final,
            "kiFinal": self.ctx.ki_final,
            "longRunTargetMs": self.opts.long_run_default_ms,
        });
        self.set_state(State::LongRun, detail, out);
    }

    fn tick_long_run(&mut self, sample: &Sample, out: &mut Vec<Event>) {
        let elapsed = sample.t - self.ctx.long_run_start_ms;
        let can_abbreviate = elapsed >= self.opts.long_run_min_abbreviate_ms;
        let reached = elapsed >= self.opts.long_run_default_ms;

        if can_abbreviate && !self.ctx.long_run_can_abbreviate_emitted {
            self.ctx.long_run_can_abbreviate_emitted = true;
            out.push(Event::Progress {
                step: "longRun",
                detail: serde_json::json!({ "canAbbreviate": true, "elapsedMs": elapsed }),
            });
        }

        if reached || (can_abbreviate && self.ctx.abbreviate_requested) {
            self.finish_long_run(sample, out);
            return;
        }

        // A light heartbeat, so the wizard shows the run advancing.
        let due = self
            .ctx
            .last_long_run_emit_ms
            .is_none_or(|last| sample.t - last > 5000.0);
        if due {
            self.ctx.last_long_run_emit_ms = Some(sample.t);
            out.push(Event::Progress {
                step: "longRun",
                detail: serde_json::json!({ "elapsedMs": elapsed }),
            });
        }
    }

    fn finish_long_run(&mut self, sample: &Sample, out: &mut Vec<Event>) {
        let stats: RateStats =
            compute_rate_stats(&self.ctx.samples, Some(self.opts.long_run_stats_window_ms));
        // Size the adjustment ceiling from the excursion actually observed,
        // with a margin, but never below the floor.
        let raw_max = (stats.peak_abs_ppm * self.opts.max_adjust_safety_margin) / 1e6;
        let max_adjust = raw_max.max(self.opts.max_adjust_floor);
        let update_interval = if stats.std_ppm < self.opts.update_interval_clean_std_ppm {
            self.opts.update_interval_clean
        } else {
            self.opts.update_interval_default
        };
        self.ctx.max_adjust_final = Some(max_adjust);
        self.ctx.update_interval_final = Some(update_interval);
        self.ctx.long_run_duration_ms = sample.t - self.ctx.long_run_start_ms;
        out.push(Event::ApplyParams {
            kp_near: None,
            ki: None,
            max_adjust: Some(max_adjust),
            update_interval_callbacks: Some(update_interval),
        });
        self.restart_palier(sample);
        let detail = serde_json::json!({
            "maxAdjustFinal": max_adjust,
            "updateIntervalFinal": update_interval,
            "maxAdjustWarn": max_adjust > self.opts.max_adjust_warn_threshold,
            "rateStats": rate_stats_json(&stats),
        });
        self.set_state(State::Tightening, detail, out);
    }

    fn tick_tightening(&mut self, sample: &Sample, out: &mut Vec<Event>) {
        if sample.t - self.ctx.palier_start_ms < self.opts.tightening_palier_ms {
            return;
        }
        let osc =
            detect_oscillation_absolute(&self.ctx.samples, self.ctx.palier_start_ms, &self.osc);
        let conv = detect_convergence(&self.ctx.samples, &self.conv);
        self.state = State::Completed;
        out.push(Event::Complete(Box::new(TuningResult {
            kp_crit: self.ctx.kp_crit,
            kp_final: self.ctx.kp_final,
            ki_final: self.ctx.ki_final,
            max_adjust_final: self.ctx.max_adjust_final,
            update_interval_final: self.ctx.update_interval_final,
            tightening_oscillation: osc.oscillating,
            tightening_converged: conv.converged,
        })));
    }

    /// Begin a fresh palier at this sample, discarding what came before: the
    /// previous regime's data must not influence the next one's verdict.
    fn restart_palier(&mut self, sample: &Sample) {
        self.ctx.palier_start_ms = sample.t;
        self.ctx.samples = vec![sample.clone()];
    }

    fn await_perturbation(&mut self, extra: serde_json::Value, out: &mut Vec<Event>) {
        let mut detail = serde_json::json!({
            "kpFinal": self.ctx.kp_final,
            "kiFinal": self.ctx.ki_final,
        });
        if let (Some(base), Some(more)) = (detail.as_object_mut(), extra.as_object()) {
            for (k, v) in more {
                base.insert(k.clone(), v.clone());
            }
        }
        self.set_state(State::AwaitPerturbation, detail.clone(), out);
        out.push(Event::AwaitUserAction {
            kind: "perturbation",
            detail,
        });
    }
}

/// Mean absolute latency error over `[from_ms, to_ms]`, or `None` when the
/// span holds no usable sample.
fn mean_abs_err(samples: &[Sample], from_ms: f64, to_ms: f64) -> Option<f64> {
    let mut sum = 0.0;
    let mut n = 0usize;
    for s in samples {
        if s.t < from_ms || s.t > to_ms {
            continue;
        }
        let Some(e) = error_ms(s) else { continue };
        sum += e.abs();
        n += 1;
    }
    (n > 0).then(|| sum / n as f64)
}

/// Largest absolute latency error over `[from_ms, to_ms]`.
fn peak_abs_err(samples: &[Sample], from_ms: f64, to_ms: f64) -> Option<f64> {
    let mut peak = 0.0f64;
    let mut found = false;
    for s in samples {
        if s.t < from_ms || s.t > to_ms {
            continue;
        }
        let Some(e) = error_ms(s) else { continue };
        peak = peak.max(e.abs());
        found = true;
    }
    found.then_some(peak)
}
