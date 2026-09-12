//! The auto-tune wizard (`auto-tune/wizard-ui.js` and `runner.js`).
//!
//! The dialog that drives [`crate::auto_tune`]: it feeds the machine
//! telemetry, applies the patches it asks for to the live controller, and
//! shows what step the run is on with the two traces the run is about.
//!
//! Two rules the web set and this keeps. The values are applied live but not
//! persisted — Save is still the user's to press — and the values the run
//! started from are snapshotted so Cancel and Revert put the controller back
//! exactly as it was.

use std::time::{Duration, Instant};

use egui::{RichText, Ui, vec2};

use crate::app::StudioSpike;
use crate::auto_tune::detectors::{Phase, Sample};
use crate::auto_tune::machine::{Ack, AutoTune, Event, Failure, Note, Outcome, Patch, State};
use crate::i18n::{t, tf};
use crate::ui::{theme, widgets};

/// The web polls the controller at 50 ms; the frame loop is faster than that
/// and there is nothing to gain from feeding the machine every frame.
const POLL: Duration = Duration::from_millis(50);
/// The sparkline's window.
const SPARK_MS: f64 = 30_000.0;

/// Why a run could not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    NotEnabled,
    Paused,
}

/// The dialog's state while it is open.
pub struct Wizard {
    pub machine: AutoTune,
    /// Set until Start is pressed, and again if the start is refused.
    pub refused: Option<Refused>,
    pub started: bool,
    /// The last note the machine attached to a step, shown under it.
    pub note: Option<Note>,
    pub outcome: Option<Outcome>,
    pub failure: Option<Failure>,
    pub can_abbreviate: bool,
    pub elapsed_ms: f64,
    pub polled_at: Option<Instant>,
    /// The values the controller had before the run, restored on Cancel.
    pub snapshot: Option<Patch>,
    /// A close was asked for while the run was going.
    pub quit_asked: bool,
}

impl Default for Wizard {
    fn default() -> Self {
        Self {
            machine: AutoTune::default(),
            refused: None,
            started: false,
            note: None,
            outcome: None,
            failure: None,
            can_abbreviate: false,
            elapsed_ms: 0.0,
            polled_at: None,
            snapshot: None,
            quit_asked: false,
        }
    }
}

/// A number, or a dash where there is none.
fn number(value: Option<f64>, decimals: usize) -> String {
    match value.filter(|v| v.is_finite()) {
        Some(v) => format!("{v:.decimals$}"),
        None => "—".to_owned(),
    }
}

/// What a note says under the step it belongs to, and whether it is a warning.
fn note_line(note: Note) -> (String, bool) {
    match note {
        Note::Diverging => ("The error grew over the palier.".to_owned(), false),
        Note::Overshoot => (
            "Overshooting — backing the integral term off.".to_owned(),
            false,
        ),
        Note::TooSlow => ("Too slow — raising the integral term.".to_owned(), false),
        Note::StillConverging => ("Converging, but not yet inside the band.".to_owned(), false),
        Note::HitIterationCap => (t("autoTune.kiBestKept").to_owned(), true),
        Note::KiCollapsed => (
            "The integral term reached its floor; keeping the best seen.".to_owned(),
            true,
        ),
        Note::PerturbationOscillation => (
            "The recovery left the loop ringing — tuning again.".to_owned(),
            true,
        ),
        Note::SkippedPerturbation => ("Disturbance test skipped.".to_owned(), false),
        Note::MaxAdjustWarn => (t("autoTune.maxAdjustWarn").to_owned(), true),
    }
}

impl StudioSpike {
    /// The button that opens it, on the adaptive controller's row.
    pub(crate) fn auto_tune_button(&mut self, ui: &mut Ui) {
        if ui
            .button(t("autoTune.openButton"))
            .on_hover_text(t("autoTune.openButtonTitle"))
            .clicked()
        {
            self.auto_tune = Some(Wizard::default());
        }
    }

    pub(crate) fn auto_tune_modal(&mut self, ctx: &egui::Context) {
        self.maintain_auto_tune(ctx);
        if self.auto_tune.is_none() {
            return;
        }
        let modal = egui::Modal::new(egui::Id::new("auto-tune"))
            .frame(widgets::modal_frame())
            .show(ctx, |ui| {
                ui.set_width(520.0);
                self.auto_tune_body(ui);
            });
        // A run in progress is not closed by clicking outside it: the
        // controller is mid-patch, and dismissing the window by accident would
        // leave it there.
        if modal.should_close() && !self.auto_tune_running() {
            self.close_auto_tune();
        }
    }

    fn auto_tune_running(&self) -> bool {
        self.auto_tune.as_ref().is_some_and(|w| {
            !matches!(
                w.machine.state(),
                State::Idle | State::Completed | State::Cancelled | State::Failed
            )
        })
    }

    fn auto_tune_body(&mut self, ui: &mut Ui) {
        let Some(wizard) = &self.auto_tune else {
            return;
        };
        let state = wizard.machine.state();
        let ctx = wizard.machine.context();
        ui.label(
            RichText::new(t("autoTune.title"))
                .size(theme::FONT_SIZE_TITLE)
                .color(theme::TEXT_STRONG),
        );
        ui.add_space(theme::ROW_GAP);
        if let Some(refused) = wizard.refused {
            let message = match refused {
                Refused::NotEnabled => t("autoTune.refusedNotEnabled"),
                Refused::Paused => t("autoTune.refusedPaused"),
            };
            widgets::banner(ui, widgets::Severity::Error, message, None);
            ui.add_space(theme::PANEL_GAP);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(t("common.close")).clicked() {
                    self.auto_tune = None;
                }
            });
            return;
        }
        if !wizard.started {
            self.auto_tune_preparation(ui);
            return;
        }
        if let Some(failure) = wizard.failure {
            let Failure::NoOscillation { kp_reached } = failure;
            widgets::banner(
                ui,
                widgets::Severity::Error,
                &tf(
                    "autoTune.errorNoOsc",
                    &[("kp", &number(Some(kp_reached), 2))],
                ),
                None,
            );
            ui.add_space(theme::PANEL_GAP);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(t("common.close")).clicked() {
                    self.revert_auto_tune();
                }
            });
            return;
        }
        if let Some(outcome) = wizard.outcome {
            self.auto_tune_summary(ui, outcome);
            return;
        }
        // ── a run in progress ────────────────────────────────────────────
        let (title, hint) = match state {
            State::HoldKp => ("autoTune.step1Label", "autoTune.holdHint"),
            State::TuningKi => ("autoTune.step2Label", ""),
            State::AwaitPerturbation => ("autoTune.step3Label", "autoTune.perturbationPrompt"),
            State::PerturbationRecovering => {
                ("autoTune.step3Recovering", "autoTune.recoveringHint")
            }
            State::LongRun => ("autoTune.step4Label", "autoTune.longRunHint"),
            State::Tightening => ("autoTune.step5Label", "autoTune.tighteningHint"),
            State::Suspended => ("autoTune.suspended", "autoTune.sourceLostHint"),
            _ => ("autoTune.title", ""),
        };
        ui.label(RichText::new(t(title)).size(theme::FONT_SIZE).color(
            if state == State::Suspended {
                theme::WARN
            } else {
                theme::TEXT_STRONG
            },
        ));
        let values = match state {
            State::HoldKp => format!("Kp = {}", number(Some(ctx.current_kp), 2)),
            State::TuningKi | State::PerturbationRecovering => format!(
                "Kp = {} · Ki = {}",
                number(ctx.kp_final, 2),
                number(Some(ctx.current_ki), 4)
            ),
            State::AwaitPerturbation => format!(
                "Kp = {} · Ki = {}",
                number(ctx.kp_final, 2),
                number(ctx.ki_final, 4)
            ),
            State::Tightening => format!(
                "max_adjust = {} % · update_interval_callbacks = {}",
                number(ctx.max_adjust_final.map(|v| v * 100.0), 2),
                ctx.update_interval_final
                    .map_or_else(|| "—".to_owned(), |v| v.to_string())
            ),
            State::LongRun => tf(
                "autoTune.elapsed",
                &[(
                    "sec",
                    &format!("{}", (wizard.elapsed_ms / 1000.0).round() as i64),
                )],
            ),
            _ => String::new(),
        };
        if !values.is_empty() {
            ui.label(RichText::new(values).size(theme::FONT_SIZE));
        }
        if state == State::TuningKi {
            widgets::note(
                ui,
                &tf(
                    "autoTune.kiIteration",
                    &[("iter", &ctx.ki_iteration.to_string())],
                ),
            );
        }
        if !hint.is_empty() {
            widgets::note(ui, t(hint));
        }
        if let Some(note) = wizard.note {
            let (line, warn) = note_line(note);
            ui.label(
                RichText::new(line)
                    .size(theme::FONT_SIZE_SMALL)
                    .color(if warn { theme::WARN } else { theme::TEXT_MUTED }),
            );
        }
        ui.add_space(theme::ROW_GAP);
        let (rect, _) =
            ui.allocate_exact_size(vec2(ui.available_width(), 140.0), egui::Sense::hover());
        let painter = ui.painter().with_clip_rect(rect);
        self.resample_traces(&painter, rect, SPARK_MS);
        ui.add_space(theme::PANEL_GAP);

        let can_abbreviate = self.auto_tune.as_ref().is_some_and(|w| w.can_abbreviate);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            match state {
                State::AwaitPerturbation => {
                    if ui.button(t("autoTune.continue")).clicked() {
                        self.ack_auto_tune(Ack::Perturbation);
                    }
                    if ui.button(t("autoTune.skip")).clicked() {
                        self.ack_auto_tune(Ack::SkipPerturbation);
                    }
                }
                State::Suspended => {
                    if ui.button(t("autoTune.resume")).clicked() {
                        self.ack_auto_tune(Ack::ResumeAfterSourceLoss);
                    }
                }
                State::LongRun if can_abbreviate => {
                    if ui.button(t("autoTune.abbreviate")).clicked()
                        && let Some(wizard) = &mut self.auto_tune
                    {
                        wizard.machine.abbreviate();
                    }
                }
                _ => {}
            }
            if ui.button(t("common.cancel")).clicked() {
                self.revert_auto_tune();
            }
        });
    }

    fn auto_tune_preparation(&mut self, ui: &mut Ui) {
        ui.label(
            RichText::new(t("autoTune.intro"))
                .size(theme::FONT_SIZE)
                .color(theme::TEXT_STRONG),
        );
        ui.label(RichText::new(t("autoTune.prep")).size(theme::FONT_SIZE));
        widgets::note(ui, t("autoTune.persistReminder"));
        ui.add_space(theme::PANEL_GAP);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(t("autoTune.start")).clicked() {
                self.start_auto_tune();
            }
            if ui.button(t("common.cancel")).clicked() {
                self.auto_tune = None;
            }
        });
    }

    fn auto_tune_summary(&mut self, ui: &mut Ui, outcome: Outcome) {
        ui.label(
            RichText::new(t("autoTune.summaryTitle"))
                .size(theme::FONT_SIZE)
                .color(theme::TEXT_STRONG),
        );
        for (label, value) in [
            ("kp_crit", format!("≈ {}", number(outcome.kp_crit, 2))),
            ("kp_near", number(outcome.kp_final, 2)),
            ("ki", number(outcome.ki_final, 4)),
            (
                "max_adjust",
                format!(
                    "{} %",
                    number(outcome.max_adjust_final.map(|v| v * 100.0), 2)
                ),
            ),
            (
                "update_interval_callbacks",
                outcome
                    .update_interval_final
                    .map_or_else(|| "—".to_owned(), |v| v.to_string()),
            ),
        ] {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(label)
                        .size(theme::FONT_SIZE_SMALL)
                        .color(theme::TEXT_MUTED)
                        .monospace(),
                );
                ui.label(RichText::new(value).size(theme::FONT_SIZE).monospace());
            });
        }
        // The final palier is the check on the numbers above: it ran with them
        // and says whether the loop stayed put.
        if outcome.tightening_oscillation {
            widgets::banner(
                ui,
                widgets::Severity::Warning,
                "The final palier was still ringing — the numbers are worth a second run.",
                None,
            );
        } else if !outcome.tightening_converged {
            widgets::note(
                ui,
                "The final palier did not converge inside its band; the numbers hold, but the \
                 link was not quiet.",
            );
        }
        widgets::note(ui, t("autoTune.persistReminder"));
        ui.add_space(theme::PANEL_GAP);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.button(t("autoTune.accept")).clicked() {
                // The values are already live; accepting is letting them
                // stand, and dropping the snapshot that would undo them.
                self.close_auto_tune();
            }
            if ui.button(t("autoTune.revert")).clicked() {
                self.revert_auto_tune();
            }
        });
    }

    // ── the runner ──────────────────────────────────────────────────────

    fn start_auto_tune(&mut self) {
        let (enabled, paused) = {
            let live = self.live.lock().unwrap();
            (
                live.app.adaptive_resampling.unwrap_or(0) != 0,
                live.app.adaptive_resampling_paused.unwrap_or(0) != 0,
            )
        };
        // The run patches a controller that has to be running: tuning a
        // disabled or paused one would tune nothing.
        let refused = match (enabled, paused) {
            (false, _) => Some(Refused::NotEnabled),
            (_, true) => Some(Refused::Paused),
            _ => None,
        };
        if let Some(refused) = refused {
            if let Some(wizard) = &mut self.auto_tune {
                wizard.refused = Some(refused);
            }
            return;
        }
        let snapshot = self.controller_snapshot();
        let Some(wizard) = &mut self.auto_tune else {
            return;
        };
        wizard.snapshot = Some(snapshot);
        wizard.started = true;
        let events = wizard
            .machine
            .start(self.diag_started.elapsed().as_secs_f64() * 1000.0);
        self.apply_auto_tune_events(events);
    }

    /// The four values the run touches, as they are now.
    fn controller_snapshot(&self) -> Patch {
        let live = self.live.lock().unwrap();
        Patch {
            kp_near: live.app.adaptive_resampling_kp_near,
            ki: live.app.adaptive_resampling_ki,
            max_adjust: live.app.adaptive_resampling_max_adjust,
            update_interval_callbacks: live
                .app
                .adaptive_resampling_update_interval_callbacks
                .map(|v| v.max(1) as u32),
        }
    }

    fn ack_auto_tune(&mut self, ack: Ack) {
        let now = self.diag_started.elapsed().as_secs_f64() * 1000.0;
        let Some(wizard) = &mut self.auto_tune else {
            return;
        };
        let events = wizard.machine.user_ack(ack, now);
        self.apply_auto_tune_events(events);
    }

    /// Put the controller back where the run found it and close.
    fn revert_auto_tune(&mut self) {
        let snapshot = self.auto_tune.as_ref().and_then(|w| w.snapshot);
        if let Some(wizard) = &mut self.auto_tune {
            wizard.machine.cancel();
        }
        if let Some(snapshot) = snapshot {
            self.apply_auto_tune_patch(snapshot);
        }
        self.close_auto_tune();
    }

    fn close_auto_tune(&mut self) {
        self.auto_tune = None;
    }

    /// Feed the machine, at the web's cadence, and act on what it says.
    fn maintain_auto_tune(&mut self, ctx: &egui::Context) {
        if !self.auto_tune_running() {
            return;
        }
        // The sparkline is the wizard's own picture of the run, and it is fed
        // whether or not the telemetry plot is open.
        self.poll_resample_sample(ctx);
        let now = Instant::now();
        let due = self
            .auto_tune
            .as_ref()
            .and_then(|w| w.polled_at)
            .is_none_or(|at| now.duration_since(at) >= POLL);
        if !due {
            return;
        }
        let sample = {
            let live = self.live.lock().unwrap();
            Sample {
                t: self.diag_started.elapsed().as_secs_f64() * 1000.0,
                latency_smoothed_ms: live.app.latency.latency_smoothed_ms,
                latency_target_ms: live.app.latency.latency_target_ms.map(|v| v as f64),
                resample_ratio: live.app.resample_ratio,
                phase: match live.app.adaptive_resampling_state.as_deref() {
                    Some("low-recover") => Phase::LowRecover,
                    _ => Phase::Other,
                },
            }
        };
        let Some(wizard) = &mut self.auto_tune else {
            return;
        };
        wizard.polled_at = Some(now);
        let events = wizard.machine.push_sample(sample);
        self.apply_auto_tune_events(events);
    }

    fn apply_auto_tune_events(&mut self, events: Vec<Event>) {
        for event in events {
            match event {
                Event::ApplyParams(patch) => self.apply_auto_tune_patch(patch),
                Event::Progress(progress) => {
                    if let Some(wizard) = &mut self.auto_tune {
                        wizard.note = progress.note;
                        if let Some(elapsed) = progress.elapsed_ms {
                            wizard.elapsed_ms = elapsed;
                        }
                        wizard.can_abbreviate |= progress.can_abbreviate;
                    }
                }
                Event::Complete(outcome) => {
                    if let Some(wizard) = &mut self.auto_tune {
                        wizard.outcome = Some(outcome);
                    }
                    self.log("info", "auto-tune", "the run finished".to_owned());
                }
                Event::Failed(failure) => {
                    if let Some(wizard) = &mut self.auto_tune {
                        wizard.failure = Some(failure);
                    }
                }
                Event::SourceLost { events } => {
                    self.log(
                        "warn",
                        "auto-tune",
                        format!("the source went away ({events} recoveries); the run is held"),
                    );
                }
                Event::SourceRecovered { .. } | Event::AwaitUserAction(_) | Event::Cancelled => {}
            }
        }
    }

    /// Write a patch into the live controller and send it, the way a slider
    /// on the adaptive panel would.
    fn apply_auto_tune_patch(&mut self, patch: Patch) {
        {
            let mut live = self.live.lock().unwrap();
            if let Some(kp) = patch.kp_near {
                live.app.adaptive_resampling_kp_near = Some(kp);
            }
            if let Some(ki) = patch.ki {
                live.app.adaptive_resampling_ki = Some(ki);
            }
            if let Some(max_adjust) = patch.max_adjust {
                live.app.adaptive_resampling_max_adjust = Some(max_adjust);
            }
            if let Some(interval) = patch.update_interval_callbacks {
                live.app.adaptive_resampling_update_interval_callbacks =
                    Some(interval.max(1) as i64);
            }
        }
        crate::host::commands::audio::send_audio_document(&self.host);
    }

    /// Closing the window mid-run would leave the controller on the values the
    /// sweep happened to reach, so the run has to be settled first.
    pub(crate) fn auto_tune_quit_guard(&mut self, ctx: &egui::Context) {
        if !self.auto_tune_running() {
            return;
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if let Some(wizard) = &mut self.auto_tune {
                wizard.quit_asked = true;
            }
        }
        if !self.auto_tune.as_ref().is_some_and(|w| w.quit_asked) {
            return;
        }
        let modal = egui::Modal::new(egui::Id::new("auto-tune-quit"))
            .frame(widgets::modal_frame())
            .show(ctx, |ui| {
                ui.set_max_width(380.0);
                ui.label(
                    RichText::new(t("autoTune.quitTitle"))
                        .size(theme::FONT_SIZE)
                        .color(theme::TEXT_STRONG),
                );
                ui.label(RichText::new(t("autoTune.quitBody")).size(theme::FONT_SIZE));
                ui.add_space(theme::PANEL_GAP);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(t("autoTune.quitStay")).clicked()
                        && let Some(wizard) = &mut self.auto_tune
                    {
                        wizard.quit_asked = false;
                    }
                    if ui.button(t("autoTune.quitLeave")).clicked() {
                        self.revert_auto_tune();
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
        if modal.should_close()
            && let Some(wizard) = &mut self.auto_tune
        {
            wizard.quit_asked = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The notes the run attaches to a step are all said in words, and the
    /// ones that mean "look at this" are marked as warnings.
    #[test]
    fn every_note_says_something_and_the_warnings_are_marked() {
        for note in [
            Note::Diverging,
            Note::Overshoot,
            Note::TooSlow,
            Note::StillConverging,
            Note::HitIterationCap,
            Note::KiCollapsed,
            Note::PerturbationOscillation,
            Note::SkippedPerturbation,
            Note::MaxAdjustWarn,
        ] {
            let (line, _) = note_line(note);
            assert!(!line.is_empty(), "{note:?} has no line");
        }
        assert!(note_line(Note::MaxAdjustWarn).1);
        assert!(note_line(Note::PerturbationOscillation).1);
        assert!(!note_line(Note::TooSlow).1);
    }

    #[test]
    fn a_missing_number_reads_as_a_dash() {
        assert_eq!(number(Some(1.5), 2), "1.50");
        assert_eq!(number(None, 2), "—");
        assert_eq!(number(Some(f64::NAN), 2), "—");
    }
}
