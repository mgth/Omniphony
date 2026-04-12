// Prevents an additional console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app_state;
mod config;
mod layouts;
mod osc_listener;
mod osc_parser;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::{fs, fs::File, process::Command as ProcessCommand, process::Stdio};

use app_state::AppState;
use config::{load_config, save_config, OscConfig};
use layouts::Layout;
use osc_listener::{spawn_osc_task, OscControlMsg};
use rfd::FileDialog;
use tauri::{Manager, State};
use tokio::sync::mpsc::UnboundedSender;

// ── shared state wrapper ──────────────────────────────────────────────────

struct SharedState {
    inner: Arc<Mutex<AppState>>,
    osc_tx: Arc<Mutex<Option<UnboundedSender<OscControlMsg>>>>,
    config_dir: PathBuf,
    listen_port: Arc<Mutex<u16>>,
}

// ── helper ────────────────────────────────────────────────────────────────

fn send_control(tx: &Arc<Mutex<Option<UnboundedSender<OscControlMsg>>>>, msg: OscControlMsg) {
    if let Some(tx) = tx.lock().unwrap().as_ref() {
        let _ = tx.send(msg);
    }
}

#[derive(serde::Serialize)]
struct AboutInfo {
    name: &'static str,
    version: &'static str,
    license: &'static str,
    repository_url: &'static str,
    description: &'static str,
}

#[derive(serde::Serialize)]
struct OrenderServiceStatus {
    installed: bool,
    running: bool,
    manager: &'static str,
}

struct OrenderLaunchSpec {
    orender_path: PathBuf,
    args: Vec<String>,
}

const ORENDER_SERVICE_NAME: &str = "omniphony-renderer";

// ── Tauri commands ────────────────────────────────────────────────────────

#[tauri::command]
fn get_state(state: State<SharedState>) -> serde_json::Value {
    let s = state.inner.lock().unwrap();
    serde_json::to_value(&*s).unwrap_or(serde_json::Value::Null)
}

#[tauri::command]
fn get_osc_config(state: State<SharedState>) -> OscConfig {
    load_config(&state.config_dir)
}

#[tauri::command]
fn get_about_info() -> AboutInfo {
    AboutInfo {
        name: "Omniphony Studio",
        version: env!("CARGO_PKG_VERSION"),
        license: "GPL-3.0-only",
        repository_url: "https://github.com/mgth/Omniphony",
        description: "Omniphony is an open spatial-audio project built around realtime rendering, transport, control, and monitoring tools for object-based audio workflows. Omniphony Studio is the visual control surface of that ecosystem.",
    }
}

#[tauri::command]
fn save_osc_config(state: State<SharedState>, config: OscConfig) -> Result<(), String> {
    save_config(&state.config_dir, &config)?;
    state.inner.lock().unwrap().osc_metering_enabled =
        Some(if config.osc_metering_enabled { 1 } else { 0 });
    send_control(
        &state.osc_tx,
        OscControlMsg::SetMeteringEnabled {
            enabled: config.osc_metering_enabled,
        },
    );
    let listen_port = *state.listen_port.lock().unwrap();
    send_control(
        &state.osc_tx,
        OscControlMsg::Reconnect {
            host: config.host,
            rx_port: config.osc_rx_port,
            listen_port,
        },
    );
    Ok(())
}

#[tauri::command]
fn control_osc_metering(state: State<SharedState>, enable: i32) -> Result<(), String> {
    let enabled = enable != 0;
    let mut cfg = load_config(&state.config_dir);
    cfg.osc_metering_enabled = enabled;
    save_config(&state.config_dir, &cfg)?;
    state.inner.lock().unwrap().osc_metering_enabled = Some(if enabled { 1 } else { 0 });
    send_control(&state.osc_tx, OscControlMsg::SetMeteringEnabled { enabled });
    Ok(())
}

#[tauri::command]
fn select_layout(state: State<SharedState>, key: String) -> bool {
    let mut s = state.inner.lock().unwrap();
    let exists = s.layouts.iter().any(|l| l.key == key);
    if exists {
        s.selected_layout_key = Some(key);
    }
    exists
}

#[tauri::command]
fn import_layout_from_path(
    state: State<SharedState>,
    path: String,
) -> Result<serde_json::Value, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("empty layout path".to_string());
    }
    let mut layout = layouts::load_layout_file(std::path::Path::new(trimmed))
        .ok_or_else(|| "failed to parse layout file".to_string())?;

    let mut s = state.inner.lock().unwrap();
    let base_key = layout.key.clone();
    let mut suffix = 1usize;
    while s.layouts.iter().any(|l| l.key == layout.key) {
        layout.key = format!("{base_key}-{}", suffix);
        suffix += 1;
    }
    s.selected_layout_key = Some(layout.key.clone());
    s.layouts.push(layout);
    s.layouts
        .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    Ok(serde_json::json!({
        "layouts": s.layouts,
        "selectedLayoutKey": s.selected_layout_key
    }))
}

#[tauri::command]
fn pick_import_layout_path() -> Option<String> {
    FileDialog::new()
        .add_filter("Layout", &["json", "yaml", "yml"])
        .pick_file()
        .map(|path| path.to_string_lossy().to_string())
}

#[tauri::command]
fn pick_export_layout_path(suggested_name: Option<String>) -> Option<String> {
    let file_name = suggested_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            let lowered = s.to_ascii_lowercase();
            if lowered.ends_with(".yaml") || lowered.ends_with(".yml") || lowered.ends_with(".json")
            {
                s.to_string()
            } else {
                format!("{s}.yaml")
            }
        })
        .unwrap_or_else(|| "layout.yaml".to_string());

    FileDialog::new()
        .add_filter("Layout YAML", &["yaml", "yml"])
        .add_filter("Layout JSON", &["json"])
        .set_file_name(&file_name)
        .save_file()
        .map(|path| path.to_string_lossy().to_string())
}

#[tauri::command]
fn pick_import_evaluation_artifact_path() -> Option<String> {
    FileDialog::new()
        .add_filter("Omniphony evaluator", &["oevl"])
        .pick_file()
        .map(|path| path.to_string_lossy().to_string())
}

#[tauri::command]
fn pick_export_evaluation_artifact_path(suggested_name: Option<String>) -> Option<String> {
    let file_name = suggested_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            let lowered = s.to_ascii_lowercase();
            if lowered.ends_with(".oevl") {
                s.to_string()
            } else {
                format!("{s}.oevl")
            }
        })
        .unwrap_or_else(|| "evaluation.oevl".to_string());

    FileDialog::new()
        .add_filter("Omniphony evaluator", &["oevl"])
        .set_file_name(&file_name)
        .save_file()
        .map(|path| path.to_string_lossy().to_string())
}

#[tauri::command]
fn pick_bridge_path() -> Option<String> {
    FileDialog::new()
        .set_title("Select bridge library")
        .pick_file()
        .map(|path| path.to_string_lossy().to_string())
}

#[tauri::command]
fn pick_orender_path() -> Option<String> {
    FileDialog::new()
        .set_title("Select orender executable")
        .pick_file()
        .map(|path| path.to_string_lossy().to_string())
}

#[tauri::command]
fn export_layout_to_path(path: String, layout: Layout) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("empty export path".to_string());
    }

    layouts::save_layout_file(std::path::Path::new(trimmed), &layout)
}

#[tauri::command]
fn control_object_gain(state: State<SharedState>, id: i32, gain: f32) {
    let clamped = gain.max(0.0).min(2.0);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: format!("/omniphony/control/object/{id}/gain"),
            value: clamped,
        },
    );
}

#[tauri::command]
fn control_speaker_gain(state: State<SharedState>, id: i32, gain: f32) {
    let clamped = gain.max(0.0).min(2.0);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: format!("/omniphony/control/speaker/{id}/gain"),
            value: clamped,
        },
    );
}

#[tauri::command]
fn control_object_mute(state: State<SharedState>, id: i32, muted: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: format!("/omniphony/control/object/{id}/mute"),
            value: if muted != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
fn control_speaker_mute(state: State<SharedState>, id: i32, muted: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: format!("/omniphony/control/speaker/{id}/mute"),
            value: if muted != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
fn control_master_gain(state: State<SharedState>, gain: f32) {
    let clamped = gain.max(0.0).min(2.0);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/gain".to_string(),
            value: clamped,
        },
    );
}

#[tauri::command]
fn control_loudness(state: State<SharedState>, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/loudness".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
fn control_adaptive_resampling(state: State<SharedState>, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
fn control_adaptive_resampling_enable_far_mode(state: State<SharedState>, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling/enable_far_mode".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
fn control_adaptive_resampling_force_silence_in_far_mode(state: State<SharedState>, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling/force_silence_in_far_mode".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
fn control_adaptive_resampling_hard_recover_high_in_far_mode(
    state: State<SharedState>,
    enable: i32,
) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling/hard_recover_high_in_far_mode"
                .to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
fn control_adaptive_resampling_hard_recover_low_in_far_mode(
    state: State<SharedState>,
    enable: i32,
) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling/hard_recover_low_in_far_mode"
                .to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
fn control_adaptive_resampling_far_mode_return_fade_in_ms(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling/far_mode_return_fade_in_ms"
                .to_string(),
            value: value.max(0),
        },
    );
}

#[tauri::command]
fn control_latency_target(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/latency_target".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
fn control_adaptive_resampling_kp_near(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/adaptive_resampling/kp_near".to_string(),
            value: value.max(0.00000001),
        },
    );
}

#[tauri::command]
fn control_adaptive_resampling_ki(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/adaptive_resampling/ki".to_string(),
            value: value.max(0.00000001),
        },
    );
}

#[tauri::command]
fn control_adaptive_resampling_integral_discharge_ratio(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/adaptive_resampling/integral_discharge_ratio".to_string(),
            value: value.clamp(0.0, 1.0),
        },
    );
}

#[tauri::command]
fn control_adaptive_resampling_max_adjust(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/adaptive_resampling/max_adjust".to_string(),
            value: value.max(0.000001),
        },
    );
}

#[tauri::command]
fn control_adaptive_resampling_update_interval_callbacks(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling/update_interval_callbacks".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
fn control_adaptive_resampling_near_far_threshold_ms(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling/near_far_threshold_ms".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
fn control_adaptive_resampling_pause(state: State<SharedState>, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling/pause".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
fn control_adaptive_resampling_reset_ratio(state: State<SharedState>) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling/reset_ratio".to_string(),
            value: 1,
        },
    );
}

#[tauri::command]
fn control_spread_min(state: State<SharedState>, value: f32) {
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
fn control_spread_max(state: State<SharedState>, value: f32) {
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
fn control_spread_from_distance(state: State<SharedState>, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/spread/from_distance".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
fn control_spread_distance_range(state: State<SharedState>, value: f32) {
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
fn control_spread_distance_curve(state: State<SharedState>, value: f32) {
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
fn control_distance_model(state: State<SharedState>, value: String) {
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
fn control_experimental_distance_distance_floor(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/experimental_distance/distance_floor".to_string(),
            value: value.max(0.0),
        },
    );
}

#[tauri::command]
fn control_experimental_distance_min_active_speakers(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/experimental_distance/min_active_speakers".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
fn control_experimental_distance_max_active_speakers(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/experimental_distance/max_active_speakers".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
fn control_experimental_distance_position_error_floor(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/experimental_distance/position_error_floor".to_string(),
            value: value.max(0.0),
        },
    );
}

#[tauri::command]
fn control_experimental_distance_position_error_nearest_scale(
    state: State<SharedState>,
    value: f32,
) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/experimental_distance/position_error_nearest_scale"
                .to_string(),
            value: value.max(0.0),
        },
    );
}

#[tauri::command]
fn control_experimental_distance_position_error_span_scale(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/experimental_distance/position_error_span_scale"
                .to_string(),
            value: value.max(0.0),
        },
    );
}

#[tauri::command]
fn control_render_evaluation_cartesian_x_size(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/cartesian/x_size".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
fn control_render_evaluation_cartesian_y_size(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/cartesian/y_size".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
fn control_render_evaluation_cartesian_z_size(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/cartesian/z_size".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
fn control_render_evaluation_cartesian_z_neg_size(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/cartesian/z_neg_size".to_string(),
            value: value.max(0),
        },
    );
}

#[tauri::command]
fn control_render_backend(state: State<SharedState>, value: String) {
    let normalized = value.trim().to_ascii_lowercase();
    if !matches!(
        normalized.as_str(),
        "vbap" | "barycenter" | "experimental_distance"
    ) {
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

#[tauri::command]
fn control_barycenter_localize(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/barycenter/localize".to_string(),
            value: value.max(0.0),
        },
    );
}

#[tauri::command]
fn control_restore_render_backend(state: State<SharedState>) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_backend/restore".to_string(),
            value: 1,
        },
    );
}

#[tauri::command]
fn control_render_evaluation_mode(state: State<SharedState>, value: String) {
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
fn control_render_evaluation_polar_azimuth_resolution(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/polar/azimuth_resolution".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
fn control_render_evaluation_polar_elevation_resolution(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/polar/elevation_resolution".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
fn control_render_evaluation_polar_distance_res(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/polar/distance_res".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
fn control_render_evaluation_polar_distance_max(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/render_evaluation/polar/distance_max".to_string(),
            value: value.max(0.01),
        },
    );
}

#[tauri::command]
fn control_render_evaluation_position_interpolation(state: State<SharedState>, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/render_evaluation/position_interpolation".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
fn request_speaker_heatmap(
    state: State<SharedState>,
    speaker_index: i32,
    request_id: i32,
    mode: String,
    max_samples: Option<i32>,
) {
    if speaker_index < 0 || request_id < 0 {
        return;
    }
    let value = serde_json::json!({
        "speaker_index": speaker_index,
        "request_id": request_id,
        "mode": mode,
        "max_samples": max_samples,
    })
    .to_string();
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/debug/speaker_heatmap/request".to_string(),
            value,
        },
    );
}

#[tauri::command]
fn control_distance_diffuse_enabled(state: State<SharedState>, enable: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/distance_diffuse/enabled".to_string(),
            value: if enable != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
fn control_distance_diffuse_threshold(state: State<SharedState>, value: f32) {
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
fn control_distance_diffuse_curve(state: State<SharedState>, value: f32) {
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
fn control_room_ratio(state: State<SharedState>, width: f32, length: f32, height: f32) {
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

#[tauri::command]
fn control_room_ratio_rear(state: State<SharedState>, value: f32) {
    let v = value.max(0.01);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/room_ratio_rear".to_string(),
            value: v,
        },
    );
}

#[tauri::command]
fn control_room_ratio_lower(state: State<SharedState>, value: f32) {
    let v = value.max(0.01);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/room_ratio_lower".to_string(),
            value: v,
        },
    );
}

#[tauri::command]
fn control_room_ratio_center_blend(state: State<SharedState>, value: f32) {
    let v = value.clamp(0.0, 1.0);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/room_ratio_center_blend".to_string(),
            value: v,
        },
    );
}

#[tauri::command]
fn control_layout_radius_m(state: State<SharedState>, value: f32) {
    let v = value.max(0.01);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/layout/radius_m".to_string(),
            value: v,
        },
    );
}

#[tauri::command]
fn control_speaker_az(state: State<SharedState>, id: i32, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: format!("/omniphony/control/speaker/{id}/az"),
            value,
        },
    );
}

#[tauri::command]
fn control_speaker_el(state: State<SharedState>, id: i32, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: format!("/omniphony/control/speaker/{id}/el"),
            value,
        },
    );
}

#[tauri::command]
fn control_speaker_distance(state: State<SharedState>, id: i32, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: format!("/omniphony/control/speaker/{id}/distance"),
            value,
        },
    );
}

#[tauri::command]
fn control_speaker_x(state: State<SharedState>, id: i32, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: format!("/omniphony/control/speaker/{id}/x"),
            value: value.clamp(-1.0, 1.0),
        },
    );
}

#[tauri::command]
fn control_speaker_y(state: State<SharedState>, id: i32, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: format!("/omniphony/control/speaker/{id}/y"),
            value: value.clamp(-1.0, 1.0),
        },
    );
}

#[tauri::command]
fn control_speaker_z(state: State<SharedState>, id: i32, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: format!("/omniphony/control/speaker/{id}/z"),
            value: value.clamp(-1.0, 1.0),
        },
    );
}

#[tauri::command]
fn control_speaker_coord_mode(state: State<SharedState>, id: i32, value: String) {
    let normalized = if value.trim().eq_ignore_ascii_case("cartesian") {
        "cartesian"
    } else {
        "polar"
    };
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: format!("/omniphony/control/speaker/{id}/coord_mode"),
            value: normalized.to_string(),
        },
    );
}

#[tauri::command]
fn control_speaker_delay(state: State<SharedState>, id: i32, delay_ms: f32) {
    let v = delay_ms.max(0.0);
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: format!("/omniphony/control/speaker/{id}/delay"),
            value: v,
        },
    );
}

#[tauri::command]
fn control_speaker_spatialize(state: State<SharedState>, id: i32, spatialize: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: format!("/omniphony/control/speaker/{id}/spatialize"),
            value: if spatialize != 0 { 1 } else { 0 },
        },
    );
}

#[tauri::command]
fn control_speaker_name(state: State<SharedState>, id: i32, name: String) {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: format!("/omniphony/control/speaker/{id}/name"),
            value: trimmed.to_string(),
        },
    );
}

#[tauri::command]
fn control_speakers_apply(state: State<SharedState>) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/speakers/apply".to_string(),
        },
    );
}

#[tauri::command]
fn control_speakers_add(
    state: State<SharedState>,
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
    send_control(
        &state.osc_tx,
        OscControlMsg::SendSpeakerAdd {
            name: n.to_string(),
            azimuth,
            elevation,
            distance: distance.max(0.01),
            spatialize: if spatialize != 0 { 1 } else { 0 },
            delay_ms: delay_ms.max(0.0),
        },
    );
}

#[tauri::command]
fn control_speakers_remove(state: State<SharedState>, index: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/speakers/remove".to_string(),
            value: index.max(0),
        },
    );
}

#[tauri::command]
fn control_speakers_move(state: State<SharedState>, from: i32, to: i32) {
    send_control(&state.osc_tx, OscControlMsg::SendSpeakersMove { from, to });
}

#[tauri::command]
fn control_save_config(state: State<SharedState>) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/save_config".to_string(),
        },
    );
}

#[tauri::command]
fn control_reload_config(state: State<SharedState>) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/reload_config".to_string(),
        },
    );
}

#[tauri::command]
fn control_log_level(state: State<SharedState>, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(
        trimmed.as_str(),
        "off" | "error" | "warn" | "info" | "debug" | "trace"
    ) {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/log_level".to_string(),
            value: trimmed,
        },
    );
}

#[tauri::command]
fn control_ramp_mode(state: State<SharedState>, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(trimmed.as_str(), "off" | "frame" | "sample") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/ramp_mode".to_string(),
            value: trimmed,
        },
    );
}

#[tauri::command]
fn control_export_layout(state: State<SharedState>, name: Option<String>) {
    if let Some(raw) = name {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            send_control(
                &state.osc_tx,
                OscControlMsg::SendString {
                    address: "/omniphony/control/layout/export".to_string(),
                    value: trimmed.to_string(),
                },
            );
            return;
        }
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/layout/export".to_string(),
        },
    );
}

#[tauri::command]
fn control_import_evaluation_artifact(state: State<SharedState>, path: String) {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/render_evaluation_mode/from_file".to_string(),
            value: trimmed.to_string(),
        },
    );
}

#[tauri::command]
fn control_export_evaluation_artifact(state: State<SharedState>, path: String) {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/render_evaluation/export".to_string(),
            value: trimmed.to_string(),
        },
    );
}

#[tauri::command]
fn control_audio_sample_rate(state: State<SharedState>, sample_rate: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/audio/sample_rate".to_string(),
            value: sample_rate.max(0),
        },
    );
}

#[tauri::command]
fn control_audio_output_device(state: State<SharedState>, output_device: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/audio/output_device".to_string(),
            value: output_device.trim().to_string(),
        },
    );
}

#[tauri::command]
fn refresh_output_devices(state: State<SharedState>) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/audio/output_devices/refresh".to_string(),
        },
    );
}

#[tauri::command]
fn control_input_mode(state: State<SharedState>, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(
        trimmed.as_str(),
        "bridge" | "pipe_bridge" | "live" | "pipewire" | "pipewire_bridge"
    ) {
        return;
    }
    let normalized = match trimmed.as_str() {
        "bridge" => "pipe_bridge",
        "live" => "pipewire",
        other => other,
    };
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/mode".to_string(),
            value: normalized.to_string(),
        },
    );
}

#[tauri::command]
fn control_input_live_backend(state: State<SharedState>, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(trimmed.as_str(), "pipewire" | "asio") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/live/backend".to_string(),
            value: trimmed,
        },
    );
}

#[tauri::command]
fn control_input_live_node(state: State<SharedState>, value: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/live/node".to_string(),
            value: value.trim().to_string(),
        },
    );
}

#[tauri::command]
fn control_input_live_description(state: State<SharedState>, value: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/live/description".to_string(),
            value: value.trim().to_string(),
        },
    );
}

#[tauri::command]
fn control_input_live_layout(state: State<SharedState>, value: String) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/live/layout".to_string(),
            value: value.trim().to_string(),
        },
    );
}

#[tauri::command]
fn import_input_layout_from_path(
    state: State<SharedState>,
    path: String,
) -> Result<serde_json::Value, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("empty layout path".to_string());
    }
    layouts::load_layout_file(std::path::Path::new(trimmed))
        .ok_or_else(|| "failed to parse layout file".to_string())?;
    let contents =
        fs::read_to_string(trimmed).map_err(|e| format!("failed to read layout file: {e}"))?;
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/live/layout".to_string(),
            value: trimmed.to_string(),
        },
    );
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/live/layout_import".to_string(),
            value: contents,
        },
    );
    Ok(serde_json::json!({ "path": trimmed }))
}

#[tauri::command]
fn control_input_live_channels(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/input/live/channels".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
fn control_input_live_sample_rate(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/input/live/sample_rate".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
fn control_input_live_format(state: State<SharedState>, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(trimmed.as_str(), "f32" | "s16") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/live/format".to_string(),
            value: trimmed,
        },
    );
}

#[tauri::command]
fn control_input_live_clock_mode(state: State<SharedState>, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(trimmed.as_str(), "dac" | "pipewire" | "upstream") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/live/clock_mode".to_string(),
            value: trimmed,
        },
    );
}

#[tauri::command]
fn control_input_live_map(state: State<SharedState>, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(trimmed.as_str(), "7.1-fixed") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/live/map".to_string(),
            value: trimmed,
        },
    );
}

#[tauri::command]
fn control_input_live_lfe_mode(state: State<SharedState>, value: String) {
    let trimmed = value.trim().to_ascii_lowercase();
    if !matches!(trimmed.as_str(), "object" | "direct" | "drop") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/input/live/lfe_mode".to_string(),
            value: trimmed,
        },
    );
}

#[tauri::command]
fn control_input_apply(state: State<SharedState>) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/input/apply".to_string(),
        },
    );
}

#[tauri::command]
fn control_input_refresh(state: State<SharedState>) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/input/refresh".to_string(),
        },
    );
}

fn first_existing_path(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates.iter().find(|path| path.exists()).cloned()
}

fn bundled_orender_candidates(app: &tauri::AppHandle) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(resource_dir) = app.path().resource_dir() {
        candidates.push(resource_dir.join("orender"));
        candidates.push(resource_dir.join("orender.exe"));
    }
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(exe_dir) = current_exe.parent() {
            candidates.push(exe_dir.join("orender"));
            candidates.push(exe_dir.join("orender.exe"));
        }
    }
    candidates
}

fn bundled_layouts_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    app.path()
        .resource_dir()
        .ok()
        .map(|dir| dir.join("layouts"))
}

fn default_orender_input_path() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        PathBuf::from(r"\\.\pipe\orender.input")
    }

    #[cfg(not(target_os = "windows"))]
    {
        std::env::temp_dir().join("orender.pipe")
    }
}

fn default_orender_log_path() -> PathBuf {
    std::env::temp_dir().join("omniphony-orender.log")
}

fn resolve_orender_launch_spec(
    app: &tauri::AppHandle,
    state: &SharedState,
    host: String,
    osc_rx_port: u16,
    osc_port: u16,
    osc_metering_enabled: bool,
    bridge_path: Option<String>,
    orender_path: Option<String>,
    log_level: Option<String>,
) -> Result<OrenderLaunchSpec, String> {
    let studio_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or_else(|| "failed to resolve studio directory".to_string())?
        .to_path_buf();
    let repo_root = studio_dir
        .parent()
        .ok_or_else(|| "failed to resolve Omniphony repository root".to_string())?
        .to_path_buf();
    let workspace_root = repo_root
        .parent()
        .ok_or_else(|| "failed to resolve workspace root".to_string())?
        .to_path_buf();

    let repo_orender_candidates = [
        repo_root.join("omniphony-renderer/target/release/orender"),
        repo_root.join("omniphony-renderer/target/debug/orender"),
    ];

    let orender_path = if cfg!(debug_assertions) {
        first_existing_path(&repo_orender_candidates)
            .or_else(|| {
                orender_path
                    .as_deref()
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                    .map(PathBuf::from)
                    .filter(|path| path.exists())
            })
            .or_else(|| first_existing_path(&bundled_orender_candidates(app)))
    } else {
        orender_path
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.exists())
            .or_else(|| first_existing_path(&bundled_orender_candidates(app)))
            .or_else(|| first_existing_path(&repo_orender_candidates))
    }
    .or_else(|| {
        let lookup_cmd = if cfg!(target_os = "windows") {
            "where"
        } else {
            "which"
        };
        ProcessCommand::new(lookup_cmd)
            .arg("orender")
            .output()
            .ok()
            .filter(|out| out.status.success())
            .and_then(|out| {
                let resolved = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if resolved.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(resolved))
                }
            })
    })
    .ok_or_else(|| "orender binary not found".to_string())?;

    let bridge_path = bridge_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.exists())
        .or_else(|| {
            first_existing_path(&[
                workspace_root.join("truehd-bridge/target/release/libtruehd_bridge.so"),
                repo_root.join("omniphony-renderer/target/release/libtruehd_bridge.so"),
            ])
        })
        .ok_or_else(|| "a bridge plugin is required to launch orender".to_string())?;

    let input_path = default_orender_input_path();

    let mut args = vec![
        "render".to_string(),
        input_path.display().to_string(),
        "--continuous".to_string(),
        "--bridge-path".to_string(),
        bridge_path.display().to_string(),
        "--enable-vbap".to_string(),
        "--osc".to_string(),
        "--osc-host".to_string(),
        host.trim().to_string(),
        "--osc-port".to_string(),
        osc_rx_port.to_string(),
        "--osc-rx-port".to_string(),
        osc_rx_port.to_string(),
    ];

    if osc_metering_enabled {
        args.push("--osc-metering".to_string());
    }

    let level = log_level
        .as_deref()
        .map(str::trim)
        .filter(|s| matches!(*s, "off" | "error" | "warn" | "info" | "debug" | "trace"))
        .unwrap_or("info");
    if level != "info" {
        args.push("--loglevel".to_string());
        args.push(level.to_string());
    }

    if let Some(selected_layout) = state.inner.lock().unwrap().selected_layout_key.clone() {
        let layout_file = format!("{selected_layout}.yaml");
        let layout_path = bundled_layouts_dir(app)
            .map(|dir| dir.join(&layout_file))
            .filter(|path| path.exists())
            .or_else(|| {
                let path = repo_root.join("layouts").join(&layout_file);
                path.exists().then_some(path)
            });
        if let Some(layout_path) = layout_path {
            args.push("--speaker-layout".to_string());
            args.push(layout_path.display().to_string());
        }
    }

    let _ = save_config(
        &state.config_dir,
        &OscConfig {
            host: host.trim().to_string(),
            osc_rx_port,
            osc_port,
            osc_metering_enabled,
        },
    );

    Ok(OrenderLaunchSpec { orender_path, args })
}

fn run_command(mut cmd: ProcessCommand, action: &str) -> Result<String, String> {
    let output = cmd.output().map_err(|e| format!("{action}: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        Err(if detail.is_empty() {
            format!("{action}: command failed")
        } else {
            format!("{action}: {detail}")
        })
    }
}

fn wait_for_orender_disconnect(state: &SharedState, timeout_ms: u64) -> Result<(), String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    loop {
        let status = state.inner.lock().unwrap().osc_status.clone();
        if status.as_deref() != Some("connected") {
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            return Err("timed out while waiting for orender to stop".to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

fn stop_non_service_orender_if_running(state: &SharedState) -> Result<(), String> {
    let is_connected = state.inner.lock().unwrap().osc_status.as_deref() == Some("connected");
    if !is_connected {
        return Ok(());
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/quit".to_string(),
        },
    );
    wait_for_orender_disconnect(state, 10_000)
}

#[cfg(target_os = "windows")]
fn powershell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

#[cfg(target_os = "windows")]
fn run_elevated_windows(program: &str, args: &[String], action: &str) -> Result<String, String> {
    let arg_list = if args.is_empty() {
        "@()".to_string()
    } else {
        format!(
            "@({})",
            args.iter()
                .map(|arg| powershell_single_quote(arg))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    let command = format!(
        "$p = Start-Process -FilePath {} -ArgumentList {} -Verb RunAs -Wait -PassThru; exit $p.ExitCode",
        powershell_single_quote(program),
        arg_list
    );
    let mut cmd = ProcessCommand::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &command]);
    run_command(cmd, action)
}

#[cfg(target_os = "linux")]
fn systemd_escape_arg(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            ' ' | '\t' | '\n' | '\\' | '"' | '\'' => {
                out.push('\\');
                out.push(ch);
            }
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(target_os = "linux")]
fn linux_service_unit(exec_path: &PathBuf, args: &[String]) -> String {
    let mut exec = Vec::with_capacity(args.len() + 1);
    exec.push(systemd_escape_arg(&exec_path.display().to_string()));
    exec.extend(args.iter().map(|arg| systemd_escape_arg(arg)));
    format!(
        "[Unit]\nDescription=Omniphony Renderer\nAfter=graphical-session.target pipewire.service wireplumber.service\nWants=graphical-session.target\n\n[Service]\nType=notify\nExecStart={}\nRestart=on-failure\nRestartSec=2\nKillSignal=SIGINT\nTimeoutStopSec=30\n\n[Install]\nWantedBy=default.target\n",
        exec.join(" ")
    )
}

#[cfg(target_os = "linux")]
fn linux_user_service_name() -> String {
    format!("{ORENDER_SERVICE_NAME}.service")
}

#[cfg(target_os = "linux")]
fn linux_user_service_dir() -> Result<PathBuf, String> {
    let home = std::env::var_os("HOME").ok_or_else(|| "HOME is not set".to_string())?;
    Ok(PathBuf::from(home).join(".config/systemd/user"))
}

#[cfg(target_os = "linux")]
fn run_user_systemctl(args: &[&str], action: &str) -> Result<String, String> {
    let mut cmd = ProcessCommand::new("systemctl");
    cmd.arg("--user").args(args);
    run_command(cmd, action)
}

#[cfg(target_os = "windows")]
fn windows_service_bin_path(exec_path: &PathBuf, args: &[String]) -> String {
    let mut parts = Vec::with_capacity(args.len() + 1);
    parts.push(format!("\"{}\"", exec_path.display()));
    for arg in args {
        let escaped = arg.replace('"', "\\\"");
        if escaped.contains(' ') || escaped.contains('\t') {
            parts.push(format!("\"{}\"", escaped));
        } else {
            parts.push(escaped);
        }
    }
    parts.join(" ")
}

#[tauri::command]
fn get_orender_service_status() -> Result<OrenderServiceStatus, String> {
    #[cfg(target_os = "linux")]
    {
        let service_name = linux_user_service_name();
        let output = ProcessCommand::new("systemctl")
            .args([
                "--user",
                "show",
                "-p",
                "LoadState",
                "--value",
                &service_name,
            ])
            .output()
            .map_err(|e| format!("query service status: {e}"))?;
        let load_state = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let installed =
            output.status.success() && load_state != "not-found" && !load_state.is_empty();
        let running = if installed {
            ProcessCommand::new("systemctl")
                .args(["--user", "is-active", "--quiet", &service_name])
                .status()
                .map(|status| status.success())
                .unwrap_or(false)
        } else {
            false
        };
        return Ok(OrenderServiceStatus {
            installed,
            running,
            manager: "systemd-user",
        });
    }

    #[cfg(target_os = "windows")]
    {
        let output = ProcessCommand::new("sc")
            .args(["query", ORENDER_SERVICE_NAME])
            .output()
            .map_err(|e| format!("query service status: {e}"))?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let missing =
            stdout.contains("1060") || stderr.contains("1060") || stdout.contains("does not exist");
        let installed = output.status.success() && !missing;
        let running = installed && stdout.contains("RUNNING");
        return Ok(OrenderServiceStatus {
            installed,
            running,
            manager: "scm",
        });
    }

    #[allow(unreachable_code)]
    Err("service management is not supported on this platform".to_string())
}

#[tauri::command]
fn install_orender_service(
    app: tauri::AppHandle,
    state: State<SharedState>,
    host: String,
    osc_rx_port: u16,
    osc_port: u16,
    osc_metering_enabled: bool,
    bridge_path: Option<String>,
    orender_path: Option<String>,
    log_level: Option<String>,
) -> Result<serde_json::Value, String> {
    stop_non_service_orender_if_running(&state)?;

    let spec = resolve_orender_launch_spec(
        &app,
        &state,
        host,
        osc_rx_port,
        osc_port,
        osc_metering_enabled,
        bridge_path,
        orender_path,
        log_level,
    )?;

    #[cfg(target_os = "linux")]
    {
        let service_name = linux_user_service_name();
        let unit_dir = linux_user_service_dir()?;
        let unit_path = unit_dir.join(&service_name);
        std::fs::create_dir_all(&unit_dir).map_err(|e| {
            format!(
                "install service: failed to create {}: {e}",
                unit_dir.display()
            )
        })?;
        std::fs::write(
            &unit_path,
            linux_service_unit(&spec.orender_path, &spec.args),
        )
        .map_err(|e| {
            format!(
                "install service: failed to write {}: {e}",
                unit_path.display()
            )
        })?;
        run_user_systemctl(&["daemon-reload"], "install service")?;
        run_user_systemctl(&["enable", &service_name], "install service")?;
        return Ok(serde_json::json!({
            "command": format!("systemctl --user enable {service_name}")
        }));
    }

    #[cfg(target_os = "windows")]
    {
        let bin_path = windows_service_bin_path(&spec.orender_path, &spec.args);

        // Use a temp .ps1 script with New-Service so that the BinaryPathName
        // (which contains embedded double-quotes) is passed directly to the
        // CreateService Win32 API as a PS parameter, bypassing Win32
        // command-line parsing entirely.  Start-Process -ArgumentList mangles
        // embedded double-quotes when building sc.exe's command line, which is
        // why the sc.exe create approach silently fails.
        let script_path = {
            let mut p = std::env::temp_dir();
            p.push(format!("omniphony-install-{}.ps1", std::process::id()));
            p
        };
        let script = format!(
            "try {{\r\n\
             New-Service -Name {name} -BinaryPathName {bin} \
             -DisplayName {display} -StartupType Manual -ErrorAction Stop\r\n\
             & sc.exe description {name} {desc}\r\n\
             exit 0\r\n\
             }} catch {{\r\n\
             Write-Error $_\r\n\
             exit 1\r\n\
             }}\r\n",
            name = powershell_single_quote(ORENDER_SERVICE_NAME),
            bin = powershell_single_quote(&bin_path),
            display = powershell_single_quote("Omniphony Renderer"),
            desc = powershell_single_quote("Omniphony spatial audio renderer"),
        );
        std::fs::write(&script_path, script)
            .map_err(|e| format!("install service: failed to write temp script: {e}"))?;

        let ps_path = script_path.display().to_string();
        let command = format!(
            "$p = Start-Process powershell \
             -ArgumentList @('-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',{}) \
             -Verb RunAs -Wait -PassThru; \
             if ($p) {{ exit $p.ExitCode }} else {{ exit 1 }}",
            powershell_single_quote(&ps_path),
        );
        let mut ps_cmd = ProcessCommand::new("powershell");
        ps_cmd.args(["-NoProfile", "-NonInteractive", "-Command", &command]);
        let result = run_command(ps_cmd, "install service");
        let _ = std::fs::remove_file(&script_path);
        result?;

        return Ok(serde_json::json!({
            "command": format!("sc create {} binPath= {}", ORENDER_SERVICE_NAME, bin_path)
        }));
    }

    #[allow(unreachable_code)]
    Err("service management is not supported on this platform".to_string())
}

#[tauri::command]
fn uninstall_orender_service() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let service_name = linux_user_service_name();
        let unit_path = linux_user_service_dir()?.join(&service_name);
        let _ = run_user_systemctl(&["stop", &service_name], "uninstall service");
        let _ = run_user_systemctl(&["disable", &service_name], "uninstall service");
        if unit_path.exists() {
            std::fs::remove_file(&unit_path).map_err(|e| {
                format!(
                    "uninstall service: failed to remove {}: {e}",
                    unit_path.display()
                )
            })?;
        }
        run_user_systemctl(&["daemon-reload"], "uninstall service")?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let _ = run_elevated_windows(
            "sc.exe",
            &["stop".to_string(), ORENDER_SERVICE_NAME.to_string()],
            "uninstall service",
        );
        run_elevated_windows(
            "sc.exe",
            &["delete".to_string(), ORENDER_SERVICE_NAME.to_string()],
            "uninstall service",
        )?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err("service management is not supported on this platform".to_string())
}

#[tauri::command]
fn start_orender_service() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let service_name = linux_user_service_name();
        run_user_systemctl(&["start", &service_name], "start service")?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        run_elevated_windows(
            "sc.exe",
            &["start".to_string(), ORENDER_SERVICE_NAME.to_string()],
            "start service",
        )?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err("service management is not supported on this platform".to_string())
}

#[tauri::command]
fn stop_orender_service() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let service_name = linux_user_service_name();
        run_user_systemctl(&["stop", &service_name], "stop service")?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        run_elevated_windows(
            "sc.exe",
            &["stop".to_string(), ORENDER_SERVICE_NAME.to_string()],
            "stop service",
        )?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err("service management is not supported on this platform".to_string())
}

#[tauri::command]
fn restart_orender_service() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let service_name = linux_user_service_name();
        run_user_systemctl(&["restart", &service_name], "restart service")?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        let command = format!(
            "$p = Start-Process -FilePath powershell -ArgumentList @('-NoProfile','-NonInteractive','-Command',{}) -Verb RunAs -Wait -PassThru; exit $p.ExitCode",
            powershell_single_quote(
                &format!(
                    "Restart-Service -Name '{}' -Force -ErrorAction Stop",
                    ORENDER_SERVICE_NAME
                )
            )
        );
        let mut cmd = ProcessCommand::new("powershell");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", &command]);
        run_command(cmd, "restart service")?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err("service management is not supported on this platform".to_string())
}

#[tauri::command]
fn restart_pipewire_services() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        run_user_systemctl(&["restart", "pipewire", "wireplumber"], "restart pipewire")?;
        return Ok(());
    }

    #[allow(unreachable_code)]
    Err("PipeWire restart is only supported on Linux".to_string())
}

#[tauri::command]
fn launch_orender(
    app: tauri::AppHandle,
    state: State<SharedState>,
    host: String,
    osc_rx_port: u16,
    osc_port: u16,
    osc_metering_enabled: bool,
    bridge_path: Option<String>,
    orender_path: Option<String>,
    log_level: Option<String>,
) -> Result<serde_json::Value, String> {
    let spec = resolve_orender_launch_spec(
        &app,
        &state,
        host,
        osc_rx_port,
        osc_port,
        osc_metering_enabled,
        bridge_path,
        orender_path,
        log_level,
    )?;

    let log_path = default_orender_log_path();
    let stdout = File::create(&log_path).map_err(|e| format!("failed to create log file: {e}"))?;
    let stderr = stdout
        .try_clone()
        .map_err(|e| format!("failed to clone log file handle: {e}"))?;

    #[allow(unused_mut)]
    let mut cmd = ProcessCommand::new(&spec.orender_path);
    cmd.args(&spec.args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const NORMAL_PRIORITY_CLASS: u32 = 0x0000_0020;
        cmd.creation_flags(CREATE_NO_WINDOW | NORMAL_PRIORITY_CLASS);
    }

    cmd.spawn()
        .map_err(|e| format!("failed to launch orender: {e}"))?;

    Ok(serde_json::json!({
        "command": format!("{} {}", spec.orender_path.display(), spec.args.join(" ")),
        "logPath": log_path.display().to_string()
    }))
}

#[tauri::command]
fn stop_orender(state: State<SharedState>) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendNoArgs {
            address: "/omniphony/control/quit".to_string(),
        },
    );
}

// ── main ─────────────────────────────────────────────────────────────────

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                let decoded = image::load_from_memory(include_bytes!("../icons/icon.png"))
                    .expect("failed to decode window icon")
                    .into_rgba8();
                let (width, height) = decoded.dimensions();
                let window_icon = tauri::image::Image::new_owned(decoded.into_raw(), width, height);
                let _ = window.set_icon(window_icon);
            }

            let config_dir = app
                .path()
                .app_config_dir()
                .expect("could not resolve app config dir");

            let osc_cfg = load_config(&config_dir);

            // layouts dir: bundled as a resource
            let layouts_dir = app
                .path()
                .resource_dir()
                .map(|d| d.join("layouts"))
                .unwrap_or_else(|_| PathBuf::from("layouts"));

            let loaded_layouts = layouts::load_layouts(&layouts_dir);

            let mut initial_state = AppState::new(loaded_layouts);
            initial_state.osc_metering_enabled =
                Some(if osc_cfg.osc_metering_enabled { 1 } else { 0 });
            let app_state = Arc::new(Mutex::new(initial_state));
            let osc_tx: Arc<Mutex<Option<UnboundedSender<OscControlMsg>>>> =
                Arc::new(Mutex::new(None));
            let listen_port = Arc::new(Mutex::new(0u16));

            let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<OscControlMsg>();
            *osc_tx.lock().unwrap() = Some(tx);

            let shared = SharedState {
                inner: app_state.clone(),
                osc_tx: osc_tx.clone(),
                config_dir,
                listen_port: listen_port.clone(),
            };
            app.manage(shared);

            spawn_osc_task(
                app.handle().clone(),
                app_state,
                osc_cfg.host,
                osc_cfg.osc_port,
                osc_cfg.osc_rx_port,
                rx,
                listen_port.clone(),
            );

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_state,
            get_osc_config,
            get_about_info,
            save_osc_config,
            launch_orender,
            stop_orender,
            get_orender_service_status,
            install_orender_service,
            uninstall_orender_service,
            start_orender_service,
            stop_orender_service,
            restart_orender_service,
            restart_pipewire_services,
            control_osc_metering,
            select_layout,
            import_layout_from_path,
            pick_import_layout_path,
            pick_export_layout_path,
            pick_import_evaluation_artifact_path,
            pick_export_evaluation_artifact_path,
            pick_bridge_path,
            pick_orender_path,
            export_layout_to_path,
            control_object_gain,
            control_speaker_gain,
            control_object_mute,
            control_speaker_mute,
            control_master_gain,
            control_loudness,
            control_adaptive_resampling,
            control_adaptive_resampling_enable_far_mode,
            control_adaptive_resampling_force_silence_in_far_mode,
            control_adaptive_resampling_hard_recover_high_in_far_mode,
            control_adaptive_resampling_hard_recover_low_in_far_mode,
            control_adaptive_resampling_far_mode_return_fade_in_ms,
            control_latency_target,
            control_adaptive_resampling_kp_near,
            control_adaptive_resampling_ki,
            control_adaptive_resampling_integral_discharge_ratio,
            control_adaptive_resampling_max_adjust,
            control_adaptive_resampling_update_interval_callbacks,
            control_adaptive_resampling_near_far_threshold_ms,
            control_adaptive_resampling_pause,
            control_adaptive_resampling_reset_ratio,
            control_spread_min,
            control_spread_max,
            control_spread_from_distance,
            control_spread_distance_range,
            control_spread_distance_curve,
            control_distance_model,
            control_experimental_distance_distance_floor,
            control_experimental_distance_min_active_speakers,
            control_experimental_distance_max_active_speakers,
            control_experimental_distance_position_error_floor,
            control_experimental_distance_position_error_nearest_scale,
            control_experimental_distance_position_error_span_scale,
            control_render_evaluation_cartesian_x_size,
            control_render_evaluation_cartesian_y_size,
            control_render_evaluation_cartesian_z_size,
            control_render_evaluation_cartesian_z_neg_size,
            control_render_backend,
            control_barycenter_localize,
            control_restore_render_backend,
            control_render_evaluation_mode,
            control_render_evaluation_polar_azimuth_resolution,
            control_render_evaluation_polar_elevation_resolution,
            control_render_evaluation_polar_distance_res,
            control_render_evaluation_polar_distance_max,
            control_render_evaluation_position_interpolation,
            request_speaker_heatmap,
            control_distance_diffuse_enabled,
            control_distance_diffuse_threshold,
            control_distance_diffuse_curve,
            control_room_ratio,
            control_room_ratio_rear,
            control_room_ratio_lower,
            control_room_ratio_center_blend,
            control_layout_radius_m,
            control_speaker_az,
            control_speaker_el,
            control_speaker_distance,
            control_speaker_x,
            control_speaker_y,
            control_speaker_z,
            control_speaker_coord_mode,
            control_speaker_delay,
            control_speaker_spatialize,
            control_speaker_name,
            control_speakers_apply,
            control_speakers_add,
            control_speakers_remove,
            control_speakers_move,
            control_save_config,
            control_reload_config,
            control_log_level,
            control_ramp_mode,
            control_audio_output_device,
            refresh_output_devices,
            control_input_mode,
            control_input_live_backend,
            control_input_live_node,
            control_input_live_description,
            control_input_live_layout,
            import_input_layout_from_path,
            control_input_live_channels,
            control_input_live_sample_rate,
            control_input_live_format,
            control_input_live_clock_mode,
            control_input_live_map,
            control_input_live_lfe_mode,
            control_input_apply,
            control_input_refresh,
            control_export_layout,
            control_import_evaluation_artifact,
            control_export_evaluation_artifact,
            control_audio_sample_rate,
        ])
        .run(tauri::generate_context!())
        .expect("error running Tauri application");
}
