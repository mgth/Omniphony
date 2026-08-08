//! Spatialization / render-backend controls: spread, distance model & diffuse,
//! hybrid backend, the generic backend param setter, and the precomputed-table
//! (cartesian / polar) evaluation settings.
//!
//! Each command forwards a value to the renderer over OSC.

use crate::osc_listener::OscControlMsg;
use crate::{send_control, send_distance_metric, SharedState};
use tauri::State;

#[tauri::command]
pub fn control_spread_min(state: State<SharedState>, value: f32) {
    let clamped = value.max(0.0).min(1.0);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/spread/min".to_string(),
            value: clamped,
        },
    );
}

#[tauri::command]
pub fn control_spread_max(state: State<SharedState>, value: f32) {
    let clamped = value.max(0.0).min(1.0);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/spread/max".to_string(),
            value: clamped,
        },
    );
}

#[tauri::command]
pub fn control_spread_from_distance(state: State<SharedState>, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/spread/from_distance".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
pub fn control_size_to_spread_mode(state: State<SharedState>, value: String) {
    let normalized = value.trim().to_ascii_lowercase();
    if !matches!(
        normalized.as_str(),
        "max" | "mean" | "projection_perpendicular"
    ) {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/spread/size_to_spread_mode".to_string(),
            value: normalized,
        },
    );
}

#[tauri::command]
pub fn control_spread_distance_range(state: State<SharedState>, value: f32) {
    let v = value.max(0.01);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/spread/distance_range".to_string(),
            value: v,
        },
    );
}

#[tauri::command]
pub fn control_spread_distance_curve(state: State<SharedState>, value: f32) {
    let v = value.max(0.0);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/spread/distance_curve".to_string(),
            value: v,
        },
    );
}

#[tauri::command]
pub fn control_distance_model(state: State<SharedState>, value: String) {
    let normalized = value.trim().to_ascii_lowercase();
    if !matches!(
        normalized.as_str(),
        "none" | "linear" | "quadratic" | "inverse-square"
    ) {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/distance_model".to_string(),
            value: normalized,
        },
    );
}

#[tauri::command]
pub fn control_distance_model_metric(state: State<SharedState>, value: String) {
    send_distance_metric(&state, "/omniphony/control/distance_model_metric", value);
}

#[tauri::command]
pub fn control_distance_diffuse_metric(state: State<SharedState>, value: String) {
    send_distance_metric(&state, "/omniphony/control/distance_diffuse/metric", value);
}

/// Axes negated to build the diffuse mirror, as the letters to flip (`xy`, `y`,
/// `xyz`) or `none`. Validated here so a malformed value never reaches the OSC
/// bus; the renderer parses the same grammar.
#[tauri::command]
pub fn control_distance_diffuse_mirror_axes(state: State<SharedState>, value: String) {
    let normalized = value.trim().to_ascii_lowercase();
    let valid = normalized == "none"
        || (!normalized.is_empty() && normalized.chars().all(|c| matches!(c, 'x' | 'y' | 'z')));
    if !valid {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/distance_diffuse/mirror_axes".to_string(),
            value: normalized,
        },
    );
}

#[tauri::command]
pub fn control_hybrid_external_backend(state: State<SharedState>, value: String) {
    if let Some(normalized) = valid_hybrid_inner_id(&value) {
        send_control(
            &state.osc_tx,
            OscControlMsg::SendString {
                address: "/omniphony/control/hybrid/external_backend".to_string(),
                value: normalized,
            },
        );
    }
}

#[tauri::command]
pub fn control_hybrid_internal_backend(state: State<SharedState>, value: String) {
    if let Some(normalized) = valid_hybrid_inner_id(&value) {
        send_control(
            &state.osc_tx,
            OscControlMsg::SendString {
                address: "/omniphony/control/hybrid/internal_backend".to_string(),
                value: normalized,
            },
        );
    }
}

/// Normalise a hybrid inner-backend id and reject the only structurally invalid
/// choices (empty, or a nested `hybrid`). Any other id is forwarded; the renderer
/// validates it against its backend registry authoritatively.
fn valid_hybrid_inner_id(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    (!normalized.is_empty() && normalized != "hybrid").then_some(normalized)
}

#[tauri::command]
pub fn control_hybrid_metric(state: State<SharedState>, value: String) {
    send_distance_metric(&state, "/omniphony/control/hybrid/metric", value);
}

#[tauri::command]
pub fn control_hybrid_curve_smoothing(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/hybrid/curve_smoothing".to_string(),
            value: value.clamp(0.0, 1.0),
        },
    );
}

#[tauri::command]
pub fn control_hybrid_curve(state: State<SharedState>, points: Vec<[f32; 2]>) {
    // Flatten (x, y) control points into a single float list, clamped to [0, 1].
    let args = points
        .iter()
        .flat_map(|point| {
            [
                rosc::OscType::Float(point[0].clamp(0.0, 1.0)),
                rosc::OscType::Float(point[1].clamp(0.0, 1.0)),
            ]
        })
        .collect();
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: "/omniphony/control/hybrid/curve".to_string(),
            args,
        },
    );
}

#[tauri::command]
pub fn control_render_evaluation_object_size_intervals(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/object_size_intervals".to_string(),
            value: value.max(0),
        },
    );
}

#[tauri::command]
pub fn control_render_evaluation_cartesian_x_size(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/cartesian/x_size".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
pub fn control_render_evaluation_cartesian_y_size(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/cartesian/y_size".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
pub fn control_render_evaluation_cartesian_z_size(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/cartesian/z_size".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
pub fn control_render_evaluation_cartesian_z_neg_size(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/cartesian/z_neg_size".to_string(),
            value: value.max(0),
        },
    );
}

#[tauri::command]
pub fn control_render_backend(state: State<SharedState>, value: String) {
    // Forward any non-empty id; the engine validates it against its backend
    // registry (which includes contributor-registered backends), so we must not
    // hard-code the built-in set here.
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/render_backend".to_string(),
            value: normalized,
        },
    );
}

/// Generic backend param setter. The scalar type follows the JSON value (bool /
/// number / string), matching the param schema's kind. When `backend` is given,
/// the value is applied to that specific backend (e.g. a hybrid inner backend);
/// otherwise it targets the currently selected backend.
#[tauri::command]
pub fn control_backend_param(
    state: State<SharedState>,
    key: String,
    value: serde_json::Value,
    backend: Option<String>,
) {
    let arg = match value {
        serde_json::Value::Bool(b) => rosc::OscType::Bool(b),
        serde_json::Value::Number(n) => rosc::OscType::Float(n.as_f64().unwrap_or(0.0) as f32),
        serde_json::Value::String(s) => rosc::OscType::String(s),
        _ => return,
    };
    let args = match backend {
        Some(backend) => vec![
            rosc::OscType::String(backend),
            rosc::OscType::String(key),
            arg,
        ],
        None => vec![rosc::OscType::String(key), arg],
    };
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: "/omniphony/control/backend/param".to_string(),
            args,
        },
    );
}

/// Request the current content of an editable backend file from the renderer.
/// The renderer replies on `/omniphony/state/backend/file/content` (or `.../error`),
/// surfaced to the frontend by the OSC listener as a `backend-file-content` event.
#[tauri::command]
pub fn backend_file_get(
    state: State<SharedState>,
    backend: String,
    key: String,
    name: Option<String>,
) {
    let mut args = vec![rosc::OscType::String(backend), rosc::OscType::String(key)];
    if let Some(name) = name {
        // An explicit name previews any managed-store file; omitted, the renderer
        // reads the param's current handle.
        args.push(rosc::OscType::String(name));
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: "/omniphony/control/backend/file/get".to_string(),
            args,
        },
    );
}

/// Ask the renderer for the names of its managed files for `backend`. The reply
/// arrives on `/omniphony/state/backend/file/list` as a `backend-file-list` event.
#[tauri::command]
pub fn backend_file_list(state: State<SharedState>, backend: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: "/omniphony/control/backend/file/list".to_string(),
            args: vec![rosc::OscType::String(backend)],
        },
    );
}

/// Save `content` for an editable backend file under `name` on the renderer and
/// select it. The renderer writes its managed store (or, for a local renderer, an
/// absolute path), persists the handle and rebuilds the backend.
#[tauri::command]
pub fn backend_file_put(
    state: State<SharedState>,
    backend: String,
    key: String,
    name: String,
    content: String,
) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: "/omniphony/control/backend/file/put".to_string(),
            args: vec![
                rosc::OscType::String(backend),
                rosc::OscType::String(key),
                rosc::OscType::String(name),
                rosc::OscType::String(content),
            ],
        },
    );
}

#[tauri::command]
pub fn control_restore_render_backend(state: State<SharedState>) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_backend/restore".to_string(),
            value: 1,
        },
    );
}

#[tauri::command]
pub fn control_render_evaluation_mode(state: State<SharedState>, value: String) {
    let normalized = value.trim().to_ascii_lowercase();
    if !matches!(
        normalized.as_str(),
        "auto" | "realtime" | "precomputed_polar" | "precomputed_cartesian"
    ) {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/render_evaluation_mode".to_string(),
            value: normalized,
        },
    );
}

#[tauri::command]
pub fn control_render_evaluation_polar_azimuth_resolution(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/polar/azimuth_resolution".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
pub fn control_render_evaluation_polar_elevation_resolution(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/polar/elevation_resolution".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
pub fn control_render_evaluation_polar_distance_res(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/polar/distance_res".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
pub fn control_render_evaluation_polar_distance_max(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/render_evaluation/polar/distance_max".to_string(),
            value: value.max(0.01),
        },
    );
}

#[tauri::command]
pub fn control_render_evaluation_position_interpolation(state: State<SharedState>, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/position_interpolation".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
pub fn control_distance_diffuse_enabled(state: State<SharedState>, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/distance_diffuse/enabled".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
pub fn control_distance_diffuse_threshold(state: State<SharedState>, value: f32) {
    let v = value.max(0.01);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/distance_diffuse/threshold".to_string(),
            value: v,
        },
    );
}

#[tauri::command]
pub fn control_distance_diffuse_curve(state: State<SharedState>, value: f32) {
    let v = value.max(0.0);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/distance_diffuse/curve".to_string(),
            value: v,
        },
    );
}

#[tauri::command]
pub fn control_render_bridge_path(state: State<SharedState>, value: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/render/bridge_path".to_string(),
            value: value.trim().to_string(),
        },
    );
}

#[tauri::command]
pub fn control_render_input_pipe(state: State<SharedState>, value: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/render/input_pipe".to_string(),
            value: value.trim().to_string(),
        },
    );
}
