//! Adaptive-resampling and latency-target controls.
//!
//! Each command forwards a single value to the renderer over OSC. See the
//! adaptive resampling regulator on the renderer side for the meaning of each
//! parameter.

use super::OscControlMsg;
use super::{SharedState, send_control};

pub fn control_adaptive_resampling(state: &SharedState, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

pub fn control_adaptive_resampling_enable_far_mode(state: &SharedState, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling/enable_far_mode".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

pub fn control_adaptive_resampling_force_silence_in_far_mode(state: &SharedState, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling/force_silence_in_far_mode".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

pub fn control_adaptive_resampling_hard_recover_high_in_far_mode(state: &SharedState, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling/hard_recover_high_in_far_mode"
                .to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

pub fn control_adaptive_resampling_hard_recover_low_in_far_mode(state: &SharedState, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling/hard_recover_low_in_far_mode"
                .to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

pub fn control_adaptive_resampling_far_mode_return_fade_in_ms(state: &SharedState, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling/far_mode_return_fade_in_ms"
                .to_string(),
            value: value.max(0),
        },
    );
}

/// Apply the target the form asked for: the model shows it at once (the
/// requested value and the target the meter draws against), and the renderer
/// is told.
pub fn set_latency_target(state: &SharedState, ms: i64) {
    let requested = ms.max(1);
    {
        let mut live = state.inner.lock().unwrap();
        live.app.latency.latency_requested_ms = Some(requested);
        live.app.latency.latency_target_ms = Some(requested);
    }
    control_latency_target(state, requested as i32);
}

/// The adaptive controller's own switch. It rides the audio document, so the
/// change is applied to the model and the whole document is sent.
pub fn set_adaptive_resampling_enabled(state: &SharedState, enabled: bool) {
    state.inner.lock().unwrap().app.adaptive_resampling = Some(u8::from(enabled));
    super::audio::send_audio_document(state);
}

/// Pause or resume the controller, the same way.
pub fn set_adaptive_resampling_paused(state: &SharedState, paused: bool) {
    state.inner.lock().unwrap().app.adaptive_resampling_paused = Some(u8::from(paused));
    super::audio::send_audio_document(state);
}

pub fn control_latency_target(state: &SharedState, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/latency_target".to_string(),
            value: value.max(1),
        },
    );
}

pub fn control_adaptive_resampling_kp_near(state: &SharedState, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/adaptive_resampling/kp_near".to_string(),
            value: value.max(0.00000001),
        },
    );
}

pub fn control_adaptive_resampling_ki(state: &SharedState, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/adaptive_resampling/ki".to_string(),
            value: value.max(0.00000001),
        },
    );
}

pub fn control_adaptive_resampling_integral_discharge_ratio(state: &SharedState, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/adaptive_resampling/integral_discharge_ratio".to_string(),
            value: value.clamp(0.0, 1.0),
        },
    );
}

pub fn control_adaptive_resampling_max_adjust(state: &SharedState, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/adaptive_resampling/max_adjust".to_string(),
            value: value.max(0.000001),
        },
    );
}

pub fn control_adaptive_resampling_update_interval_callbacks(state: &SharedState, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling/update_interval_callbacks".to_string(),
            value: value.max(1),
        },
    );
}

pub fn control_adaptive_resampling_high_recover_entry_margin_ms(state: &SharedState, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling/high_recover_entry_margin_ms"
                .to_string(),
            value: value.max(1),
        },
    );
}

pub fn control_adaptive_resampling_pause(state: &SharedState, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling/pause".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

pub fn control_adaptive_resampling_reset_ratio(state: &SharedState) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling/reset_ratio".to_string(),
            value: 1,
        },
    );
}
