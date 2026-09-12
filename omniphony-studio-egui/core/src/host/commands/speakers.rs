//! Speaker-layout and room-geometry controls: room ratios, per-speaker geometry
//! (position, delay, spatialization, crossover, name) and the add/remove/move/
//! apply operations on the layout.
//!
//! Each command forwards a value to the renderer over OSC.

use super::OscControlMsg;
use super::{SharedState, send_control, send_json_control};

pub fn control_room_ratio(state: &SharedState, width: f32, length: f32, height: f32) {
    let w = width.max(0.01);
    let l = length.max(0.01);
    let h = height.max(0.01);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloats3 {
            address: "/omniphony/control/room_ratio".to_string(),
            a: w,
            b: l,
            c: h,
        },
    );
}

pub fn control_room_ratio_rear(state: &SharedState, value: f32) {
    let v = value.max(0.01);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/room_ratio_rear".to_string(),
            value: v,
        },
    );
}

pub fn control_room_ratio_lower(state: &SharedState, value: f32) {
    let v = value.max(0.01);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/room_ratio_lower".to_string(),
            value: v,
        },
    );
}

pub fn control_room_ratio_center_blend(state: &SharedState, value: f32) {
    let v = value.clamp(0.0, 1.0);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/room_ratio_center_blend".to_string(),
            value: v,
        },
    );
}

pub fn control_layout_radius_m(state: &SharedState, value: f32) {
    let v = value.max(0.01);
    send_json_control(
        &state.osc_tx,
        "/omniphony/control/config/layout",
        serde_json::json!({ "radiusM": v }),
    );
}

pub fn control_layout_config(state: &SharedState, payload: serde_json::Value) {
    send_json_control(&state.osc_tx, "/omniphony/control/config/layout", payload);
}

pub fn control_layout_config_apply(state: &SharedState) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/config/layout/apply".to_string(),
        },
    );
}

pub fn control_speakers_config(state: &SharedState, payload: serde_json::Value) {
    send_json_control(&state.osc_tx, "/omniphony/control/config/speakers", payload);
}

pub fn control_speaker_az(state: &SharedState, id: i32, value: f32) {
    send_json_control(
        &state.osc_tx,
        "/omniphony/control/config/layout",
        serde_json::json!({ "speakerEdits": [{ "id": id.max(0), "azimuth": value }] }),
    );
}

pub fn control_speaker_el(state: &SharedState, id: i32, value: f32) {
    send_json_control(
        &state.osc_tx,
        "/omniphony/control/config/layout",
        serde_json::json!({ "speakerEdits": [{ "id": id.max(0), "elevation": value }] }),
    );
}

pub fn control_speaker_distance(state: &SharedState, id: i32, value: f32) {
    send_json_control(
        &state.osc_tx,
        "/omniphony/control/config/layout",
        serde_json::json!({ "speakerEdits": [{ "id": id.max(0), "distance": value.max(0.01) }] }),
    );
}

pub fn control_speaker_x(state: &SharedState, id: i32, value: f32) {
    send_json_control(
        &state.osc_tx,
        "/omniphony/control/config/layout",
        serde_json::json!({ "speakerEdits": [{ "id": id.max(0), "x": value.clamp(-1.0, 1.0) }] }),
    );
}

pub fn control_speaker_y(state: &SharedState, id: i32, value: f32) {
    send_json_control(
        &state.osc_tx,
        "/omniphony/control/config/layout",
        serde_json::json!({ "speakerEdits": [{ "id": id.max(0), "y": value.clamp(-1.0, 1.0) }] }),
    );
}

pub fn control_speaker_z(state: &SharedState, id: i32, value: f32) {
    send_json_control(
        &state.osc_tx,
        "/omniphony/control/config/layout",
        serde_json::json!({ "speakerEdits": [{ "id": id.max(0), "z": value.clamp(-1.0, 1.0) }] }),
    );
}

pub fn control_speaker_coord_mode(state: &SharedState, id: i32, value: String) {
    let normalized = if value.trim().eq_ignore_ascii_case("cartesian") {
        "cartesian"
    } else {
        "polar"
    };
    send_json_control(
        &state.osc_tx,
        "/omniphony/control/config/layout",
        serde_json::json!({ "speakerEdits": [{ "id": id.max(0), "coordMode": normalized }] }),
    );
}

pub fn control_speaker_delay(state: &SharedState, id: i32, delay_ms: f32) {
    let v = delay_ms.max(0.0);
    send_json_control(
        &state.osc_tx,
        "/omniphony/control/config/speakers",
        serde_json::json!({ "speakerEdits": [{ "id": id.max(0), "delayMs": v }] }),
    );
}

pub fn control_speaker_spatialize(state: &SharedState, id: i32, spatialize: i32) {
    send_json_control(
        &state.osc_tx,
        "/omniphony/control/config/layout",
        serde_json::json!({ "speakerEdits": [{ "id": id.max(0), "spatialize": spatialize != 0 }] }),
    );
}

pub fn control_speaker_name(state: &SharedState, id: i32, name: String) {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return;
    }
    send_json_control(
        &state.osc_tx,
        "/omniphony/control/config/layout",
        serde_json::json!({ "speakerEdits": [{ "id": id.max(0), "name": trimmed }] }),
    );
}

pub fn control_speaker_freq_low(state: &SharedState, id: i32, freq_low: f32) {
    let value = if freq_low > 0.0 {
        serde_json::Value::from(freq_low)
    } else {
        serde_json::Value::Null
    };
    send_json_control(
        &state.osc_tx,
        "/omniphony/control/config/layout",
        serde_json::json!({ "speakerEdits": [{ "id": id.max(0), "freqLow": value }] }),
    );
}

pub fn control_speaker_freq_high(state: &SharedState, id: i32, freq_high: f32) {
    let value = if freq_high > 0.0 {
        serde_json::Value::from(freq_high)
    } else {
        serde_json::Value::Null
    };
    send_json_control(
        &state.osc_tx,
        "/omniphony/control/config/layout",
        serde_json::json!({ "speakerEdits": [{ "id": id.max(0), "freqHigh": value }] }),
    );
}

pub fn control_speakers_apply(state: &SharedState) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/config/layout/apply".to_string(),
        },
    );
}

pub fn control_speakers_add(
    state: &SharedState,
    name: String,
    azimuth: f32,
    elevation: f32,
    distance: f32,
    spatialize: i32,
    delay_ms: f32,
) {
    let n = if name.trim().is_empty() {
        "speaker"
    } else {
        name.trim()
    };
    send_json_control(
        &state.osc_tx,
        "/omniphony/control/config/layout",
        serde_json::json!({
            "addSpeaker": {
                "name": n,
                "azimuth": azimuth,
                "elevation": elevation,
                "distance": distance.max(0.01),
                "spatialize": spatialize != 0,
                "delayMs": delay_ms.max(0.0)
            }
        }),
    );
    control_speakers_apply(state);
}

pub fn control_speakers_remove(state: &SharedState, index: i32) {
    send_json_control(
        &state.osc_tx,
        "/omniphony/control/config/layout",
        serde_json::json!({ "removeSpeaker": index.max(0) }),
    );
    control_speakers_apply(state);
}

pub fn control_speakers_move(state: &SharedState, from: i32, to: i32) {
    send_json_control(
        &state.osc_tx,
        "/omniphony/control/config/layout",
        serde_json::json!({ "moveSpeaker": { "from": from.max(0), "to": to.max(0) } }),
    );
    control_speakers_apply(state);
}
