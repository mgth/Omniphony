//! The adaptive resampling controller's parameters: what they are, what they
//! read and what they write.
//!
//! The panel above this draws a form, and a form needs labels, steps and
//! decimals. None of that says what a parameter *is*. The identity — which
//! model field it stands for, what it defaults to when the renderer has not
//! said, the range outside which it is not a value at all, and the two
//! corrections the set has to satisfy — belongs here, next to the document the
//! controller is sent in.
//!
//! The numeric parameters share one apply: half a controller's settings applied
//! on their own is a worse state than the one before the edit.

use std::collections::BTreeMap;

use super::SharedState;
use crate::model::app_state::AppState;

/// A switch of the controller.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Switch {
    HardRecoverHigh,
    HardRecoverLow,
    SilenceFar,
    UsePreBridgeClock,
    UseOutputPacing,
    DisableBackpressure,
}

impl Switch {
    /// What it reads now, with the renderer's own default where it has said
    /// nothing.
    pub fn get(self, a: &AppState) -> bool {
        match self {
            Self::HardRecoverHigh => {
                a.adaptive_resampling_hard_recover_high_in_far_mode
                    .unwrap_or(1)
                    != 0
            }
            Self::HardRecoverLow => {
                a.adaptive_resampling_hard_recover_low_in_far_mode
                    .unwrap_or(0)
                    != 0
            }
            Self::SilenceFar => a.adaptive_resampling_force_silence_in_far_mode.unwrap_or(1) != 0,
            Self::UsePreBridgeClock => a.adaptive_resampling_use_pre_bridge_clock.unwrap_or(0) != 0,
            Self::UseOutputPacing => a.adaptive_resampling_use_output_pacing.unwrap_or(0) != 0,
            Self::DisableBackpressure => {
                a.adaptive_resampling_disable_backpressure.unwrap_or(0) != 0
            }
        }
    }

    fn set(self, a: &mut AppState, on: bool) {
        let v = Some(u8::from(on));
        match self {
            Self::HardRecoverHigh => a.adaptive_resampling_hard_recover_high_in_far_mode = v,
            Self::HardRecoverLow => a.adaptive_resampling_hard_recover_low_in_far_mode = v,
            Self::SilenceFar => a.adaptive_resampling_force_silence_in_far_mode = v,
            Self::UsePreBridgeClock => a.adaptive_resampling_use_pre_bridge_clock = v,
            Self::UseOutputPacing => a.adaptive_resampling_use_output_pacing = v,
            Self::DisableBackpressure => a.adaptive_resampling_disable_backpressure = v,
        }
    }
}

/// A numeric parameter of the controller.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Param {
    HighRecoverEntryMarginMs,
    LowRecoverEntryMarginMs,
    LowRecoverExitMarginMs,
    FarModeReturnFadeInMs,
    UpdateIntervalCallbacks,
    MaxAdjustPpm,
    KpNear,
    Ki,
    IntegralDischargeRatio,
    LowRecoverSettleStableMs,
    LowRecoverSettleMarginMs,
    LowRecoverRefillDeltaAlpha,
    ControlSmoothingCutoffHz,
    ControlSmoothingOrder,
}

impl Param {
    /// What it reads now, with the renderer's own default where it has said
    /// nothing.
    pub fn get(self, a: &AppState) -> f64 {
        match self {
            Self::HighRecoverEntryMarginMs => a
                .adaptive_resampling_high_recover_entry_margin_ms
                .unwrap_or(1000) as f64,
            Self::LowRecoverEntryMarginMs => a
                .adaptive_resampling_low_recover_entry_margin_ms
                .unwrap_or(18.0),
            Self::LowRecoverExitMarginMs => a
                .adaptive_resampling_low_recover_exit_margin_ms
                .unwrap_or(6.0),
            Self::FarModeReturnFadeInMs => a
                .adaptive_resampling_far_mode_return_fade_in_ms
                .unwrap_or(0) as f64,
            Self::UpdateIntervalCallbacks => {
                a.adaptive_resampling_update_interval_callbacks.unwrap_or(1) as f64
            }
            // The model holds a ratio; the form is in parts per million, which
            // is the unit the value is talked about in.
            Self::MaxAdjustPpm => (a.adaptive_resampling_max_adjust.unwrap_or(0.01) * 1e6).round(),
            Self::KpNear => a.adaptive_resampling_kp_near.unwrap_or(1.0),
            Self::Ki => a.adaptive_resampling_ki.unwrap_or(1.0),
            Self::IntegralDischargeRatio => a
                .adaptive_resampling_integral_discharge_ratio
                .unwrap_or(0.25),
            Self::LowRecoverSettleStableMs => a
                .adaptive_resampling_low_recover_settle_stable_ms
                .unwrap_or(200.0),
            Self::LowRecoverSettleMarginMs => a
                .adaptive_resampling_low_recover_settle_margin_ms
                .unwrap_or(6.0),
            Self::LowRecoverRefillDeltaAlpha => a
                .adaptive_resampling_low_recover_refill_delta_alpha
                .unwrap_or(0.5),
            Self::ControlSmoothingCutoffHz => a
                .adaptive_resampling_control_smoothing_cutoff_hz
                .unwrap_or(0.5),
            Self::ControlSmoothingOrder => {
                a.adaptive_resampling_control_smoothing_order.unwrap_or(1) as f64
            }
        }
    }

    /// The range outside which it is not a value. The form clamps to it as the
    /// user types; the apply clamps to it again, because a value that arrives
    /// any other way is no less wrong.
    pub fn range(self) -> (f64, f64) {
        match self {
            Self::HighRecoverEntryMarginMs => (1.0, 10_000.0),
            Self::LowRecoverEntryMarginMs => (0.0, 1000.0),
            Self::LowRecoverExitMarginMs => (0.0, 1000.0),
            Self::FarModeReturnFadeInMs => (0.0, 10_000.0),
            Self::UpdateIntervalCallbacks => (1.0, 1000.0),
            Self::MaxAdjustPpm => (1.0, 100_000.0),
            Self::KpNear => (0.01, 100.0),
            Self::Ki => (0.0, 100.0),
            Self::IntegralDischargeRatio => (0.0, 1.0),
            Self::LowRecoverSettleStableMs => (0.0, 10_000.0),
            Self::LowRecoverSettleMarginMs => (0.0, 1000.0),
            Self::LowRecoverRefillDeltaAlpha => (0.0, 1.0),
            Self::ControlSmoothingCutoffHz => (0.001, 20.0),
            Self::ControlSmoothingOrder => (1.0, 2.0),
        }
    }

    fn set(self, a: &mut AppState, v: f64) {
        match self {
            Self::HighRecoverEntryMarginMs => {
                a.adaptive_resampling_high_recover_entry_margin_ms = Some(v.round() as i64);
            }
            Self::LowRecoverEntryMarginMs => {
                a.adaptive_resampling_low_recover_entry_margin_ms = Some(v);
            }
            Self::LowRecoverExitMarginMs => {
                a.adaptive_resampling_low_recover_exit_margin_ms = Some(v);
            }
            Self::FarModeReturnFadeInMs => {
                a.adaptive_resampling_far_mode_return_fade_in_ms = Some(v.round() as i64);
            }
            Self::UpdateIntervalCallbacks => {
                a.adaptive_resampling_update_interval_callbacks = Some(v.round() as i64);
            }
            Self::MaxAdjustPpm => a.adaptive_resampling_max_adjust = Some((v / 1e6).max(1e-6)),
            Self::KpNear => a.adaptive_resampling_kp_near = Some(v),
            Self::Ki => a.adaptive_resampling_ki = Some(v),
            Self::IntegralDischargeRatio => {
                a.adaptive_resampling_integral_discharge_ratio = Some(v);
            }
            Self::LowRecoverSettleStableMs => {
                a.adaptive_resampling_low_recover_settle_stable_ms = Some(v.round());
            }
            Self::LowRecoverSettleMarginMs => {
                a.adaptive_resampling_low_recover_settle_margin_ms = Some(v);
            }
            Self::LowRecoverRefillDeltaAlpha => {
                a.adaptive_resampling_low_recover_refill_delta_alpha = Some(v);
            }
            Self::ControlSmoothingCutoffHz => {
                a.adaptive_resampling_control_smoothing_cutoff_hz = Some(v);
            }
            Self::ControlSmoothingOrder => {
                a.adaptive_resampling_control_smoothing_order = Some(v.round() as u32);
            }
        }
    }
}

/// Flip one switch and send the whole controller.
pub fn set_switch(state: &SharedState, switch: Switch, on: bool) {
    {
        let mut live = state.inner.lock().unwrap();
        switch.set(&mut live.app, on);
        // The far mode is not a switch of its own: it is on when any of its
        // three actions is armed.
        let derived = Switch::HardRecoverHigh.get(&live.app)
            || Switch::HardRecoverLow.get(&live.app)
            || Switch::SilenceFar.get(&live.app);
        live.app.adaptive_resampling_enable_far_mode = Some(u8::from(derived));
    }
    super::audio::send_audio_document(state);
}

/// Apply a batch of edited parameters and send the whole controller.
///
/// The exit margin is corrected against the entry margin: the hysteresis has to
/// stay well formed whatever order the two were typed in, and an exit above its
/// entry is a band that never closes.
pub fn apply_params(state: &SharedState, edits: &BTreeMap<Param, f64>) {
    {
        let mut live = state.inner.lock().unwrap();
        for (param, value) in edits {
            let (min, max) = param.range();
            param.set(&mut live.app, value.clamp(min, max));
        }
        let entry = Param::LowRecoverEntryMarginMs.get(&live.app);
        let exit = Param::LowRecoverExitMarginMs.get(&live.app);
        Param::LowRecoverExitMarginMs.set(&mut live.app, exit.min((entry - 0.1).max(0.0)));
    }
    super::audio::send_audio_document(state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_far_mode_follows_the_three_actions_that_can_fire_in_it() {
        let state = super::super::tests::state();
        // Silencing is on by default, so the mode is on before anything is
        // touched — and stays on until the last of the three goes.
        set_switch(&state, Switch::HardRecoverHigh, false);
        assert_eq!(
            state
                .inner
                .lock()
                .unwrap()
                .app
                .adaptive_resampling_enable_far_mode,
            Some(1)
        );
        set_switch(&state, Switch::SilenceFar, false);
        assert_eq!(
            state
                .inner
                .lock()
                .unwrap()
                .app
                .adaptive_resampling_enable_far_mode,
            Some(0)
        );
        set_switch(&state, Switch::HardRecoverLow, true);
        assert_eq!(
            state
                .inner
                .lock()
                .unwrap()
                .app
                .adaptive_resampling_enable_far_mode,
            Some(1)
        );
    }

    #[test]
    fn an_applied_batch_is_clamped_and_leaves_the_hysteresis_well_formed() {
        let state = super::super::tests::state();
        apply_params(
            &state,
            &BTreeMap::from([
                // Out of range in both directions.
                (Param::KpNear, 1e9),
                (Param::ControlSmoothingCutoffHz, -3.0),
                // An exit margin above its entry: a band that never closes.
                (Param::LowRecoverEntryMarginMs, 10.0),
                (Param::LowRecoverExitMarginMs, 40.0),
            ]),
        );
        let live = state.inner.lock().unwrap();
        assert_eq!(Param::KpNear.get(&live.app), 100.0);
        assert_eq!(Param::ControlSmoothingCutoffHz.get(&live.app), 0.001);
        assert_eq!(Param::LowRecoverEntryMarginMs.get(&live.app), 10.0);
        assert_eq!(Param::LowRecoverExitMarginMs.get(&live.app), 9.9);
    }

    #[test]
    fn the_parts_per_million_form_round_trips_through_the_stored_ratio() {
        let state = super::super::tests::state();
        apply_params(&state, &BTreeMap::from([(Param::MaxAdjustPpm, 25_000.0)]));
        let live = state.inner.lock().unwrap();
        assert_eq!(live.app.adaptive_resampling_max_adjust, Some(0.025));
        assert_eq!(Param::MaxAdjustPpm.get(&live.app), 25_000.0);
    }
}
