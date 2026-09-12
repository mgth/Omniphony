//! The auto-tune wizard (`auto-tune/wizard-ui.js`).
//!
//! The dialog only. The run it shows — feeding [`crate::auto_tune`]'s machine,
//! patching the live controller, the snapshot Cancel restores — is
//! `host::services::auto_tune`, so a sweep keeps its cadence whether or not
//! anything is being drawn.

use egui::{RichText, Ui, vec2};

use crate::app::StudioSpike;
use crate::auto_tune::machine::{Ack, Context, Failure, Note, Outcome, State};
use crate::host::services::auto_tune::{self, Refused};
use crate::i18n::{t, tf};
use crate::ui::{theme, widgets};

/// The sparkline's window.
const SPARK_MS: f64 = 30_000.0;

/// What the dialog needs from the run, read once so nothing draws while the
/// model is locked.
struct View {
    state: State,
    ctx: Context,
    refused: Option<Refused>,
    started: bool,
    note: Option<Note>,
    outcome: Option<Outcome>,
    failure: Option<Failure>,
    can_abbreviate: bool,
    elapsed_ms: f64,
}

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
            auto_tune::open(&self.host);
        }
    }

    /// The run as the dialog reads it, or `None` when the wizard is closed.
    fn auto_tune_view(&self) -> Option<View> {
        let live = self.live.lock().unwrap();
        let run = live.auto_tune.as_ref()?;
        Some(View {
            state: run.machine.state(),
            ctx: run.machine.context(),
            refused: run.refused,
            started: run.started,
            note: run.note,
            outcome: run.outcome,
            failure: run.failure,
            can_abbreviate: run.can_abbreviate,
            elapsed_ms: run.elapsed_ms,
        })
    }

    pub(crate) fn auto_tune_modal(&mut self, ctx: &egui::Context) {
        if self.auto_tune_view().is_none() {
            return;
        }
        // The sparkline is the wizard's own picture of the run, and it is fed
        // whether or not the telemetry plot is open.
        if self.auto_tune_running() {
            self.poll_resample_sample(ctx);
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
            auto_tune::close(&self.host);
        }
    }

    fn auto_tune_running(&self) -> bool {
        self.live
            .lock()
            .unwrap()
            .auto_tune
            .as_ref()
            .is_some_and(auto_tune::Run::running)
    }

    fn auto_tune_body(&mut self, ui: &mut Ui) {
        let Some(view) = self.auto_tune_view() else {
            return;
        };
        let state = view.state;
        let ctx = view.ctx;
        ui.label(
            RichText::new(t("autoTune.title"))
                .size(theme::FONT_SIZE_TITLE)
                .color(theme::TEXT_STRONG),
        );
        ui.add_space(theme::ROW_GAP);
        if let Some(refused) = view.refused {
            let message = match refused {
                Refused::NotEnabled => t("autoTune.refusedNotEnabled"),
                Refused::Paused => t("autoTune.refusedPaused"),
            };
            widgets::banner(ui, widgets::Severity::Error, message, None);
            ui.add_space(theme::PANEL_GAP);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(t("common.close")).clicked() {
                    auto_tune::close(&self.host);
                }
            });
            return;
        }
        if !view.started {
            self.auto_tune_preparation(ui);
            return;
        }
        if let Some(failure) = view.failure {
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
                    auto_tune::revert(&self.host);
                }
            });
            return;
        }
        if let Some(outcome) = view.outcome {
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
                    &format!("{}", (view.elapsed_ms / 1000.0).round() as i64),
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
        if let Some(note) = view.note {
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

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            match state {
                State::AwaitPerturbation => {
                    if ui.button(t("autoTune.continue")).clicked() {
                        auto_tune::acknowledge(&self.host, Ack::Perturbation);
                    }
                    if ui.button(t("autoTune.skip")).clicked() {
                        auto_tune::acknowledge(&self.host, Ack::SkipPerturbation);
                    }
                }
                State::Suspended => {
                    if ui.button(t("autoTune.resume")).clicked() {
                        auto_tune::acknowledge(&self.host, Ack::ResumeAfterSourceLoss);
                    }
                }
                State::LongRun if view.can_abbreviate => {
                    if ui.button(t("autoTune.abbreviate")).clicked() {
                        auto_tune::abbreviate(&self.host);
                    }
                }
                _ => {}
            }
            if ui.button(t("common.cancel")).clicked() {
                auto_tune::revert(&self.host);
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
                auto_tune::start(&self.host);
            }
            if ui.button(t("common.cancel")).clicked() {
                auto_tune::close(&self.host);
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
                auto_tune::close(&self.host);
            }
            if ui.button(t("autoTune.revert")).clicked() {
                auto_tune::revert(&self.host);
            }
        });
    }

    /// Closing the window mid-run would leave the controller on the values the
    /// sweep happened to reach, so the run has to be settled first.
    pub(crate) fn auto_tune_quit_guard(&mut self, ctx: &egui::Context) {
        if !self.auto_tune_running() {
            return;
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.auto_tune_quit_asked = true;
        }
        if !self.auto_tune_quit_asked {
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
                    if ui.button(t("autoTune.quitStay")).clicked() {
                        self.auto_tune_quit_asked = false;
                    }
                    if ui.button(t("autoTune.quitLeave")).clicked() {
                        auto_tune::revert(&self.host);
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                });
            });
        if modal.should_close() {
            self.auto_tune_quit_asked = false;
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
