// Prevents an additional console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app_state;
mod config;
mod layouts;
mod osc_listener;
mod osc_parser;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use app_state::AppState;
use config::{load_config, save_config, OscConfig};
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
fn save_osc_config(state: State<SharedState>, config: OscConfig) -> Result<(), String> {
    save_config(&state.config_dir, &config)?;
    state.inner.lock().unwrap().osc_metering_enabled = Some(if config.osc_metering_enabled { 1 } else { 0 });
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
    send_control(
        &state.osc_tx,
        OscControlMsg::SetMeteringEnabled { enabled },
    );
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
            if s.to_ascii_lowercase().ends_with(".json") {
                s.to_string()
            } else {
                format!("{s}.json")
            }
        })
        .unwrap_or_else(|| "layout.json".to_string());

    FileDialog::new()
        .add_filter("Layout JSON", &["json"])
        .set_file_name(&file_name)
        .save_file()
        .map(|path| path.to_string_lossy().to_string())
}

#[tauri::command]
fn export_layout_to_path(
    state: State<SharedState>,
    path: String,
    key: Option<String>,
) -> Result<(), String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("empty export path".to_string());
    }

    let target_key = key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);

    let layout = {
        let s = state.inner.lock().unwrap();
        let selected = target_key.or_else(|| s.selected_layout_key.clone());
        let key = selected.ok_or_else(|| "no layout selected".to_string())?;
        s.layouts
            .iter()
            .find(|layout| layout.key == key)
            .cloned()
            .ok_or_else(|| "layout not found".to_string())?
    };

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
fn control_adaptive_resampling_kp_far(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/adaptive_resampling/kp_far".to_string(),
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
fn control_adaptive_resampling_max_adjust_far(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/adaptive_resampling/max_adjust_far".to_string(),
            value: value.max(0.000001),
        },
    );
}

#[tauri::command]
fn control_adaptive_resampling_near_far_threshold_ms(
    state: State<SharedState>,
    value: i32,
) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling/near_far_threshold_ms".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
fn control_adaptive_resampling_hard_correction_threshold_ms(
    state: State<SharedState>,
    value: i32,
) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/adaptive_resampling/hard_correction_threshold_ms".to_string(),
            value: value.max(0),
        },
    );
}

#[tauri::command]
fn control_adaptive_resampling_measurement_smoothing_alpha(
    state: State<SharedState>,
    value: f32,
) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/adaptive_resampling/measurement_smoothing_alpha".to_string(),
            value: value.clamp(0.0, 1.0),
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
fn control_vbap_cart_x_size(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/vbap/cart/x_size".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
fn control_vbap_cart_y_size(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/vbap/cart/y_size".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
fn control_vbap_cart_z_size(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/vbap/cart/z_size".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
fn control_vbap_cart_z_neg_size(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/vbap/cart/z_neg_size".to_string(),
            value: value.max(0),
        },
    );
}

#[tauri::command]
fn control_vbap_table_mode(state: State<SharedState>, mode: String) {
    let normalized = mode.trim().to_ascii_lowercase();
    if !matches!(normalized.as_str(), "auto" | "polar" | "cartesian") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: "/omniphony/control/vbap/table_mode".to_string(),
            value: normalized,
        },
    );
}

#[tauri::command]
fn control_vbap_polar_azimuth_resolution(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/vbap/polar/azimuth_resolution".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
fn control_vbap_polar_elevation_resolution(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/vbap/polar/elevation_resolution".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
fn control_vbap_polar_distance_res(state: State<SharedState>, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: "/omniphony/control/vbap/polar/distance_res".to_string(),
            value: value.max(1),
        },
    );
}

#[tauri::command]
fn control_vbap_polar_distance_max(state: State<SharedState>, value: f32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendFloat {
            address: "/omniphony/control/vbap/polar/distance_max".to_string(),
            value: value.max(0.01),
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

// ── main ─────────────────────────────────────────────────────────────────

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                let decoded = image::load_from_memory(include_bytes!("../icons/icon.png"))
                    .expect("failed to decode window icon")
                    .into_rgba8();
                let (width, height) = decoded.dimensions();
                let window_icon =
                    tauri::image::Image::new_owned(decoded.into_raw(), width, height);
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
            initial_state.osc_metering_enabled = Some(if osc_cfg.osc_metering_enabled { 1 } else { 0 });
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
            save_osc_config,
            control_osc_metering,
            select_layout,
            import_layout_from_path,
            pick_import_layout_path,
            pick_export_layout_path,
            export_layout_to_path,
            control_object_gain,
            control_speaker_gain,
            control_object_mute,
            control_speaker_mute,
            control_master_gain,
            control_loudness,
            control_adaptive_resampling,
            control_latency_target,
            control_adaptive_resampling_kp_near,
            control_adaptive_resampling_kp_far,
            control_adaptive_resampling_ki,
            control_adaptive_resampling_max_adjust,
            control_adaptive_resampling_max_adjust_far,
            control_adaptive_resampling_near_far_threshold_ms,
            control_adaptive_resampling_hard_correction_threshold_ms,
            control_adaptive_resampling_measurement_smoothing_alpha,
            control_spread_min,
            control_spread_max,
            control_spread_from_distance,
            control_spread_distance_range,
            control_spread_distance_curve,
            control_vbap_cart_x_size,
            control_vbap_cart_y_size,
            control_vbap_cart_z_size,
            control_vbap_cart_z_neg_size,
            control_vbap_table_mode,
            control_vbap_polar_azimuth_resolution,
            control_vbap_polar_elevation_resolution,
            control_vbap_polar_distance_res,
            control_vbap_polar_distance_max,
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
            control_export_layout,
            control_audio_sample_rate,
        ])
        .run(tauri::generate_context!())
        .expect("error running Tauri application");
}
