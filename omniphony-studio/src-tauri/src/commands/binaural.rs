//! Binaural (headphone) output controls: output-mode toggle, HRIR source, and
//! the Sensors2OSC head-tracking settings (address, format, smoothing, invert,
//! recenter).
//!
//! Each command forwards a value to the renderer over OSC.

use crate::osc_listener::OscControlMsg;
use crate::{send_control, SharedState};
use tauri::State;

#[tauri::command]
pub fn control_output_mode(state: State<SharedState>, value: String) {
    let normalized = value.trim().to_ascii_lowercase();
    if !matches!(normalized.as_str(), "speaker" | "binaural") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/output_mode".to_string(),
            value: normalized,
        },
    );
}

#[tauri::command]
pub fn control_binaural_mode(state: State<SharedState>, value: String) {
    // Binaural stage input: per-object HRTF ("direct") or the fixed
    // virtual-speaker cascade ("cascaded").
    let normalized = value.trim().to_ascii_lowercase();
    if !matches!(normalized.as_str(), "direct" | "cascaded") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/binaural_mode".to_string(),
            value: normalized,
        },
    );
}

#[tauri::command]
pub fn control_ear_gain(state: State<SharedState>, ear: u32, value: f32) {
    // Headphone L/R output gain: dedicated ear params (the ears no longer
    // ride the first two per-speaker slots).
    if ear > 1 || !value.is_finite() {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: "/omniphony/control/binaural/ear_gain".to_string(),
            args: vec![
                rosc::OscType::Int(ear as i32),
                rosc::OscType::Float(value.clamp(0.0, 4.0)),
            ],
        },
    );
}

#[tauri::command]
pub fn control_ear_mute(state: State<SharedState>, ear: u32, muted: bool) {
    // Headphone L/R mute (solo is composed client-side from the two mutes).
    if ear > 1 {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: "/omniphony/control/binaural/ear_mute".to_string(),
            args: vec![
                rosc::OscType::Int(ear as i32),
                rosc::OscType::Int(if muted { 1 } else { 0 }),
            ],
        },
    );
}

#[tauri::command]
pub fn control_hrir_source(state: State<SharedState>, value: String) {
    // "synthetic" | "saf"/"kemar" | "sofa:<path>".
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/binaural/hrir_source".to_string(),
            value: value.trim().to_string(),
        },
    );
}

#[tauri::command]
pub fn control_binaural_unit_scale(state: State<SharedState>, value: f32) {
    // Metres per ADM unit (isotropic distance scale).
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/binaural/unit_scale".to_string(),
            value: value.clamp(0.01, 100.0),
        },
    );
}

#[tauri::command]
pub fn control_binaural_head_radius(state: State<SharedState>, value: f32) {
    // Effective head radius in metres (half the inter-ear distance).
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/binaural/head_radius".to_string(),
            value: value.clamp(0.05, 0.15),
        },
    );
}

#[tauri::command]
pub fn control_binaural_reflections_enabled(state: State<SharedState>, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/binaural/reflections/enabled".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
pub fn control_binaural_reflections_level(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/binaural/reflections/level".to_string(),
            value: value.clamp(0.0, 1.0),
        },
    );
}

#[tauri::command]
pub fn control_binaural_reflections_wall_cutoff(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/binaural/reflections/wall_cutoff".to_string(),
            value: value.clamp(1_000.0, 20_000.0),
        },
    );
}

#[tauri::command]
pub fn control_binaural_reflections_room(state: State<SharedState>, axis: String, value: f32) {
    // axis: "width" | "depth" | "height"; value in metres.
    let address = match axis.as_str() {
        "width" => "/omniphony/control/binaural/reflections/room_width",
        "depth" => "/omniphony/control/binaural/reflections/room_depth",
        "height" => "/omniphony/control/binaural/reflections/room_height",
        _ => return,
    };
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: address.to_string(),
            value: value.clamp(1.0, 20.0),
        },
    );
}

#[tauri::command]
pub fn control_binaural_reverb_enabled(state: State<SharedState>, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/binaural/reverb/enabled".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
pub fn control_binaural_reverb_level(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/binaural/reverb/level".to_string(),
            value: value.clamp(0.0, 1.0),
        },
    );
}

#[tauri::command]
pub fn control_binaural_reverb_rt60(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/binaural/reverb/rt60".to_string(),
            value: value.clamp(0.1, 3.0),
        },
    );
}

#[tauri::command]
pub fn control_binaural_reverb_size(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/binaural/reverb/size".to_string(),
            value: value.clamp(0.5, 2.0),
        },
    );
}

#[tauri::command]
pub fn control_binaural_reverb_rt60_low_ratio(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/binaural/reverb/rt60_low_ratio".to_string(),
            value: value.clamp(0.25, 4.0),
        },
    );
}

#[tauri::command]
pub fn control_binaural_reverb_rt60_high_ratio(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/binaural/reverb/rt60_high_ratio".to_string(),
            value: value.clamp(0.25, 4.0),
        },
    );
}

#[tauri::command]
pub fn control_binaural_diffuse_field_eq(state: State<SharedState>, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/binaural/diffuse_field_eq".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
pub fn control_binaural_air_absorption(state: State<SharedState>, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/binaural/air_absorption".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
pub fn control_head_calibrate(state: State<SharedState>, step: String) {
    // step: "front" | "left" | "up" | "reset".
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/head/calibrate".to_string(),
            value: step,
        },
    );
}

#[tauri::command]
pub fn control_head_recenter(state: State<SharedState>) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/head/recenter".to_string(),
            value: 1,
        },
    );
}

#[tauri::command]
pub fn control_head_tracking_address(state: State<SharedState>, value: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/head/tracking/address".to_string(),
            value: value.trim().to_string(),
        },
    );
}

#[tauri::command]
pub fn control_head_tracking_format(state: State<SharedState>, value: String) {
    let normalized = value.trim().to_ascii_lowercase();
    if !matches!(normalized.as_str(), "auto" | "quat" | "rotvec" | "euler") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/head/tracking/format".to_string(),
            value: normalized,
        },
    );
}

#[tauri::command]
pub fn control_head_tracking_smoothing(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/head/tracking/smoothing".to_string(),
            value: value.clamp(0.0, 0.999),
        },
    );
}

#[tauri::command]
pub fn control_head_tracking_invert(state: State<SharedState>, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/head/tracking/invert".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}
