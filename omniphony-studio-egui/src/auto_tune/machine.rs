//! The auto-tune state machine (`auto-tune/state-machine.js`).
//!
//! Pure logic: it is fed telemetry and hands back events, and knows nothing of
//! the renderer, the clock or the UI. The procedure is the one written down in
//! `PI_TUNING_PROCEDURE.md` — raise the proportional gain palier by palier
//! until the loop rings, back off by the Ziegler-Nichols factor, bring the
//! integral term in, ask for a disturbance, then run long enough to size the
//! rate limit from what the link actually needed.
//!
//! It touches kp, ki, the rate limit and the callback interval, and nothing
//! else. In particular it never patches the integral discharge ratio, which
//! does nothing on this hardware and would only add a knob to the report.

use super::detectors::{
    Convergence, Oscillation, PalierStats, RateStats, Sample, Saturation, SourceLoss,
    compute_palier_stats, compute_rate_stats, detect_convergence, detect_oscillation_absolute,
    detect_oscillation_by_jump, detect_saturation, detect_source_loss, error_ms,
};

/// `AUTO_TUNE_DEFAULTS`.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub initial_kp: f64,
    pub initial_max_adjust: f64,
    pub initial_update_interval: u32,
    /// The sweep gives up here rather than doubling for ever.
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
    /// Ziegler-Nichols: the usable gain is a fraction of the critical one.
    pub zieger_kp_scale: f64,
    pub initial_ki_from_kp_divisor: f64,
    pub sample_retention_ms: f64,
    pub max_adjust_floor: f64,
    pub max_adjust_safety_margin: f64,
    pub max_adjust_warn_threshold: f64,
    pub update_interval_clean_std_ppm: f64,
    pub update_interval_clean: u32,
    pub update_interval_default: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            initial_kp: 1.0,
            // Enough to clear the drift a cold link starts with; the long run
            // is what sizes the limit that ships.
            initial_max_adjust: 0.10,
            initial_update_interval: 1,
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
            update_interval_clean: 5,
            update_interval_default: 10,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    #[default]
    Idle,
    /// Holding one palier of the kp sweep.
    HoldKp,
    TuningKi,
    /// Waiting for the user to disturb the link.
    AwaitPerturbation,
    PerturbationRecovering,
    LongRun,
    /// The final palier, run with the limits the long run produced.
    Tightening,
    /// The source went away; the run holds until it is back.
    Suspended,
    Completed,
    Cancelled,
    Failed,
}

/// A patch to the renderer's controller. Only the fields that changed are set.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Patch {
    pub kp_near: Option<f64>,
    pub ki: Option<f64>,
    pub max_adjust: Option<f64>,
    pub update_interval_callbacks: Option<u32>,
}

/// Why the machine moved, when the move itself does not say it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Note {
    /// The palier's second half was worse than its first.
    Diverging,
    /// Peaks well above the mean: the loop is overshooting.
    Overshoot,
    TooSlow,
    StillConverging,
    /// The iteration budget ran out; the best ki seen is the one kept.
    HitIterationCap,
    /// Halving would take ki under its floor.
    KiCollapsed,
    /// The recovery left the loop ringing.
    PerturbationOscillation,
    SkippedPerturbation,
    /// The limit the long run sized is high enough to be worth saying.
    MaxAdjustWarn,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Progress {
    pub step: State,
    pub note: Option<Note>,
    /// Milliseconds into the current phase, where that is what is being
    /// waited on.
    pub elapsed_ms: Option<f64>,
    /// The long run has gone on long enough to be cut short.
    pub can_abbreviate: bool,
}

/// What the run settled on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Outcome {
    pub kp_crit: Option<f64>,
    pub kp_final: Option<f64>,
    pub ki_final: Option<f64>,
    pub max_adjust_final: Option<f64>,
    pub update_interval_final: Option<u32>,
    pub tightening_oscillation: bool,
    pub tightening_converged: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Failure {
    /// The sweep reached its ceiling without the loop ever ringing.
    NoOscillation { kp_reached: f64 },
}

/// What the caller has to act on.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    ApplyParams(Patch),
    Progress(Progress),
    /// The run needs the user to do something before it can go on.
    AwaitUserAction(Ack),
    SourceLost {
        events: u32,
    },
    SourceRecovered {
        restored: State,
    },
    Complete(Outcome),
    Cancelled,
    Failed(Failure),
}

/// The acknowledgements the run understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ack {
    /// "I have disturbed the link" — start timing the recovery.
    Perturbation,
    /// "Skip that step."
    SkipPerturbation,
    /// "The source is back."
    ResumeAfterSourceLoss,
}

/// The numbers a UI reports while the run is going.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Context {
    pub current_kp: f64,
    pub current_ki: f64,
    pub kp_crit: Option<f64>,
    pub kp_final: Option<f64>,
    pub ki_final: Option<f64>,
    pub max_adjust_final: Option<f64>,
    pub update_interval_final: Option<u32>,
    pub ki_iteration: u32,
    pub palier: u32,
}

struct KpPalier {
    stats: Option<PalierStats>,
    saturated: bool,
}

pub struct AutoTune {
    settings: Settings,
    oscillation: Oscillation,
    saturation: Saturation,
    convergence: Convergence,
    source_loss: SourceLoss,
    state: State,
    samples: Vec<Sample>,
    ctx: Context,
    palier_start_ms: f64,
    best_ki: Option<f64>,
    best_ki_err: f64,
    long_run_start_ms: f64,
    long_run_can_abbreviate_emitted: bool,
    last_long_run_emit_ms: Option<f64>,
    abbreviate_requested: bool,
    perturbation_start_ms: f64,
    suspended_from: Option<State>,
    kp_history: Vec<KpPalier>,
    /// What the last long run measured, kept for the report.
    pub rate_stats: Option<RateStats>,
}

impl Default for AutoTune {
    fn default() -> Self {
        Self::new(Settings::default())
    }
}

impl AutoTune {
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            oscillation: Oscillation::default(),
            saturation: Saturation::default(),
            convergence: Convergence::default(),
            source_loss: SourceLoss::default(),
            state: State::Idle,
            samples: Vec::new(),
            ctx: Context::default(),
            palier_start_ms: 0.0,
            best_ki: None,
            best_ki_err: f64::INFINITY,
            long_run_start_ms: 0.0,
            long_run_can_abbreviate_emitted: false,
            last_long_run_emit_ms: None,
            abbreviate_requested: false,
            perturbation_start_ms: 0.0,
            suspended_from: None,
            kp_history: Vec::new(),
            rate_stats: None,
        }
    }

    pub fn state(&self) -> State {
        self.state
    }

    pub fn context(&self) -> Context {
        self.ctx
    }

    /// Begin a run. Returns the events it starts with, or nothing when a run
    /// is already going.
    pub fn start(&mut self, now_ms: f64) -> Vec<Event> {
        if !matches!(
            self.state,
            State::Idle | State::Cancelled | State::Completed | State::Failed
        ) {
            return Vec::new();
        }
        let settings = self.settings;
        *self = Self::new(settings);
        self.ctx.current_kp = settings.initial_kp;
        self.ctx.palier = 1;
        self.palier_start_ms = now_ms;
        self.state = State::HoldKp;
        vec![
            Event::ApplyParams(Patch {
                kp_near: Some(settings.initial_kp),
                ki: Some(0.0),
                max_adjust: Some(settings.initial_max_adjust),
                update_interval_callbacks: Some(settings.initial_update_interval),
            }),
            Event::Progress(Progress {
                step: State::HoldKp,
                note: None,
                elapsed_ms: Some(0.0),
                can_abbreviate: false,
            }),
        ]
    }

    /// Feed one telemetry sample.
    pub fn push_sample(&mut self, sample: Sample) -> Vec<Event> {
        if matches!(
            self.state,
            State::Idle | State::Cancelled | State::Failed | State::Completed
        ) {
            return Vec::new();
        }
        self.samples.push(sample);
        let cutoff = sample.t - self.settings.sample_retention_ms;
        let keep = self.samples.partition_point(|s| s.t < cutoff);
        self.samples.drain(..keep);

        // The source-loss watchdog, except where an absent source is the point
        // of the step: the perturbation *is* the user taking the source away.
        if !matches!(
            self.state,
            State::AwaitPerturbation | State::PerturbationRecovering | State::Suspended
        ) {
            let (lost, events) = detect_source_loss(&self.samples, &self.source_loss);
            if lost {
                self.suspended_from = Some(self.state);
                self.state = State::Suspended;
                return vec![Event::SourceLost { events }];
            }
        }
        match self.state {
            State::HoldKp => self.tick_hold_kp(sample),
            State::TuningKi => self.tick_tuning_ki(sample),
            State::PerturbationRecovering => self.tick_perturbation_recovering(sample),
            State::LongRun => self.tick_long_run(sample),
            State::Tightening => self.tick_tightening(sample),
            _ => Vec::new(),
        }
    }

    fn restart_palier(&mut self, sample: Sample) {
        self.palier_start_ms = sample.t;
        self.samples = vec![sample];
    }

    fn tick_hold_kp(&mut self, sample: Sample) -> Vec<Event> {
        if sample.t - self.palier_start_ms < self.settings.kp_palier_ms {
            return Vec::new();
        }
        let stats = compute_palier_stats(&self.samples, self.palier_start_ms, &self.oscillation);
        let (saturated, _) = detect_saturation(
            &self.samples,
            self.settings.initial_max_adjust,
            &self.saturation,
        );
        // A saturated palier says nothing about the loop's own behaviour: the
        // limit was doing the moving, so it is no baseline.
        let baselines: Vec<PalierStats> = self
            .kp_history
            .iter()
            .filter_map(|p| (!p.saturated).then_some(p.stats).flatten())
            .rev()
            .take(self.settings.kp_baseline_paliers)
            .collect();
        let verdict = detect_oscillation_by_jump(stats.as_ref(), &baselines, &self.oscillation);
        self.kp_history.push(KpPalier { stats, saturated });

        if verdict.is_ok() {
            let kp_crit = self.ctx.current_kp;
            let kp_final = self.settings.zieger_kp_scale * kp_crit;
            let ki = kp_final / self.settings.initial_ki_from_kp_divisor;
            self.ctx.kp_crit = Some(kp_crit);
            self.ctx.kp_final = Some(kp_final);
            self.ctx.current_ki = ki;
            self.ctx.ki_iteration = 0;
            self.best_ki = Some(ki);
            self.best_ki_err = f64::INFINITY;
            self.restart_palier(sample);
            self.state = State::TuningKi;
            return vec![
                Event::ApplyParams(Patch {
                    kp_near: Some(kp_final),
                    ki: Some(ki),
                    ..Default::default()
                }),
                Event::Progress(Progress {
                    step: State::TuningKi,
                    note: None,
                    elapsed_ms: Some(0.0),
                    can_abbreviate: false,
                }),
            ];
        }

        let next_kp = self.ctx.current_kp * 2.0;
        if next_kp > self.settings.kp_max {
            self.state = State::Failed;
            return vec![Event::Failed(Failure::NoOscillation {
                kp_reached: self.ctx.current_kp,
            })];
        }
        self.ctx.current_kp = next_kp;
        self.ctx.palier += 1;
        self.restart_palier(sample);
        vec![
            Event::ApplyParams(Patch {
                kp_near: Some(next_kp),
                ..Default::default()
            }),
            Event::Progress(Progress {
                step: State::HoldKp,
                note: None,
                elapsed_ms: Some(0.0),
                can_abbreviate: false,
            }),
        ]
    }

    fn await_perturbation(&mut self, note: Option<Note>) -> Vec<Event> {
        self.state = State::AwaitPerturbation;
        vec![
            Event::Progress(Progress {
                step: State::AwaitPerturbation,
                note,
                elapsed_ms: None,
                can_abbreviate: false,
            }),
            Event::AwaitUserAction(Ack::Perturbation),
        ]
    }

    fn tick_tuning_ki(&mut self, sample: Sample) -> Vec<Event> {
        if sample.t - self.palier_start_ms < self.settings.ki_palier_ms {
            return Vec::new();
        }
        if detect_convergence(&self.samples, &self.convergence).converged {
            self.ctx.ki_final = Some(self.ctx.current_ki);
            return self.await_perturbation(None);
        }
        if self.ctx.ki_iteration >= self.settings.ki_max_iterations {
            self.ctx.ki_final = Some(self.best_ki.unwrap_or(self.ctx.current_ki));
            return self.await_perturbation(Some(Note::HitIterationCap));
        }

        // Is the second half of the palier better than the first? That is the
        // whole heuristic: a term that is helping shows up as an error that
        // keeps shrinking, one that is too strong shows up as peaks.
        let half = self.palier_start_ms + self.settings.ki_palier_ms / 2.0;
        let first = mean_abs_err(&self.samples, self.palier_start_ms, half);
        let second = mean_abs_err(&self.samples, half, sample.t);
        let peak = peak_abs_err(&self.samples, half, sample.t);
        let overshoot = matches!((peak, second), (Some(p), Some(m)) if p > 2.0 * m && p > 1.0);
        let improving = matches!((first, second), (Some(f), Some(s)) if s < f * 0.8);
        let worse = matches!((first, second), (Some(f), Some(s)) if s > f);

        let (next_ki, note) = if overshoot {
            (self.ctx.current_ki / 2.0, Note::Overshoot)
        } else if worse {
            (self.ctx.current_ki / 2.0, Note::Diverging)
        } else if improving {
            (self.ctx.current_ki * 2.0, Note::StillConverging)
        } else {
            (self.ctx.current_ki * 2.0, Note::TooSlow)
        };

        if next_ki < self.settings.ki_min {
            self.ctx.ki_final = Some(self.best_ki.unwrap_or(self.ctx.current_ki));
            return self.await_perturbation(Some(Note::KiCollapsed));
        }
        if let Some(second) = second
            && second < self.best_ki_err
        {
            self.best_ki_err = second;
            self.best_ki = Some(self.ctx.current_ki);
        }
        self.ctx.ki_iteration += 1;
        self.ctx.current_ki = next_ki;
        self.restart_palier(sample);
        vec![
            Event::ApplyParams(Patch {
                ki: Some(next_ki),
                ..Default::default()
            }),
            Event::Progress(Progress {
                step: State::TuningKi,
                note: Some(note),
                elapsed_ms: Some(0.0),
                can_abbreviate: false,
            }),
        ]
    }

    fn tick_perturbation_recovering(&mut self, sample: Sample) -> Vec<Event> {
        if sample.t - self.perturbation_start_ms < self.settings.perturbation_recover_ms {
            return Vec::new();
        }
        // No sweep behind this step, so the absolute floors are all there is
        // to judge it by.
        let ringing = detect_oscillation_absolute(
            &self.samples,
            self.perturbation_start_ms,
            &self.oscillation,
        )
        .is_ok();
        if ringing {
            self.ctx.current_ki *= 0.7;
            // Back to the ki loop with one iteration left: this is a
            // correction, not a fresh search.
            self.ctx.ki_iteration = self.settings.ki_max_iterations.saturating_sub(1);
            self.restart_palier(sample);
            self.state = State::TuningKi;
            return vec![
                Event::ApplyParams(Patch {
                    ki: Some(self.ctx.current_ki),
                    ..Default::default()
                }),
                Event::Progress(Progress {
                    step: State::TuningKi,
                    note: Some(Note::PerturbationOscillation),
                    elapsed_ms: Some(0.0),
                    can_abbreviate: false,
                }),
            ];
        }
        self.ctx.ki_final = Some(self.ctx.current_ki);
        self.long_run_start_ms = sample.t;
        self.samples = vec![sample];
        self.state = State::LongRun;
        vec![Event::Progress(Progress {
            step: State::LongRun,
            note: None,
            elapsed_ms: Some(0.0),
            can_abbreviate: false,
        })]
    }

    fn tick_long_run(&mut self, sample: Sample) -> Vec<Event> {
        let elapsed = sample.t - self.long_run_start_ms;
        let can_abbreviate = elapsed >= self.settings.long_run_min_abbreviate_ms;
        let mut events = Vec::new();
        if can_abbreviate && !self.long_run_can_abbreviate_emitted {
            self.long_run_can_abbreviate_emitted = true;
            events.push(Event::Progress(Progress {
                step: State::LongRun,
                note: None,
                elapsed_ms: Some(elapsed),
                can_abbreviate: true,
            }));
        }
        if elapsed >= self.settings.long_run_default_ms
            || (can_abbreviate && self.abbreviate_requested)
        {
            events.extend(self.finish_long_run(sample));
            return events;
        }
        if self
            .last_long_run_emit_ms
            .is_none_or(|at| sample.t - at > 5000.0)
        {
            self.last_long_run_emit_ms = Some(sample.t);
            events.push(Event::Progress(Progress {
                step: State::LongRun,
                note: None,
                elapsed_ms: Some(elapsed),
                can_abbreviate,
            }));
        }
        events
    }

    /// Size the rate limit from what the link actually needed, with a margin,
    /// and never below the floor: a limit tighter than the drift would leave
    /// the controller unable to catch up.
    fn finish_long_run(&mut self, sample: Sample) -> Vec<Event> {
        let stats = compute_rate_stats(&self.samples, Some(self.settings.long_run_stats_window_ms));
        let max_adjust = (stats.peak_abs_ppm * self.settings.max_adjust_safety_margin / 1e6)
            .max(self.settings.max_adjust_floor);
        // A quiet link can be corrected less often, which costs less.
        let update_interval = if stats.std_ppm < self.settings.update_interval_clean_std_ppm {
            self.settings.update_interval_clean
        } else {
            self.settings.update_interval_default
        };
        self.ctx.max_adjust_final = Some(max_adjust);
        self.ctx.update_interval_final = Some(update_interval);
        self.rate_stats = Some(stats);
        self.restart_palier(sample);
        self.state = State::Tightening;
        vec![
            Event::ApplyParams(Patch {
                max_adjust: Some(max_adjust),
                update_interval_callbacks: Some(update_interval),
                ..Default::default()
            }),
            Event::Progress(Progress {
                step: State::Tightening,
                note: (max_adjust > self.settings.max_adjust_warn_threshold)
                    .then_some(Note::MaxAdjustWarn),
                elapsed_ms: Some(0.0),
                can_abbreviate: false,
            }),
        ]
    }

    fn tick_tightening(&mut self, sample: Sample) -> Vec<Event> {
        if sample.t - self.palier_start_ms < self.settings.tightening_palier_ms {
            return Vec::new();
        }
        let oscillating =
            detect_oscillation_absolute(&self.samples, self.palier_start_ms, &self.oscillation)
                .is_ok();
        let converged = detect_convergence(&self.samples, &self.convergence).converged;
        self.state = State::Completed;
        vec![Event::Complete(Outcome {
            kp_crit: self.ctx.kp_crit,
            kp_final: self.ctx.kp_final,
            ki_final: self.ctx.ki_final,
            max_adjust_final: self.ctx.max_adjust_final,
            update_interval_final: self.ctx.update_interval_final,
            tightening_oscillation: oscillating,
            tightening_converged: converged,
        })]
    }

    /// Answer a question the run asked. `now_ms` is the caller's clock, so the
    /// machine stays free of one.
    pub fn user_ack(&mut self, ack: Ack, now_ms: f64) -> Vec<Event> {
        match (ack, self.state) {
            (Ack::Perturbation, State::AwaitPerturbation) => {
                self.perturbation_start_ms = now_ms;
                self.samples.clear();
                self.state = State::PerturbationRecovering;
                vec![Event::Progress(Progress {
                    step: State::PerturbationRecovering,
                    note: None,
                    elapsed_ms: Some(0.0),
                    can_abbreviate: false,
                })]
            }
            (Ack::SkipPerturbation, State::AwaitPerturbation) => {
                self.ctx.ki_final = Some(self.ctx.current_ki);
                self.long_run_start_ms = now_ms;
                self.samples.clear();
                self.state = State::LongRun;
                vec![Event::Progress(Progress {
                    step: State::LongRun,
                    note: Some(Note::SkippedPerturbation),
                    elapsed_ms: Some(0.0),
                    can_abbreviate: false,
                })]
            }
            (Ack::ResumeAfterSourceLoss, State::Suspended) => {
                let Some(restored) = self.suspended_from.take() else {
                    return Vec::new();
                };
                // The palier starts again from now: samples taken while the
                // source was away would decide the step on the outage.
                self.samples.clear();
                self.palier_start_ms = now_ms;
                if restored == State::LongRun {
                    self.long_run_start_ms = now_ms;
                }
                self.state = restored;
                vec![Event::SourceRecovered { restored }]
            }
            _ => Vec::new(),
        }
    }

    /// Ask for the long run to stop as soon as it may.
    pub fn abbreviate(&mut self) -> bool {
        if self.state != State::LongRun {
            return false;
        }
        self.abbreviate_requested = true;
        true
    }

    pub fn cancel(&mut self) -> Vec<Event> {
        if matches!(
            self.state,
            State::Cancelled | State::Completed | State::Failed | State::Idle
        ) {
            return Vec::new();
        }
        self.state = State::Cancelled;
        vec![Event::Cancelled]
    }
}

fn mean_abs_err(samples: &[Sample], from_ms: f64, to_ms: f64) -> Option<f64> {
    let mut sum = 0.0;
    let mut n = 0usize;
    for sample in samples.iter().filter(|s| s.t >= from_ms && s.t <= to_ms) {
        if let Some(err) = error_ms(sample) {
            sum += err.abs();
            n += 1;
        }
    }
    (n > 0).then(|| sum / n as f64)
}

fn peak_abs_err(samples: &[Sample], from_ms: f64, to_ms: f64) -> Option<f64> {
    samples
        .iter()
        .filter(|s| s.t >= from_ms && s.t <= to_ms)
        .filter_map(error_ms)
        .map(f64::abs)
        .reduce(f64::max)
}

#[cfg(test)]
mod tests {
    use super::super::detectors::Phase;
    use super::*;

    /// A feed the run can be driven with: a quiet link that starts ringing
    /// once kp passes `rings_at`, sampled every 100 ms.
    struct Link {
        t: f64,
        kp: f64,
        rings_at: f64,
        target_ms: f64,
        error_ms: f64,
    }

    impl Link {
        fn new(rings_at: f64) -> Self {
            Self {
                t: 0.0,
                kp: 1.0,
                rings_at,
                target_ms: 40.0,
                error_ms: 0.0,
            }
        }

        fn sample(&mut self) -> Sample {
            let ringing = self.kp >= self.rings_at;
            let hz = if ringing { 2.0 } else { 0.5 };
            let amplitude = if ringing { 4000.0 } else { 300.0 };
            let phase = 2.0 * std::f64::consts::PI * hz * self.t / 1000.0;
            let sample = Sample {
                t: self.t,
                latency_smoothed_ms: Some(self.target_ms + self.error_ms),
                latency_target_ms: Some(self.target_ms),
                resample_ratio: Some(1.0 + amplitude * phase.sin() / 1e6),
                phase: Phase::Other,
            };
            self.t += 100.0;
            sample
        }

        /// Feed the machine for `ms`, following every gain it applies.
        fn run(&mut self, machine: &mut AutoTune, ms: f64) -> Vec<Event> {
            let until = self.t + ms;
            let mut events = Vec::new();
            while self.t <= until {
                let sample = self.sample();
                for event in machine.push_sample(sample) {
                    if let Event::ApplyParams(patch) = &event
                        && let Some(kp) = patch.kp_near
                    {
                        self.kp = kp;
                    }
                    events.push(event);
                }
            }
            events
        }
    }

    fn applied(events: &[Event]) -> Vec<Patch> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::ApplyParams(patch) => Some(*patch),
                _ => None,
            })
            .collect()
    }

    /// The sweep doubles the gain palier by palier, and the palier that rings
    /// is the critical one — after which the gain that ships is the
    /// Ziegler-Nichols fraction of it, and the integral term starts from a
    /// fifth of that.
    #[test]
    fn the_sweep_doubles_until_the_loop_rings_then_backs_off() {
        let mut machine = AutoTune::default();
        let mut link = Link::new(8.0);
        let start = machine.start(0.0);
        assert_eq!(machine.state(), State::HoldKp);
        assert_eq!(
            applied(&start)[0].kp_near,
            Some(Settings::default().initial_kp)
        );
        // Four paliers of thirty seconds: 1 → 2 → 4 → 8, and 8 rings.
        link.run(&mut machine, 4.0 * 31_000.0);
        assert_eq!(machine.state(), State::TuningKi);
        let ctx = machine.context();
        assert_eq!(ctx.kp_crit, Some(8.0));
        assert_eq!(ctx.kp_final, Some(0.6 * 8.0));
        assert!((ctx.current_ki - 0.6 * 8.0 / 5.0).abs() < 1e-9);
    }

    /// A loop that never rings is a failed run, not one that doubles the gain
    /// for ever.
    #[test]
    fn a_link_that_never_rings_ends_the_run() {
        let settings = Settings {
            kp_max: 4.0,
            ..Default::default()
        };
        let mut machine = AutoTune::new(settings);
        let mut link = Link::new(f64::INFINITY);
        machine.start(0.0);
        let events = link.run(&mut machine, 4.0 * 31_000.0);
        assert_eq!(machine.state(), State::Failed);
        assert!(matches!(
            events.last(),
            Some(Event::Failed(Failure::NoOscillation { kp_reached })) if *kp_reached == 4.0
        ));
    }

    /// An outage suspends the run where it stands, and resuming restarts the
    /// palier: deciding a step on samples taken while the source was away
    /// would be deciding it on the outage.
    #[test]
    fn an_outage_suspends_the_run_and_resuming_restarts_the_palier() {
        let mut machine = AutoTune::default();
        machine.start(0.0);
        let mut t = 0.0;
        let mut events = Vec::new();
        // Two entries into recovery inside the watchdog's window.
        for phase in [
            Phase::Other,
            Phase::LowRecover,
            Phase::Other,
            Phase::LowRecover,
        ] {
            t += 100.0;
            events.extend(machine.push_sample(Sample {
                t,
                phase,
                resample_ratio: Some(1.0),
                latency_smoothed_ms: Some(40.0),
                latency_target_ms: Some(40.0),
            }));
        }
        assert_eq!(machine.state(), State::Suspended);
        assert!(matches!(
            events.last(),
            Some(Event::SourceLost { events: 2 })
        ));
        // Nothing is decided while suspended.
        assert!(
            machine
                .push_sample(Sample {
                    t: 60_000.0,
                    ..Default::default()
                })
                .is_empty()
        );
        let resumed = machine.user_ack(Ack::ResumeAfterSourceLoss, 90_000.0);
        assert_eq!(
            resumed,
            vec![Event::SourceRecovered {
                restored: State::HoldKp
            }]
        );
        // The palier is timed from the resume, so a sample thirty seconds
        // after the *original* start decides nothing.
        assert!(
            machine
                .push_sample(Sample {
                    t: 100_000.0,
                    resample_ratio: Some(1.0),
                    ..Default::default()
                })
                .is_empty()
        );
    }

    /// The long run sizes the limit from the drift it saw, with a margin, and
    /// never below the floor.
    #[test]
    fn the_long_run_sizes_the_limit_from_what_the_link_needed() {
        let settings = Settings {
            long_run_default_ms: 10_000.0,
            long_run_stats_window_ms: 10_000.0,
            ..Default::default()
        };
        let mut machine = AutoTune::new(settings);
        machine.start(0.0);
        // Straight to the long run, the way the user's "skip" does.
        machine.state = State::AwaitPerturbation;
        machine.ctx.current_ki = 0.9;
        machine.user_ack(Ack::SkipPerturbation, 0.0);
        assert_eq!(machine.ctx.ki_final, Some(0.9));
        // A link that needed 40 000 ppm at its worst.
        let mut events = Vec::new();
        let mut t = 0.0;
        while t <= 11_000.0 {
            let ppm = if t > 5000.0 && t < 5200.0 {
                40_000.0
            } else {
                100.0
            };
            events.extend(machine.push_sample(Sample {
                t,
                resample_ratio: Some(1.0 + ppm / 1e6),
                latency_smoothed_ms: Some(40.0),
                latency_target_ms: Some(40.0),
                phase: Phase::Other,
            }));
            t += 100.0;
        }
        assert_eq!(machine.state(), State::Tightening);
        let patch = applied(&events).pop().expect("a patch");
        // 40 000 ppm × 1.5 = 6 %, over the 2 % floor.
        assert!((patch.max_adjust.unwrap() - 0.06).abs() < 1e-6);
        // The excursion makes the link a noisy one, so corrections stay
        // frequent.
        assert_eq!(
            patch.update_interval_callbacks,
            Some(Settings::default().update_interval_default)
        );
    }

    /// A quiet link gets the floor and the cheaper callback interval.
    #[test]
    fn a_quiet_link_gets_the_floor_and_fewer_callbacks() {
        let settings = Settings {
            long_run_default_ms: 10_000.0,
            long_run_stats_window_ms: 10_000.0,
            ..Default::default()
        };
        let mut machine = AutoTune::new(settings);
        machine.start(0.0);
        machine.state = State::AwaitPerturbation;
        machine.user_ack(Ack::SkipPerturbation, 0.0);
        let mut events = Vec::new();
        let mut t = 0.0;
        while t <= 11_000.0 {
            events.extend(machine.push_sample(Sample {
                t,
                resample_ratio: Some(1.0 + 20.0 / 1e6),
                latency_smoothed_ms: Some(40.0),
                latency_target_ms: Some(40.0),
                phase: Phase::Other,
            }));
            t += 100.0;
        }
        let patch = applied(&events).pop().expect("a patch");
        assert_eq!(patch.max_adjust, Some(settings.max_adjust_floor));
        assert_eq!(
            patch.update_interval_callbacks,
            Some(settings.update_interval_clean)
        );
    }

    #[test]
    fn a_cancelled_run_stops_answering() {
        let mut machine = AutoTune::default();
        machine.start(0.0);
        assert_eq!(machine.cancel(), vec![Event::Cancelled]);
        assert!(machine.cancel().is_empty());
        assert!(machine.push_sample(Sample::default()).is_empty());
        // And a fresh run may start from there.
        assert!(!machine.start(0.0).is_empty());
        assert_eq!(machine.state(), State::HoldKp);
    }
}
