//! Gain & mute controls: per-speaker / per-object / master gain, object and
//! speaker mute, loudness compensation and auto-gain. The realtime gain commands
//! stamp a monotonic sequence number so the renderer can drop stale updates.
//!
//! Each command forwards a value to the renderer over OSC.

use crate::osc_listener::OscControlMsg;
use crate::{send_control, send_json_control, SharedState};
use std::sync::atomic::Ordering;
use tauri::State;

#[tauri::command]
pub fn control_speaker_gain(state: State<SharedState>, id: i32, gain: f32) {
    let clamped = gain.max(0.0).min(2.0);
    let seq = state.realtime_seq.fetch_add(1, Ordering::Relaxed) + 1;
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: "/omniphony/control/realtime/speaker_gain".to_string(),
            args: vec![
                rosc::OscType::Int(id),
                rosc::OscType::Float(clamped),
                rosc::OscType::Int(seq),
            ],
        },
    );
}

#[tauri::command]
pub fn control_object_mute(state: State<SharedState>, id: i32, muted: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: format!("/omniphony/control/object/{id}/mute"),
            value: if muted != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
pub fn control_speaker_mute(state: State<SharedState>, id: i32, muted: i32) {
    send_json_control(
        &state.osc_tx,
        "/omniphony/control/config/speakers",
        serde_json::json!({
            "speakerEdits": [{
                "id": id.max(0),
                "muted": muted != 0
            }]
        }),
    );
}

#[tauri::command]
pub fn control_master_gain(state: State<SharedState>, gain: f32) {
    let clamped = gain.max(0.0).min(2.0);
    let seq = state.realtime_seq.fetch_add(1, Ordering::Relaxed) + 1;
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: "/omniphony/control/realtime/master_gain".to_string(),
            args: vec![rosc::OscType::Float(clamped), rosc::OscType::Int(seq)],
        },
    );
}

#[tauri::command]
pub fn control_loudness(state: State<SharedState>, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/loudness".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
pub fn control_auto_gain(state: State<SharedState>, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/auto_gain".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
pub fn control_auto_gain_ceiling(state: State<SharedState>, db: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/auto_gain_ceiling".to_string(),
            value: db.clamp(-12.0, 0.0),
        },
    );
}
