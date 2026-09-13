//! Binaural (headphone) output controls: output-mode toggle, HRIR source, and
//! the Sensors2OSC head-tracking settings (address, format, smoothing, invert,
//! recenter).
//!
//! Each command forwards a value to the renderer over OSC.

use super::OscControlMsg;
use super::{SharedState, send_control};
use crate::osc_contract;

pub fn control_output_mode(state: &SharedState, value: String) {
    let normalized = value.trim().to_ascii_lowercase();
    if !matches!(normalized.as_str(), "speaker" | "binaural") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_OUTPUT_MODE.to_string(),
            value: normalized,
        },
    );
}

pub fn control_binaural_mode(state: &SharedState, value: String) {
    // Binaural stage input: per-object HRTF ("direct") or the fixed
    // virtual-speaker cascade ("cascaded").
    let normalized = value.trim().to_ascii_lowercase();
    if !matches!(normalized.as_str(), "direct" | "cascaded") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_BINAURAL_MODE.to_string(),
            value: normalized,
        },
    );
}

pub fn control_ear_gain(state: &SharedState, ear: u32, value: f32) {
    // Headphone L/R output gain: dedicated ear params (the ears no longer
    // ride the first two per-speaker slots).
    if ear > 1 || !value.is_finite() {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_BINAURAL_EAR_GAIN.to_string(),
            args: vec![
                rosc::OscType::Int(ear as i32),
                rosc::OscType::Float(value.clamp(0.0, 4.0)),
            ],
        },
    );
}

pub fn control_ear_mute(state: &SharedState, ear: u32, muted: bool) {
    // Headphone L/R mute (solo is composed client-side from the two mutes).
    if ear > 1 {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_BINAURAL_EAR_MUTE.to_string(),
            args: vec![
                rosc::OscType::Int(ear as i32),
                rosc::OscType::Int(if muted { 1 } else { 0 }),
            ],
        },
    );
}

pub fn control_hrir_source(state: &SharedState, value: String) {
    // "synthetic" | "saf"/"kemar" | "sofa:<path>".
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_BINAURAL_HRIR_SOURCE.to_string(),
            value: value.trim().to_string(),
        },
    );
}

pub fn control_binaural_unit_scale(state: &SharedState, value: f32) {
    // Metres per ADM unit (isotropic distance scale).
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: osc_contract::CONTROL_BINAURAL_UNIT_SCALE.to_string(),
            value: value.clamp(0.01, 100.0),
        },
    );
}

pub fn control_binaural_head_radius(state: &SharedState, value: f32) {
    // Effective head radius in metres (half the inter-ear distance).
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: osc_contract::CONTROL_BINAURAL_HEAD_RADIUS.to_string(),
            value: value.clamp(0.05, 0.15),
        },
    );
}

pub fn control_binaural_reflections_enabled(state: &SharedState, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: osc_contract::CONTROL_BINAURAL_REFLECTIONS_ENABLED.to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

pub fn control_binaural_reflections_level(state: &SharedState, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: osc_contract::CONTROL_BINAURAL_REFLECTIONS_LEVEL.to_string(),
            value: value.clamp(0.0, 1.0),
        },
    );
}

pub fn control_binaural_reflections_wall_cutoff(state: &SharedState, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: osc_contract::CONTROL_BINAURAL_REFLECTIONS_WALL_CUTOFF.to_string(),
            value: value.clamp(1_000.0, 20_000.0),
        },
    );
}

pub fn control_binaural_reflections_room(state: &SharedState, axis: String, value: f32) {
    // axis: "width" | "depth" | "height"; value in metres.
    let address = match axis.as_str() {
        "width" => osc_contract::CONTROL_BINAURAL_REFLECTIONS_ROOM_WIDTH,
        "depth" => osc_contract::CONTROL_BINAURAL_REFLECTIONS_ROOM_DEPTH,
        "height" => osc_contract::CONTROL_BINAURAL_REFLECTIONS_ROOM_HEIGHT,
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

pub fn control_binaural_reverb_enabled(state: &SharedState, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: osc_contract::CONTROL_BINAURAL_REVERB_ENABLED.to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

pub fn control_binaural_reverb_level(state: &SharedState, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: osc_contract::CONTROL_BINAURAL_REVERB_LEVEL.to_string(),
            value: value.clamp(0.0, 1.0),
        },
    );
}

pub fn control_binaural_reverb_rt60(state: &SharedState, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: osc_contract::CONTROL_BINAURAL_REVERB_RT60.to_string(),
            value: value.clamp(0.1, 3.0),
        },
    );
}

pub fn control_binaural_reverb_size(state: &SharedState, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: osc_contract::CONTROL_BINAURAL_REVERB_SIZE.to_string(),
            value: value.clamp(0.5, 2.0),
        },
    );
}

pub fn control_binaural_reverb_rt60_low_ratio(state: &SharedState, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: osc_contract::CONTROL_BINAURAL_REVERB_RT60_LOW_RATIO.to_string(),
            value: value.clamp(0.25, 4.0),
        },
    );
}

pub fn control_binaural_reverb_rt60_high_ratio(state: &SharedState, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: osc_contract::CONTROL_BINAURAL_REVERB_RT60_HIGH_RATIO.to_string(),
            value: value.clamp(0.25, 4.0),
        },
    );
}

pub fn control_binaural_diffuse_field_eq(state: &SharedState, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: osc_contract::CONTROL_BINAURAL_DIFFUSE_FIELD_EQ.to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

pub fn control_binaural_air_absorption(state: &SharedState, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: osc_contract::CONTROL_BINAURAL_AIR_ABSORPTION.to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

pub fn control_head_calibrate(state: &SharedState, step: String) {
    // step: "front" | "left" | "up" | "reset".
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_HEAD_CALIBRATE.to_string(),
            value: step,
        },
    );
}

pub fn control_head_recenter(state: &SharedState) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: osc_contract::CONTROL_HEAD_RECENTER.to_string(),
            value: 1,
        },
    );
}

pub fn control_head_tracking_address(state: &SharedState, value: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_HEAD_TRACKING_ADDRESS.to_string(),
            value: value.trim().to_string(),
        },
    );
}

pub fn control_head_tracking_format(state: &SharedState, value: String) {
    let normalized = value.trim().to_ascii_lowercase();
    if !matches!(normalized.as_str(), "auto" | "quat" | "rotvec" | "euler") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: osc_contract::CONTROL_HEAD_TRACKING_FORMAT.to_string(),
            value: normalized,
        },
    );
}

pub fn control_head_tracking_smoothing(state: &SharedState, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: osc_contract::CONTROL_HEAD_TRACKING_SMOOTHING.to_string(),
            value: value.clamp(0.0, 0.999),
        },
    );
}

pub fn control_head_tracking_invert(state: &SharedState, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: osc_contract::CONTROL_HEAD_TRACKING_INVERT.to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}
