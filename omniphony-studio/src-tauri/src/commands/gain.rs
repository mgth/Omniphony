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

/// Start or stop the per-speaker test signal (band-limited pink noise).
///
/// `id < 0` stops any running test. The trigger policy — hold, fixed burst or
/// toggle — lives in the UI, so this is the whole renderer-facing contract:
/// start this speaker, or stop.
/// Arm/disarm the speaker-test idle feed: while armed the renderer fabricates
/// silence input frames when nothing is playing, so the output chain is warm
/// and a test is heard immediately. The arm expires renderer-side after a
/// keepalive window; the UI re-sends it periodically while the Test pane is
/// open.
#[tauri::command]
pub fn control_speaker_test_idle_feed(state: State<SharedState>, enable: bool) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/speaker_test/idle_feed".to_string(),
            value: if enable { 1 } else { 0 },
        },
    );
}

/// Place (or stop) the object test signal.
///
/// Sent once per pointer move while the user drags the object across a face, so
/// it stays a plain fire-and-forget message: the renderer ramps to the new
/// position without restarting the noise, which is what makes dragging audible
/// as movement rather than as a series of clicks.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn control_object_test(
    state: State<SharedState>,
    on: bool,
    x: f32,
    y: f32,
    z: f32,
    level: f32,
    size: f32,
    isolation: String,
) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: "/omniphony/control/object_test".to_string(),
            args: vec![
                rosc::OscType::Int(i32::from(on)),
                // Clamped here as well as renderer-side, same reasoning as the
                // speaker test: a stray UI value must not reach the audio path.
                rosc::OscType::Float(x.clamp(-1.0, 1.0)),
                rosc::OscType::Float(y.clamp(-1.0, 1.0)),
                rosc::OscType::Float(z.clamp(-1.0, 1.0)),
                rosc::OscType::Float(level.clamp(0.0, 1.0)),
                rosc::OscType::Float(size.clamp(0.0, 1.0)),
                rosc::OscType::String(isolation),
            ],
        },
    );
}

/// Set the object test's orbit.
///
/// Its own command rather than more arguments on `control_object_test`, for the
/// same reason the OSC address is separate: that one fires on every pointer
/// move while dragging, and this changes only when a knob does.
#[tauri::command]
pub fn control_object_test_rotation(
    state: State<SharedState>,
    axis: String,
    radius: f32,
    period: f32,
    azimuth: f32,
    elevation: f32,
) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: "/omniphony/control/object_test/rotation".to_string(),
            args: vec![
                rosc::OscType::String(axis),
                rosc::OscType::Float(radius.clamp(0.0, 4.0)),
                rosc::OscType::Float(period.clamp(0.05, 600.0)),
                rosc::OscType::Float(azimuth),
                rosc::OscType::Float(elevation),
            ],
        },
    );
}

#[tauri::command]
pub fn control_speaker_test(state: State<SharedState>, id: i32, level: f32, isolation: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: "/omniphony/control/speaker_test".to_string(),
            args: vec![
                rosc::OscType::Int(id),
                // Clamped here as well as renderer-side: this drives a speaker,
                // and a stray value from the UI must not reach the audio path.
                rosc::OscType::Float(level.clamp(0.0, 1.0)),
                rosc::OscType::String(isolation),
            ],
        },
    );
}
