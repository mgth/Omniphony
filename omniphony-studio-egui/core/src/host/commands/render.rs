//! Spatialization / render-backend controls: spread, distance model & diffuse,
//! hybrid backend, the generic backend param setter, and the precomputed-table
//! (cartesian / polar) evaluation settings.
//!
//! Each command forwards a value to the renderer over OSC.

use omniphony_geometry::f64 as geometry;

use super::OscControlMsg;
use super::{SharedState, send_control, send_distance_metric};

pub fn control_spread_min(state: &SharedState, value: f32) {
    let clamped = value.max(0.0).min(1.0);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/spread/min".to_string(),
            value: clamped,
        },
    );
}

pub fn control_spread_max(state: &SharedState, value: f32) {
    let clamped = value.max(0.0).min(1.0);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/spread/max".to_string(),
            value: clamped,
        },
    );
}

pub fn control_spread_from_distance(state: &SharedState, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/spread/from_distance".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

pub fn control_size_to_spread_mode(state: &SharedState, value: String) {
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

pub fn control_spread_distance_range(state: &SharedState, value: f32) {
    let v = value.max(0.01);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/spread/distance_range".to_string(),
            value: v,
        },
    );
}

pub fn control_spread_distance_curve(state: &SharedState, value: f32) {
    let v = value.max(0.0);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/spread/distance_curve".to_string(),
            value: v,
        },
    );
}

pub fn control_distance_model(state: &SharedState, value: String) {
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

pub fn control_distance_model_metric(state: &SharedState, value: String) {
    send_distance_metric(&state, "/omniphony/control/distance_model_metric", value);
}

pub fn control_distance_diffuse_metric(state: &SharedState, value: String) {
    send_distance_metric(&state, "/omniphony/control/distance_diffuse/metric", value);
}

/// Axes negated to build the diffuse mirror, as the letters to flip (`xy`, `y`,
/// `xyz`) or `none`. Validated here so a malformed value never reaches the OSC
/// bus; the renderer parses the same grammar.
pub fn control_distance_diffuse_mirror_axes(state: &SharedState, value: String) {
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

/// Eight seconds without a `vbap:recomputing` broadcast is an unanswered
/// recompute (`markRecomputePending`).
pub const RECOMPUTE_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(8);

/// `markRecomputePending`: assume the engine will recompute, and arm the
/// deadline that complains if it never says it did. Every command that
/// re-plans the layout calls this as it sends.
pub fn mark_recompute_pending(state: &SharedState) {
    let mut live = state.inner.lock().unwrap();
    live.app.vbap_recomputing = Some(true);
    live.app.recompute_error = None;
    live.recompute_timed_out = false;
    live.recompute_deadline = Some(std::time::Instant::now() + RECOMPUTE_ACK_TIMEOUT);
}

/// Raise the no-answer flag once the deadline passes, and say when to look
/// again. `None` means nothing is pending — the shape the core's services take
/// in phase 3 of the boundary plan.
pub fn tick_recompute(state: &SharedState, now: std::time::Instant) -> Option<std::time::Instant> {
    let mut live = state.inner.lock().unwrap();
    let deadline = live.recompute_deadline?;
    if live.app.vbap_recomputing != Some(true) {
        live.recompute_deadline = None;
        return None;
    }
    if now < deadline {
        return Some(deadline);
    }
    live.app.vbap_recomputing = Some(false);
    live.recompute_timed_out = true;
    live.recompute_deadline = None;
    None
}

pub fn control_hybrid_external_backend(state: &SharedState, value: String) {
    if let Some(normalized) = valid_hybrid_inner_id(&value) {
        state
            .inner
            .lock()
            .unwrap()
            .app
            .render_backend_state
            .hybrid
            .external_backend = Some(normalized.clone());
        mark_recompute_pending(state);
        send_control(
            &state.osc_tx,
            OscControlMsg::SendString {
                address: "/omniphony/control/hybrid/external_backend".to_string(),
                value: normalized,
            },
        );
    }
}

pub fn control_hybrid_internal_backend(state: &SharedState, value: String) {
    if let Some(normalized) = valid_hybrid_inner_id(&value) {
        state
            .inner
            .lock()
            .unwrap()
            .app
            .render_backend_state
            .hybrid
            .internal_backend = Some(normalized.clone());
        mark_recompute_pending(state);
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

pub fn control_hybrid_metric(state: &SharedState, value: String) {
    let normalized = value.trim().to_ascii_lowercase();
    if !matches!(normalized.as_str(), "spherical" | "chebyshev") {
        return;
    }
    state
        .inner
        .lock()
        .unwrap()
        .app
        .render_backend_state
        .hybrid
        .metric = Some(normalized.clone());
    mark_recompute_pending(state);
    send_distance_metric(state, "/omniphony/control/hybrid/metric", normalized);
}

pub fn control_hybrid_curve_smoothing(state: &SharedState, value: f32) {
    let clamped = value.clamp(0.0, 1.0);
    state
        .inner
        .lock()
        .unwrap()
        .app
        .render_backend_state
        .hybrid
        .curve_smoothing = Some(f64::from(clamped));
    mark_recompute_pending(state);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/hybrid/curve_smoothing".to_string(),
            value: clamped,
        },
    );
}

/// The curve, as the editor holds it: the model keeps the points it dragged,
/// the renderer is sent the same list flattened and clamped.
pub fn set_hybrid_curve(state: &SharedState, points: Vec<[f64; 2]>) {
    state
        .inner
        .lock()
        .unwrap()
        .app
        .render_backend_state
        .hybrid
        .curve = points.clone();
    mark_recompute_pending(state);
    control_hybrid_curve(
        state,
        points
            .iter()
            .map(|p| [p[0] as f32, p[1] as f32])
            .collect::<Vec<_>>(),
    );
}

pub fn control_hybrid_curve(state: &SharedState, points: Vec<[f32; 2]>) {
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

pub fn control_render_evaluation_object_size_intervals(state: &SharedState, value: i32) {
    state.inner.lock().unwrap().app.object_size_intervals = value.max(0) as u32;
    mark_recompute_pending(state);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/object_size_intervals".to_string(),
            value: value.max(0),
        },
    );
}

pub fn control_render_evaluation_cartesian_x_size(state: &SharedState, value: i32) {
    mark_recompute_pending(state);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/cartesian/x_size".to_string(),
            value: value.max(1),
        },
    );
}

pub fn control_render_evaluation_cartesian_y_size(state: &SharedState, value: i32) {
    mark_recompute_pending(state);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/cartesian/y_size".to_string(),
            value: value.max(1),
        },
    );
}

pub fn control_render_evaluation_cartesian_z_size(state: &SharedState, value: i32) {
    mark_recompute_pending(state);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/cartesian/z_size".to_string(),
            value: value.max(1),
        },
    );
}

pub fn control_render_evaluation_cartesian_z_neg_size(state: &SharedState, value: i32) {
    mark_recompute_pending(state);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/cartesian/z_neg_size".to_string(),
            value: value.max(0),
        },
    );
}

pub fn control_render_backend(state: &SharedState, value: String) {
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
pub fn control_backend_param(
    state: &SharedState,
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
pub fn backend_file_get(state: &SharedState, backend: String, key: String, name: Option<String>) {
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
pub fn backend_file_list(state: &SharedState, backend: String) {
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
pub fn backend_file_put(
    state: &SharedState,
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

pub fn control_restore_render_backend(state: &SharedState) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_backend/restore".to_string(),
            value: 1,
        },
    );
}

pub fn control_render_evaluation_mode(state: &SharedState, value: String) {
    let normalized = value.trim().to_ascii_lowercase();
    if !matches!(
        normalized.as_str(),
        "auto" | "realtime" | "precomputed_polar" | "precomputed_cartesian"
    ) {
        return;
    }
    state
        .inner
        .lock()
        .unwrap()
        .app
        .render_evaluation_mode_state
        .selection = Some(normalized.clone());
    mark_recompute_pending(state);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/render_evaluation_mode".to_string(),
            value: normalized,
        },
    );
}

pub fn control_render_evaluation_polar_azimuth_resolution(state: &SharedState, value: i32) {
    mark_recompute_pending(state);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/polar/azimuth_resolution".to_string(),
            value: value.max(1),
        },
    );
}

pub fn control_render_evaluation_polar_elevation_resolution(state: &SharedState, value: i32) {
    mark_recompute_pending(state);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/polar/elevation_resolution".to_string(),
            value: value.max(1),
        },
    );
}

pub fn control_render_evaluation_polar_distance_res(state: &SharedState, value: i32) {
    mark_recompute_pending(state);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/polar/distance_res".to_string(),
            value: value.max(1),
        },
    );
}

pub fn control_render_evaluation_polar_distance_max(state: &SharedState, value: f32) {
    state.inner.lock().unwrap().app.vbap_polar.distance_max = Some(f64::from(value.max(0.01)));
    mark_recompute_pending(state);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/render_evaluation/polar/distance_max".to_string(),
            value: value.max(0.01),
        },
    );
}

pub fn control_render_evaluation_position_interpolation(state: &SharedState, enable: i32) {
    state
        .inner
        .lock()
        .unwrap()
        .app
        .vbap_polar
        .position_interpolation = Some(enable != 0);
    mark_recompute_pending(state);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/position_interpolation".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

pub fn control_distance_diffuse_enabled(state: &SharedState, enable: i32) {
    state.inner.lock().unwrap().app.distance_diffuse.enabled = Some(enable != 0);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/distance_diffuse/enabled".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

pub fn control_distance_diffuse_threshold(state: &SharedState, value: f32) {
    let v = value.max(0.01);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/distance_diffuse/threshold".to_string(),
            value: v,
        },
    );
}

pub fn control_distance_diffuse_curve(state: &SharedState, value: f32) {
    let v = value.max(0.0);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/distance_diffuse/curve".to_string(),
            value: v,
        },
    );
}

pub fn control_render_bridge_path(state: &SharedState, value: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/render/bridge_path".to_string(),
            value: value.trim().to_string(),
        },
    );
}

pub fn control_render_input_pipe(state: &SharedState, value: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/render/input_pipe".to_string(),
            value: value.trim().to_string(),
        },
    );
}

/// Node positions of the cartesian gain table, per axis, or `null` when no
/// cartesian grid is configured.
///
/// The Studio's snap-to-grid has to land on the nodes the renderer actually
/// samples at. The frontend used to rebuild them from the published interval
/// counts, which meant re-implementing two protocol conventions: that the
/// counts are intervals rather than nodes, and that the height axis is
/// asymmetric (an optional negative half at its own resolution, stopping short
/// of zero so both halves do not claim it). Both now come from
/// `omniphony-geometry`, which is also what the renderer builds the table with.
pub fn get_vbap_grid_nodes(state: &SharedState) -> Option<serde_json::Value> {
    let cartesian = {
        let s = state.inner.lock().unwrap();
        s.vbap_cartesian.clone()
    };

    // Intervals, as published. Anything under one interval is not a grid.
    let x_intervals = cartesian.x_size.unwrap_or(0);
    let y_intervals = cartesian.y_size.unwrap_or(0);
    let z_intervals = cartesian.z_size.unwrap_or(0);
    if x_intervals < 1 || y_intervals < 1 || z_intervals < 1 {
        return None;
    }
    let z_neg_nodes = cartesian.z_neg_size.unwrap_or(0) as usize;

    // Intervals -> nodes, the same conversion `live_params.rs` applies before
    // handing the counts to the table builder.
    let x = geometry::evenly_spaced_axis(x_intervals as usize + 1, -1.0, 1.0);
    let y = geometry::evenly_spaced_axis(y_intervals as usize + 1, -1.0, 1.0);
    let z = geometry::cartesian_z_axis(z_intervals as usize + 1, z_neg_nodes);

    Some(serde_json::json!({ "x": x, "y": y, "z": z }))
}

/// Sample the hybrid backend's blend curve at `count + 1` evenly spaced
/// positions across `[0, 1]`, for the editor's preview.
///
/// The preview used to be drawn by a second implementation of the curve living
/// in the frontend. Two implementations of the same function is two chances to
/// disagree, and a preview that disagrees with the audio path is worse than no
/// preview: it says the crossfade is somewhere it is not. This calls the same
/// `blend_curve_y` the renderer's `BlendCurve::eval` does.
pub fn sample_hybrid_curve(points: Vec<[f64; 2]>, smoothing: f64, count: usize) -> Vec<f64> {
    let count = count.clamp(1, 4096);
    (0..=count)
        .map(|i| {
            let x = i as f64 / count as f64;
            geometry::blend_curve_y(&points, smoothing, x)
        })
        .collect()
}
