//! mpv overlay IPC: trail prefs, active/labels/objects visibility and the energy
//! heatmap (enabled, bands, colormap, custom stops).
//!
//! Each command forwards a value to the renderer over OSC.

use super::OscControlMsg;
use super::{SharedState, send_control};

pub fn mpv_overlay_set_trail_prefs(
    state: &SharedState,
    enabled: bool,
    ttl_ms: u32,
    mode: String,
    teleport_threshold: f32,
) {
    // The mpv overlay is generated in-process by orender and owns its own
    // (persisted) display prefs, so trail config travels purely as OSC control.
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: "/omniphony/control/overlay/trails".to_string(),
            args: vec![
                rosc::OscType::Int(if enabled { 1 } else { 0 }),
                rosc::OscType::Int(ttl_ms as i32),
                rosc::OscType::String(mode),
                rosc::OscType::Float(teleport_threshold),
            ],
        },
    );
}

/// Show/hide the whole mpv overlay. The overlay is now drawn in-process by
/// orender, so the on/off toggle travels as OSC control to the renderer (not
/// over the mpv IPC socket, which doesn't reach an atmos-ranker-launched mpv).
pub fn mpv_overlay_set_active(state: &SharedState, enabled: bool) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/overlay/enabled".to_string(),
            value: if enabled { 1 } else { 0 },
        },
    );
}

/// Show/hide object labels in the mpv overlay (mirrors the 3D view's label
/// toggle). Travels as OSC control to the renderer.
pub fn mpv_overlay_set_labels(state: &SharedState, enabled: bool) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/overlay/labels".to_string(),
            value: if enabled { 1 } else { 0 },
        },
    );
}

/// Show/hide the objects (markers + labels + trails) on the mpv overlay — a
/// display-only switch that does not change the label/trail config. Travels as
/// OSC control to the renderer.
pub fn mpv_overlay_set_objects(state: &SharedState, visible: bool) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/overlay/objects".to_string(),
            value: if visible { 1 } else { 0 },
        },
    );
}

/// Enable/disable the mpv overlay's energy heatmap (mirrors Studio's "Object
/// energy field" toggle). Travels as OSC control to the renderer.
pub fn mpv_overlay_set_heatmap_enabled(state: &SharedState, enabled: bool) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/overlay/heatmap_enabled".to_string(),
            value: if enabled { 1 } else { 0 },
        },
    );
}

/// Set the number of depth planes in the mpv overlay's energy heatmap. Travels
/// as OSC control to the renderer.
pub fn mpv_overlay_set_heatmap_bands(state: &SharedState, count: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/overlay/heatmap_bands".to_string(),
            value: count.clamp(1, 12),
        },
    );
}

/// Set the energy-heatmap colour gradient in the mpv overlay. The index mirrors
/// Studio's `OBJECT_ENERGY_COLORMAPS` (0 heatmap, 1 blue→white, 2 white→red,
/// 3 red, 4 custom). Travels as OSC control to the renderer.
pub fn mpv_overlay_set_heatmap_colormap(state: &SharedState, colormap: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/overlay/heatmap_colormap".to_string(),
            value: colormap.clamp(0, 4),
        },
    );
}

/// Push the custom-gradient stops (used by colormap index 4) to the mpv overlay.
/// `stops` is a flat `[pos, r, g, b, …]` list in [0,1]; travels as OSC floats.
pub fn mpv_overlay_set_heatmap_custom_stops(state: &SharedState, stops: Vec<f32>) {
    let args = stops.into_iter().map(rosc::OscType::Float).collect();
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: "/omniphony/control/overlay/heatmap_custom_stops".to_string(),
            args,
        },
    );
}
