use rosc::{decoder, OscPacket, OscType};
use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app_state::OutputDeviceOption;
use crate::app_state::{
    AppState, DistanceDiffuse, Meter, RenderBackendState, RoomRatio, SpreadState,
    VbapCartesian, VbapPolar,
};
use crate::layouts::{Layout, Speaker};
use crate::osc_parser::{
    is_heartbeat_address, parse_osc_message, CoordinateFormat, HeartbeatResponse, LogEntry,
    OscEvent,
};

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const HEARTBEAT_ACK_TIMEOUT: Duration = Duration::from_secs(10);
const SNAPSHOT_REQUEST_INTERVAL: Duration = Duration::from_secs(1);

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AudioDomainState {
    output_devices: Option<Vec<OutputDeviceOption>>,
    output_device: Option<String>,
    output_device_effective: Option<String>,
    sample_rate: Option<u32>,
    sample_format: Option<String>,
    error: Option<String>,
    adaptive_resampling: Option<AudioAdaptiveDomainState>,
    latency_target_ms: Option<u32>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AudioAdaptiveDomainState {
    enabled: Option<bool>,
    enable_far_mode: Option<bool>,
    force_silence_in_far_mode: Option<bool>,
    hard_recover_high_in_far_mode: Option<bool>,
    hard_recover_low_in_far_mode: Option<bool>,
    far_mode_return_fade_in_ms: Option<u32>,
    kp_near: Option<f64>,
    ki: Option<f64>,
    integral_discharge_ratio: Option<f64>,
    max_adjust: Option<f64>,
    high_recover_entry_margin_ms: Option<u32>,
    update_interval_callbacks: Option<u32>,
    low_recover_settle_stable_ms: Option<f32>,
    low_recover_entry_margin_ms: Option<f32>,
    low_recover_exit_margin_ms: Option<f32>,
    low_recover_settle_margin_ms: Option<f32>,
    low_recover_refill_delta_alpha: Option<f32>,
    control_smoothing_cutoff_hz: Option<f64>,
    control_smoothing_order: Option<u32>,
    paused: Option<bool>,
    use_pre_bridge_clock: Option<bool>,
    use_output_pacing: Option<bool>,
    disable_backpressure: Option<bool>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct InputDomainState {
    mode: Option<String>,
    active_mode: Option<String>,
    apply_pending: Option<bool>,
    drc_mode: Option<String>,
    drc_weight: Option<f32>,
    supported_drc_modes: Option<Vec<String>>,
    requested: Option<RequestedInputDomainState>,
    applied: Option<AppliedInputDomainState>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RequestedInputDomainState {
    backend: Option<String>,
    node: Option<String>,
    description: Option<String>,
    layout: Option<String>,
    clock_mode: Option<String>,
    channels: Option<u32>,
    sample_rate: Option<u32>,
    format: Option<String>,
    map: Option<String>,
    lfe_mode: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AppliedInputDomainState {
    backend: Option<String>,
    channels: Option<u32>,
    sample_rate: Option<u32>,
    node: Option<String>,
    description: Option<String>,
    stream_format: Option<String>,
    error: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RendererDomainState {
    render_backend: Option<String>,
    render_backend_effective: Option<String>,
    render_evaluation_mode: Option<String>,
    render_evaluation_mode_effective: Option<String>,
    master_gain: Option<f64>,
    ramp_mode: Option<String>,
    distance_model: Option<String>,
    distance_model_metric: Option<String>,
    room_ratio: Option<RoomRatio>,
    spread: Option<SpreadState>,
    distance_diffuse: Option<DistanceDiffuse>,
    vbap_cartesian: Option<VbapCartesian>,
    vbap_polar: Option<VbapPolar>,
    render_backend_state: Option<RenderBackendState>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LoudnessDomainState {
    enabled: Option<bool>,
    source: Option<f64>,
    gain: Option<f64>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct MonitoringDomainState {
    meter_rate_hz: Option<f32>,
    diag_rate_hz: Option<f32>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct LayoutDomainState {
    name: Option<String>,
    radius_m: Option<f64>,
    #[serde(default)]
    speakers: Vec<LayoutDomainSpeakerState>,
}

#[derive(serde::Deserialize, Default)]
struct LayoutDomainSpeakerState {
    #[serde(default)]
    id: Option<serde_json::Value>,
    #[serde(default)]
    name: Option<serde_json::Value>,
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
    #[serde(default)]
    z: Option<f64>,
    #[serde(default, alias = "az", alias = "azimuthDeg", alias = "azimuth_deg")]
    azimuth: Option<f64>,
    #[serde(default, alias = "el", alias = "elevationDeg", alias = "elevation_deg")]
    elevation: Option<f64>,
    #[serde(default, alias = "dist", alias = "distanceM", alias = "distance_m")]
    distance: Option<f64>,
    #[serde(default, alias = "coordinate_mode", alias = "coordMode")]
    coord_mode: Option<String>,
    #[serde(default, alias = "delay")]
    delay_ms: Option<f64>,
    #[serde(default)]
    spatialize: Option<serde_json::Value>,
    #[serde(default, alias = "freq_low")]
    freq_low: Option<f32>,
    #[serde(default, alias = "freq_high")]
    freq_high: Option<f32>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpeakersDomainState {
    #[serde(default)]
    speakers: Vec<SpeakerRuntimeState>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct SpeakerRuntimeState {
    id: u32,
    gain: Option<f64>,
    #[serde(rename = "delayMs")]
    delay_ms: Option<f64>,
    muted: Option<bool>,
}

fn clamp_layout_value(value: f64, min: f64, max: f64) -> f64 {
    value.max(min).min(max)
}

fn cartesian_to_spherical(x: f64, y: f64, z: f64) -> (f64, f64, f64) {
    let distance = (x * x + y * y + z * z).sqrt();
    let azimuth = z.atan2(x).to_degrees();
    let elevation = if distance > 0.0 {
        y.atan2((x * x + z * z).sqrt()).to_degrees()
    } else {
        0.0
    };
    (azimuth, elevation, distance)
}

fn spherical_to_cartesian(
    azimuth_deg: f64,
    elevation_deg: f64,
    distance_m: f64,
) -> (f64, f64, f64) {
    let azimuth = azimuth_deg.to_radians();
    let elevation = elevation_deg.to_radians();
    (
        distance_m * elevation.cos() * azimuth.cos(),
        distance_m * elevation.sin(),
        distance_m * elevation.cos() * azimuth.sin(),
    )
}

fn scalar_string(value: Option<serde_json::Value>, fallback: &str) -> String {
    match value {
        Some(serde_json::Value::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                fallback.to_string()
            } else {
                trimmed.to_string()
            }
        }
        Some(serde_json::Value::Number(n)) => n.to_string(),
        Some(serde_json::Value::Bool(v)) => {
            if v {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        _ => fallback.to_string(),
    }
}

fn scalar_spatialize(value: Option<serde_json::Value>) -> u8 {
    match value {
        Some(serde_json::Value::Bool(false)) => 0,
        Some(serde_json::Value::Bool(true)) => 1,
        Some(serde_json::Value::Number(n)) if n.as_f64().unwrap_or(1.0) == 0.0 => 0,
        Some(serde_json::Value::String(s)) if s.trim() == "0" => 0,
        _ => 1,
    }
}

fn normalized_layout_domain_speaker(raw: LayoutDomainSpeakerState) -> Speaker {
    let id = scalar_string(raw.id.or(raw.name), "spk");
    let coord_mode = raw
        .coord_mode
        .as_deref()
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            if raw.x.is_some() && raw.y.is_some() && raw.z.is_some() {
                "cartesian".to_string()
            } else {
                "polar".to_string()
            }
        });
    let spatialize = scalar_spatialize(raw.spatialize);
    let delay_ms = raw.delay_ms.unwrap_or(0.0).max(0.0);
    let freq_low = raw.freq_low.filter(|value| *value > 0.0);
    let freq_high = raw.freq_high.filter(|value| *value > 0.0);

    if let (Some(x), Some(y), Some(z)) = (raw.x, raw.y, raw.z) {
        let x = clamp_layout_value(x, -1.0, 1.0);
        let y = clamp_layout_value(y, -1.0, 1.0);
        let z = clamp_layout_value(z, -1.0, 1.0);
        let (fallback_azimuth, fallback_elevation, fallback_distance) =
            cartesian_to_spherical(x, y, z);
        return Speaker {
            id,
            x,
            y,
            z,
            azimuth_deg: raw.azimuth.unwrap_or(fallback_azimuth),
            elevation_deg: raw.elevation.unwrap_or(fallback_elevation),
            distance_m: raw.distance.unwrap_or(fallback_distance).max(0.01),
            coord_mode: if coord_mode == "cartesian" {
                "cartesian".to_string()
            } else {
                "polar".to_string()
            },
            spatialize,
            delay_ms,
            freq_low,
            freq_high,
        };
    }

    let azimuth = raw.azimuth.unwrap_or(0.0);
    let elevation = raw.elevation.unwrap_or(0.0);
    let distance_m = raw.distance.unwrap_or(1.0).max(0.01);
    let (x, y, z) = spherical_to_cartesian(azimuth, elevation, distance_m);
    Speaker {
        id,
        x: clamp_layout_value(x, -1.0, 1.0),
        y: clamp_layout_value(y, -1.0, 1.0),
        z: clamp_layout_value(z, -1.0, 1.0),
        azimuth_deg: azimuth,
        elevation_deg: elevation,
        distance_m,
        coord_mode: if coord_mode == "cartesian" {
            "cartesian".to_string()
        } else {
            "polar".to_string()
        },
        spatialize,
        delay_ms,
        freq_low,
        freq_high,
    }
}

fn layout_update_payload(s: &AppState) -> serde_json::Value {
    serde_json::json!({
        "layouts": s.layouts,
        "selectedLayoutKey": s.selected_layout_key
    })
}

fn apply_layout_domain_state(s: &mut AppState, value: &str) -> bool {
    // Dedup at the byte-equality level: the renderer re-broadcasts the full
    // layout JSON on stage, on apply and post-recompute, often with identical
    // content. Returning false skips the `layouts:update` emit downstream.
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    let new_hash = hasher.finish();
    if s.last_layout_state_hash == Some(new_hash) {
        return false;
    }

    let Ok(parsed) = serde_json::from_str::<LayoutDomainState>(value) else {
        return false;
    };
    let speakers = parsed
        .speakers
        .into_iter()
        .map(normalized_layout_domain_speaker)
        .collect::<Vec<_>>();
    if speakers.is_empty() {
        return false;
    }

    let layout = Layout {
        key: "omniphony-live".to_string(),
        name: parsed
            .name
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "omniphony (live)".to_string()),
        speakers,
        radius_m: parsed.radius_m.unwrap_or(1.0).max(0.01),
    };

    s.layouts.retain(|entry| entry.key != "omniphony-live");
    s.layouts.insert(0, layout);
    s.selected_layout_key = Some("omniphony-live".to_string());
    s.last_layout_state_hash = Some(new_hash);
    true
}

fn apply_speakers_domain_state(s: &mut AppState, value: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<SpeakersDomainState>(value) else {
        return false;
    };

    s.speaker_gains.clear();
    s.speaker_mutes.clear();

    if let Some(layout) = s
        .layouts
        .iter_mut()
        .find(|entry| entry.key == "omniphony-live")
    {
        for speaker in &parsed.speakers {
            let key = speaker.id.to_string();
            let gain = speaker.gain.unwrap_or(1.0).clamp(0.0, 2.0);
            if (gain - 1.0).abs() > f64::EPSILON {
                s.speaker_gains.insert(key.clone(), gain);
            }
            if speaker.muted.unwrap_or(false) {
                s.speaker_mutes.insert(key.clone(), 1);
            }
            if let Some(delay_ms) = speaker.delay_ms {
                if let Some(layout_speaker) = layout.speakers.get_mut(speaker.id as usize) {
                    layout_speaker.delay_ms = delay_ms.max(0.0);
                }
            }
        }
    }

    true
}

fn apply_audio_domain_state(s: &mut AppState, value: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<AudioDomainState>(value) else {
        return false;
    };
    if let Some(devices) = parsed.output_devices {
        s.set_audio_output_devices(devices);
    }
    if let Some(output_device) = parsed.output_device {
        s.set_audio_requested_output_device(&output_device);
    }
    if let Some(output_device_effective) = parsed.output_device_effective {
        s.set_audio_effective_output_device(&output_device_effective);
    }
    if let Some(sample_rate) = parsed.sample_rate {
        s.set_audio_sample_rate_value(sample_rate);
    }
    if let Some(sample_format) = parsed.sample_format {
        s.set_audio_sample_format(sample_format);
    }
    if let Some(error) = parsed.error {
        s.set_audio_error(&error);
    }
    if let Some(adaptive) = parsed.adaptive_resampling {
        if let Some(enabled) = adaptive.enabled {
            s.adaptive_resampling = Some(if enabled { 1 } else { 0 });
        }
        if let Some(enabled) = adaptive.enable_far_mode {
            s.adaptive_resampling_enable_far_mode = Some(if enabled { 1 } else { 0 });
        }
        if let Some(enabled) = adaptive.force_silence_in_far_mode {
            s.adaptive_resampling_force_silence_in_far_mode = Some(if enabled { 1 } else { 0 });
        }
        if let Some(enabled) = adaptive.hard_recover_high_in_far_mode {
            s.adaptive_resampling_hard_recover_high_in_far_mode = Some(if enabled { 1 } else { 0 });
        }
        if let Some(enabled) = adaptive.hard_recover_low_in_far_mode {
            s.adaptive_resampling_hard_recover_low_in_far_mode = Some(if enabled { 1 } else { 0 });
        }
        if let Some(value) = adaptive.far_mode_return_fade_in_ms {
            s.adaptive_resampling_far_mode_return_fade_in_ms = Some(value as i64);
        }
        if let Some(value) = adaptive.kp_near {
            s.adaptive_resampling_kp_near = Some(value);
        }
        if let Some(value) = adaptive.ki {
            s.adaptive_resampling_ki = Some(value);
        }
        if let Some(value) = adaptive.integral_discharge_ratio {
            s.adaptive_resampling_integral_discharge_ratio = Some(value);
        }
        if let Some(value) = adaptive.max_adjust {
            s.adaptive_resampling_max_adjust = Some(value);
        }
        if let Some(value) = adaptive.high_recover_entry_margin_ms {
            s.adaptive_resampling_high_recover_entry_margin_ms = Some(value as i64);
        }
        if let Some(value) = adaptive.update_interval_callbacks {
            s.adaptive_resampling_update_interval_callbacks = Some(value as i64);
        }
        if let Some(value) = adaptive.low_recover_settle_stable_ms {
            s.adaptive_resampling_low_recover_settle_stable_ms = Some(value as f64);
        }
        if let Some(value) = adaptive.low_recover_entry_margin_ms {
            s.adaptive_resampling_low_recover_entry_margin_ms = Some(value as f64);
        }
        if let Some(value) = adaptive.low_recover_exit_margin_ms {
            s.adaptive_resampling_low_recover_exit_margin_ms = Some(value as f64);
        }
        if let Some(value) = adaptive.low_recover_settle_margin_ms {
            s.adaptive_resampling_low_recover_settle_margin_ms = Some(value as f64);
        }
        if let Some(value) = adaptive.low_recover_refill_delta_alpha {
            s.adaptive_resampling_low_recover_refill_delta_alpha = Some(value as f64);
        }
        if let Some(value) = adaptive.control_smoothing_cutoff_hz {
            s.adaptive_resampling_control_smoothing_cutoff_hz = Some(value);
        }
        if let Some(value) = adaptive.control_smoothing_order {
            s.adaptive_resampling_control_smoothing_order = Some(value);
        }
        if let Some(paused) = adaptive.paused {
            s.adaptive_resampling_paused = Some(if paused { 1 } else { 0 });
        }
        if let Some(enabled) = adaptive.use_pre_bridge_clock {
            s.adaptive_resampling_use_pre_bridge_clock = Some(if enabled { 1 } else { 0 });
        }
        if let Some(enabled) = adaptive.use_output_pacing {
            s.adaptive_resampling_use_output_pacing = Some(if enabled { 1 } else { 0 });
        }
        if let Some(disabled) = adaptive.disable_backpressure {
            s.adaptive_resampling_disable_backpressure = Some(if disabled { 1 } else { 0 });
        }
    }
    if let Some(latency_target_ms) = parsed.latency_target_ms {
        s.latency.latency_target_ms = Some(latency_target_ms as i64);
        s.latency.latency_requested_ms = Some(latency_target_ms as i64);
    }
    true
}

fn apply_input_domain_state(s: &mut AppState, value: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<InputDomainState>(value) else {
        return false;
    };
    if let Some(mode) = parsed.mode {
        s.input_mode = Some(mode);
    }
    if let Some(active_mode) = parsed.active_mode {
        s.input_active_mode = Some(active_mode);
    }
    if let Some(apply_pending) = parsed.apply_pending {
        s.input_apply_pending = Some(if apply_pending { 1 } else { 0 });
    }
    if let Some(drc_mode) = parsed.drc_mode {
        s.drc_mode = Some(drc_mode);
    }
    if let Some(drc_weight) = parsed.drc_weight {
        s.drc_weight = Some(drc_weight.clamp(0.0, 1.0));
    }
    if let Some(supported_drc_modes) = parsed.supported_drc_modes {
        s.supported_drc_modes = supported_drc_modes;
    }
    if let Some(requested) = parsed.requested {
        s.live_input.backend = requested.backend;
        s.live_input.node = requested.node;
        s.live_input.description = requested.description;
        s.live_input.layout = requested.layout;
        s.live_input.clock_mode = requested.clock_mode;
        s.live_input.channels = requested.channels;
        s.live_input.sample_rate = requested.sample_rate;
        s.live_input.format = requested.format;
        s.live_input.map = requested.map;
        s.live_input.lfe_mode = requested.lfe_mode;
    }
    if let Some(applied) = parsed.applied {
        s.input_backend = applied.backend;
        s.input_channels = applied.channels;
        s.input_sample_rate = applied.sample_rate;
        s.input_node = applied.node;
        s.input_description = applied.description;
        s.input_stream_format = applied.stream_format;
        s.input_error = applied.error;
    }
    true
}

fn apply_renderer_domain_state(s: &mut AppState, value: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<RendererDomainState>(value) else {
        return false;
    };
    if let Some(render_backend) = parsed.render_backend {
        s.render_backend_state.selection = Some(render_backend);
    }
    if let Some(render_backend_effective) = parsed.render_backend_effective {
        s.render_backend_state.effective = Some(render_backend_effective);
    }
    if let Some(render_evaluation_mode) = parsed.render_evaluation_mode {
        s.render_evaluation_mode_state.selection = Some(render_evaluation_mode);
    }
    if let Some(render_evaluation_mode_effective) = parsed.render_evaluation_mode_effective {
        s.render_evaluation_mode_state.effective = Some(render_evaluation_mode_effective);
    }
    if let Some(master_gain) = parsed.master_gain {
        s.master_gain = Some(master_gain);
    }
    if let Some(ramp_mode) = parsed.ramp_mode {
        s.audio.ramp_mode = Some(ramp_mode);
    }
    if let Some(distance_model) = parsed.distance_model {
        s.distance_model.value = Some(distance_model);
    }
    if let Some(distance_model_metric) = parsed.distance_model_metric {
        s.distance_model.metric = Some(distance_model_metric);
    }
    if let Some(room_ratio) = parsed.room_ratio {
        s.room_ratio = room_ratio;
    }
    if let Some(spread) = parsed.spread {
        s.spread = spread;
    }
    if let Some(distance_diffuse) = parsed.distance_diffuse {
        s.distance_diffuse = distance_diffuse;
    }
    if let Some(vbap_cartesian) = parsed.vbap_cartesian {
        s.vbap_cartesian = vbap_cartesian;
    }
    if let Some(vbap_polar) = parsed.vbap_polar {
        s.vbap_polar = vbap_polar;
    }
    if let Some(render_backend_state) = parsed.render_backend_state {
        s.render_backend_state = render_backend_state;
    }
    true
}

fn apply_loudness_domain_state(s: &mut AppState, value: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<LoudnessDomainState>(value) else {
        return false;
    };
    if let Some(enabled) = parsed.enabled {
        s.loudness = Some(if enabled { 1 } else { 0 });
    }
    s.loudness_source = parsed.source;
    s.loudness_gain = parsed.gain;
    true
}

fn apply_monitoring_domain_state(s: &mut AppState, value: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<MonitoringDomainState>(value) else {
        return false;
    };
    if parsed.meter_rate_hz.is_some() {
        s.meter_rate_hz = parsed.meter_rate_hz;
    }
    if parsed.diag_rate_hz.is_some() {
        s.diag_rate_hz = parsed.diag_rate_hz;
    }
    true
}

// ── control messages (frontend → OSC listener) ────────────────────────────

pub enum OscControlMsg {
    SendFloat {
        address: String,
        value: f32,
    },
    SendInt {
        address: String,
        value: i32,
    },
    SendNoArgs {
        address: String,
    },
    SendString {
        address: String,
        value: String,
    },
    SendFloats3 {
        address: String,
        a: f32,
        b: f32,
        c: f32,
    },
    SendArgs {
        address: String,
        args: Vec<OscType>,
    },
    Reconnect {
        host: String,
        rx_port: u16,
        listen_port: u16,
    },
    SetMeteringEnabled {
        enabled: bool,
    },
}

// ── OSC send helpers ─────────────────────────────────────────────────────

fn send_osc_float(socket: &UdpSocket, addr: &str, host: &str, rx_port: u16, value: f32) {
    use rosc::{encoder, OscMessage, OscType};
    let msg = OscPacket::Message(OscMessage {
        addr: addr.to_string(),
        args: vec![OscType::Float(value)],
    });
    if let Ok(data) = encoder::encode(&msg) {
        let _ = socket.send_to(&data, format!("{host}:{rx_port}"));
    }
}

fn send_osc_int(socket: &UdpSocket, addr: &str, host: &str, rx_port: u16, value: i32) {
    use rosc::{encoder, OscMessage, OscType};
    let msg = OscPacket::Message(OscMessage {
        addr: addr.to_string(),
        args: vec![OscType::Int(value)],
    });
    if let Ok(data) = encoder::encode(&msg) {
        let _ = socket.send_to(&data, format!("{host}:{rx_port}"));
    }
}

fn send_osc_no_args(socket: &UdpSocket, addr: &str, host: &str, rx_port: u16) {
    use rosc::{encoder, OscMessage};
    let msg = OscPacket::Message(OscMessage {
        addr: addr.to_string(),
        args: vec![],
    });
    if let Ok(data) = encoder::encode(&msg) {
        let _ = socket.send_to(&data, format!("{host}:{rx_port}"));
    }
}

fn send_osc_string(socket: &UdpSocket, addr: &str, host: &str, rx_port: u16, value: &str) {
    use rosc::{encoder, OscMessage, OscType};
    let msg = OscPacket::Message(OscMessage {
        addr: addr.to_string(),
        args: vec![OscType::String(value.to_string())],
    });
    if let Ok(data) = encoder::encode(&msg) {
        let _ = socket.send_to(&data, format!("{host}:{rx_port}"));
    }
}

fn send_osc_floats3(
    socket: &UdpSocket,
    addr: &str,
    host: &str,
    rx_port: u16,
    a: f32,
    b: f32,
    c: f32,
) {
    use rosc::{encoder, OscMessage, OscType};
    let msg = OscPacket::Message(OscMessage {
        addr: addr.to_string(),
        args: vec![OscType::Float(a), OscType::Float(b), OscType::Float(c)],
    });
    if let Ok(data) = encoder::encode(&msg) {
        let _ = socket.send_to(&data, format!("{host}:{rx_port}"));
    }
}

fn send_osc_args(socket: &UdpSocket, addr: &str, host: &str, rx_port: u16, args: Vec<OscType>) {
    use rosc::{encoder, OscMessage};
    let msg = OscPacket::Message(OscMessage {
        addr: addr.to_string(),
        args,
    });
    if let Ok(data) = encoder::encode(&msg) {
        let _ = socket.send_to(&data, format!("{host}:{rx_port}"));
    }
}

fn send_register(socket: &UdpSocket, host: &str, rx_port: u16, listen_port: u16) {
    send_osc_int(
        socket,
        "/omniphony/register",
        host,
        rx_port,
        listen_port as i32,
    );
    log::info!("[osc] register sent → udp://{host}:{rx_port} listen_port={listen_port}");
}

fn send_metering_enabled(socket: &UdpSocket, host: &str, rx_port: u16, enabled: bool) {
    send_osc_int(
        socket,
        "/omniphony/control/metering",
        host,
        rx_port,
        if enabled { 1 } else { 0 },
    );
}

fn send_heartbeat(socket: &UdpSocket, host: &str, rx_port: u16, listen_port: u16) {
    send_osc_int(
        socket,
        "/omniphony/heartbeat",
        host,
        rx_port,
        listen_port as i32,
    );
}

fn emit_osc_status(app: &AppHandle, state: &Arc<Mutex<AppState>>, status: &str) {
    {
        let mut s = state.lock().unwrap();
        if status != "connected" {
            s.reset_runtime_state();
            s.osc_snapshot_ready = false;
        }
        s.osc_status = Some(status.to_string());
    }
    let _ = app.emit("osc:status", serde_json::json!({ "status": status }));
}

// ── public spawn function ─────────────────────────────────────────────────

pub fn spawn_osc_task(
    app: AppHandle,
    state: Arc<Mutex<AppState>>,
    host: String,
    osc_port: u16,
    osc_rx_port: u16,
    ctrl_rx: UnboundedReceiver<OscControlMsg>,
    listen_port_out: Arc<Mutex<u16>>,
) {
    // The mpv overlay is now generated in-process by orender (liborender.so)
    // and pulled over FFI by a small mpv Lua shim — Studio no longer talks to
    // mpv over the JSON IPC socket at all. Overlay config (enable / labels /
    // trails) travels as OSC control (see the `mpv_overlay_set_*` commands).
    std::thread::spawn(move || {
        osc_thread(
            app,
            state,
            host,
            osc_port,
            osc_rx_port,
            ctrl_rx,
            listen_port_out,
        );
    });
}

fn osc_thread(
    app: AppHandle,
    state: Arc<Mutex<AppState>>,
    mut host: String,
    osc_port: u16,
    mut osc_rx_port: u16,
    mut ctrl_rx: UnboundedReceiver<OscControlMsg>,
    listen_port_out: Arc<Mutex<u16>>,
) {
    let bind_addr = format!("0.0.0.0:{osc_port}");
    let socket = match UdpSocket::bind(&bind_addr) {
        Ok(s) => s,
        Err(e) => {
            log::error!("[osc] bind failed: {e}");
            emit_osc_status(&app, &state, "error");
            return;
        }
    };
    socket
        .set_read_timeout(Some(Duration::from_millis(50)))
        .ok();

    let listen_port = socket.local_addr().map(|a| a.port()).unwrap_or(osc_port);
    *listen_port_out.lock().unwrap() = listen_port;
    log::info!("[osc] listening on udp://0.0.0.0:{listen_port}");

    send_register(&socket, &host, osc_rx_port, listen_port);
    let mut last_snapshot_request_at = Instant::now();
    let mut last_ack_at = Instant::now();
    let mut last_heartbeat_at = Instant::now();
    let mut is_connected = false;
    let mut metering_enabled = state.lock().unwrap().osc_metering_enabled.unwrap_or(0) != 0;
    send_metering_enabled(&socket, &host, osc_rx_port, metering_enabled);
    emit_osc_status(&app, &state, "reconnecting");

    let mut buf = [0u8; 65536];

    loop {
        // drain control messages (non-blocking)
        loop {
            match ctrl_rx.try_recv() {
                Ok(msg) => match msg {
                    OscControlMsg::SendFloat { address, value } => {
                        send_osc_float(&socket, &address, &host, osc_rx_port, value);
                    }
                    OscControlMsg::SendInt { address, value } => {
                        send_osc_int(&socket, &address, &host, osc_rx_port, value);
                    }
                    OscControlMsg::SendNoArgs { address } => {
                        send_osc_no_args(&socket, &address, &host, osc_rx_port);
                    }
                    OscControlMsg::SendString { address, value } => {
                        send_osc_string(&socket, &address, &host, osc_rx_port, &value);
                    }
                    OscControlMsg::SendFloats3 { address, a, b, c } => {
                        send_osc_floats3(&socket, &address, &host, osc_rx_port, a, b, c);
                    }
                    OscControlMsg::SendArgs { address, args } => {
                        send_osc_args(&socket, &address, &host, osc_rx_port, args);
                    }
                    OscControlMsg::Reconnect {
                        host: h,
                        rx_port,
                        listen_port: lp,
                    } => {
                        host = h;
                        osc_rx_port = rx_port;
                        send_register(&socket, &host, osc_rx_port, lp);
                        last_snapshot_request_at = Instant::now();
                        send_metering_enabled(&socket, &host, osc_rx_port, metering_enabled);
                        last_ack_at = Instant::now();
                        if is_connected {
                            is_connected = false;
                        }
                        emit_osc_status(&app, &state, "reconnecting");
                    }
                    OscControlMsg::SetMeteringEnabled { enabled } => {
                        metering_enabled = enabled;
                        send_metering_enabled(&socket, &host, osc_rx_port, enabled);
                    }
                },
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(_) => return, // channel closed
            }
        }

        let snapshot_ready = state.lock().unwrap().osc_snapshot_ready;
        if !snapshot_ready && last_snapshot_request_at.elapsed() >= SNAPSHOT_REQUEST_INTERVAL {
            send_register(&socket, &host, osc_rx_port, listen_port);
            send_metering_enabled(&socket, &host, osc_rx_port, metering_enabled);
            last_snapshot_request_at = Instant::now();
            log::debug!("[osc] snapshot not ready yet, re-requesting live state bundle");
        }

        // heartbeat timer
        if last_heartbeat_at.elapsed() >= HEARTBEAT_INTERVAL {
            last_heartbeat_at = Instant::now();
            send_heartbeat(&socket, &host, osc_rx_port, listen_port);

            if last_ack_at.elapsed() >= HEARTBEAT_ACK_TIMEOUT {
                log::warn!("[osc] heartbeat timeout, re-registering");
                if is_connected {
                    is_connected = false;
                    emit_osc_status(&app, &state, "reconnecting");
                }
                send_register(&socket, &host, osc_rx_port, listen_port);
                last_snapshot_request_at = Instant::now();
                send_metering_enabled(&socket, &host, osc_rx_port, metering_enabled);
            }
        }

        // receive packet
        let n = match socket.recv_from(&mut buf) {
            Ok((n, _)) => n,
            Err(_) => continue, // timeout
        };

        match decoder::decode_udp(&buf[..n]) {
            Ok((_, packet)) => {
                handle_packet(
                    packet,
                    &app,
                    &state,
                    &socket,
                    &host,
                    osc_rx_port,
                    listen_port,
                    metering_enabled,
                    &mut last_ack_at,
                    &mut is_connected,
                );
            }
            Err(_) => {}
        }
    }
}

// The legacy "compact the scene into a CSV and push it to mpv over the JSON IPC
// socket" overlay path lived here. The overlay is now generated in-process by
// orender (see `orender_engine::overlay`) and pulled over FFI by the mpv Lua
// shim, so Studio no longer builds frames or mirrors the colour palette — the
// renderer owns both. Overlay config (trails) is sent as OSC control.

fn handle_packet(
    packet: OscPacket,
    app: &AppHandle,
    state: &Arc<Mutex<AppState>>,
    socket: &UdpSocket,
    host: &str,
    osc_rx_port: u16,
    listen_port: u16,
    metering_enabled: bool,
    last_ack_at: &mut Instant,
    is_connected: &mut bool,
) {
    match packet {
        OscPacket::Message(msg) => {
            match is_heartbeat_address(&msg.addr) {
                HeartbeatResponse::Ack => {
                    *last_ack_at = Instant::now();
                    if !*is_connected {
                        *is_connected = true;
                        emit_osc_status(app, state, "connected");
                    }
                    return;
                }
                HeartbeatResponse::Unknown => {
                    log::info!("[osc] heartbeat/unknown → re-registering");
                    send_register(socket, host, osc_rx_port, listen_port);
                    send_metering_enabled(socket, host, osc_rx_port, metering_enabled);
                    *last_ack_at = Instant::now();
                    if *is_connected {
                        *is_connected = false;
                        emit_osc_status(app, state, "reconnecting");
                    }
                    return;
                }
                HeartbeatResponse::None => {}
            }

            let coordinate_format = {
                let s = state.lock().unwrap();
                if s.current_coordinate_format == 1 {
                    CoordinateFormat::Polar
                } else {
                    CoordinateFormat::Cartesian
                }
            };

            if let Some(ev) = parse_osc_message(&msg.addr, &msg.args, coordinate_format) {
                if !*is_connected {
                    *is_connected = true;
                    emit_osc_status(app, state, "connected");
                }
                handle_event(ev, app, state);
            }
        }
        OscPacket::Bundle(bundle) => {
            for pkt in bundle.content {
                match pkt {
                    OscPacket::Message(msg) => {
                        match is_heartbeat_address(&msg.addr) {
                            HeartbeatResponse::Ack => {
                                *last_ack_at = Instant::now();
                                if !*is_connected {
                                    *is_connected = true;
                                    emit_osc_status(app, state, "connected");
                                }
                                continue;
                            }
                            HeartbeatResponse::Unknown => {
                                send_register(socket, host, osc_rx_port, listen_port);
                                send_metering_enabled(socket, host, osc_rx_port, metering_enabled);
                                *last_ack_at = Instant::now();
                                if *is_connected {
                                    *is_connected = false;
                                    emit_osc_status(app, state, "reconnecting");
                                }
                                continue;
                            }
                            HeartbeatResponse::None => {}
                        }

                        let coordinate_format = {
                            let s = state.lock().unwrap();
                            if s.current_coordinate_format == 1 {
                                CoordinateFormat::Polar
                            } else {
                                CoordinateFormat::Cartesian
                            }
                        };

                        if let Some(ev) = parse_osc_message(&msg.addr, &msg.args, coordinate_format)
                        {
                            if !*is_connected {
                                *is_connected = true;
                                emit_osc_status(app, state, "connected");
                            }
                            handle_event(ev, app, state);
                        }
                    }
                    OscPacket::Bundle(inner) => {
                        for pkt2 in inner.content {
                            if let OscPacket::Message(msg) = pkt2 {
                                let coordinate_format = {
                                    let s = state.lock().unwrap();
                                    if s.current_coordinate_format == 1 {
                                        CoordinateFormat::Polar
                                    } else {
                                        CoordinateFormat::Cartesian
                                    }
                                };

                                if let Some(ev) =
                                    parse_osc_message(&msg.addr, &msg.args, coordinate_format)
                                {
                                    if !*is_connected {
                                        *is_connected = true;
                                        emit_osc_status(app, state, "connected");
                                    }
                                    handle_event(ev, app, state);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn handle_event(ev: OscEvent, app: &AppHandle, state: &Arc<Mutex<AppState>>) {
    // Update state under the lock, collect emit data, then release before emitting.
    let (to_emit, removed_ids): (Option<(&'static str, serde_json::Value)>, Vec<String>) = {
        let mut s = state.lock().unwrap();
        let mut removed_ids: Vec<String> = Vec::new();
        match ev {
            OscEvent::SpatialFrame {
                sample_pos,
                generation,
                object_count,
                coordinate_format,
            } => {
                let generation_changed = s
                    .current_content_generation
                    .is_some_and(|prev| prev != generation);
                let is_reset = generation_changed
                    || s.last_spatial_sample_pos
                        .is_some_and(|prev| sample_pos < prev);
                s.last_spatial_sample_pos = Some(sample_pos);
                s.current_content_generation = Some(generation);
                s.current_coordinate_format = coordinate_format;

                let stale_ids: Vec<String> = if is_reset {
                    s.sources.keys().cloned().collect()
                } else {
                    s.sources
                        .keys()
                        .filter_map(|id| {
                            id.parse::<u32>().ok().and_then(|idx| {
                                if idx >= object_count {
                                    Some(id.clone())
                                } else {
                                    None
                                }
                            })
                        })
                        .collect()
                };

                for id in &stale_ids {
                    s.sources.remove(id);
                    s.source_levels.remove(id);
                    s.object_speaker_gains.remove(id);
                    s.object_gains.remove(id);
                    s.object_mutes.remove(id);
                }
                removed_ids.extend(stale_ids);
                (
                    Some((
                        "spatial:frame",
                        serde_json::json!({
                            "samplePos": sample_pos,
                            "generation": generation,
                            "objectCount": object_count,
                            "coordinateFormat": coordinate_format,
                            "reset": is_reset
                        }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::Update { id, position, name } => {
                let current_generation = s.current_content_generation;
                let entry = s.sources.entry(id.clone()).or_default();
                entry.x = position.x;
                entry.y = position.y;
                entry.z = position.z;
                entry.coord_mode = Some(position.coord_mode.clone());
                entry.azimuth_deg = position.azimuth_deg;
                entry.elevation_deg = position.elevation_deg;
                entry.distance_m = position.distance_m;
                entry.gain_db = position.gain_db;
                entry.generation = position.generation.or(current_generation);
                entry.direct_speaker_index = position.direct_speaker_index;
                if let Some(source_tag) = position.source_tag {
                    entry.source_tag = Some(source_tag);
                }
                if let Some(n) = name {
                    entry.name = Some(n);
                }
                let payload = serde_json::json!({
                    "id": id,
                    "position": {
                            "x": entry.x,
                            "y": entry.y,
                            "z": entry.z,
                            "coordMode": entry.coord_mode,
                            "azimuthDeg": entry.azimuth_deg,
                            "elevationDeg": entry.elevation_deg,
                            "distanceM": entry.distance_m,
                            "gainDb": entry.gain_db,
                            "generation": entry.generation,
                            "directSpeakerIndex": entry.direct_speaker_index,
                            "sourceTag": entry.source_tag,
                            "name": entry.name
                        }
                });
                (Some(("source:update", payload)), removed_ids)
            }

            OscEvent::UpdateSize { id, size, generation } => {
                let payload = serde_json::json!({
                    "id": id,
                    "size": { "w": size[0], "d": size[1], "h": size[2] },
                    "generation": generation,
                });
                (Some(("source:size", payload)), removed_ids)
            }

            OscEvent::Remove { id } => {
                s.sources.remove(&id);
                s.source_levels.remove(&id);
                s.object_speaker_gains.remove(&id);
                s.object_gains.remove(&id);
                s.object_mutes.remove(&id);
                (
                    Some(("source:remove", serde_json::json!({ "id": id }))),
                    removed_ids,
                )
            }

            OscEvent::MeterObject {
                id,
                peak_dbfs,
                rms_dbfs,
            } => {
                s.source_levels.insert(
                    id.clone(),
                    Meter {
                        peak_dbfs,
                        rms_dbfs,
                    },
                );
                (
                    Some((
                        "source:meter",
                        serde_json::json!({
                            "id": id,
                            "meter": { "peakDbfs": peak_dbfs, "rmsDbfs": rms_dbfs }
                        }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::MeterObjectGains { id, gains } => {
                s.object_speaker_gains.insert(id.clone(), gains.clone());
                (
                    Some((
                        "source:gains",
                        serde_json::json!({ "id": id, "gains": gains }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::MeterObjectBandGains { id, band, gains } => {
                let entry = s.object_band_gains.entry(id.clone()).or_default();
                if entry.len() <= band {
                    entry.resize(band + 1, Vec::new());
                }
                entry[band] = gains.clone();
                (
                    Some((
                        "source:band_gains",
                        serde_json::json!({ "id": id, "band": band, "gains": gains }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::MeterSpeaker {
                id,
                peak_dbfs,
                rms_dbfs,
            } => {
                s.speaker_levels.insert(
                    id.clone(),
                    Meter {
                        peak_dbfs,
                        rms_dbfs,
                    },
                );
                (
                    Some((
                        "speaker:meter",
                        serde_json::json!({
                            "id": id,
                            "meter": { "peakDbfs": peak_dbfs, "rmsDbfs": rms_dbfs }
                        }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::MeterDrcGain { value } => (
                Some((
                    "meter:drc_gain",
                    serde_json::json!({ "value": value }),
                )),
                removed_ids,
            ),

            OscEvent::StateSpeakerGain { id, gain } => {
                s.speaker_gains.insert(id.clone(), gain);
                (
                    Some((
                        "speaker:gain",
                        serde_json::json!({ "id": id, "gain": gain }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateObjectGain { id, gain } => {
                s.object_gains.insert(id.clone(), gain);
                (
                    Some(("object:gain", serde_json::json!({ "id": id, "gain": gain }))),
                    removed_ids,
                )
            }
            OscEvent::StateSpeakerDelay { id, delay_ms } => {
                if let Ok(index) = id.parse::<usize>() {
                    if let Some(layout_key) = s.selected_layout_key.clone() {
                        if let Some(layout) = s.layouts.iter_mut().find(|l| l.key == layout_key) {
                            if let Some(spk) = layout.speakers.get_mut(index) {
                                spk.delay_ms = delay_ms.max(0.0);
                            }
                        }
                    }
                }
                (
                    Some((
                        "speaker:delay",
                        serde_json::json!({ "id": id, "delayMs": delay_ms.max(0.0) }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateObjectMute { id, muted } => {
                if muted {
                    s.object_mutes.insert(id.clone(), 1);
                } else {
                    s.object_mutes.remove(&id);
                }
                (
                    Some((
                        "object:mute",
                        serde_json::json!({ "id": id, "muted": muted as u8 }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateObjectSourceTag { id, source_tag } => {
                let entry = s.sources.entry(id.clone()).or_default();
                entry.source_tag = Some(source_tag.clone());
                (
                    Some((
                        "object:source_tag",
                        serde_json::json!({ "id": id, "sourceTag": source_tag }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateSpeakerMute { id, muted } => {
                if muted {
                    s.speaker_mutes.insert(id.clone(), 1);
                } else {
                    s.speaker_mutes.remove(&id);
                }
                (
                    Some((
                        "speaker:mute",
                        serde_json::json!({ "id": id, "muted": muted as u8 }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateOscMetering { enabled } => {
                s.osc_metering_enabled = Some(if enabled { 1 } else { 0 });
                (
                    Some((
                        "osc:metering",
                        serde_json::json!({ "enabled": if enabled { 1 } else { 0 } }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateSpeakerSpatialize { id, spatialize } => {
                if let Ok(index) = id.parse::<usize>() {
                    if let Some(layout_key) = s.selected_layout_key.clone() {
                        if let Some(layout) = s.layouts.iter_mut().find(|l| l.key == layout_key) {
                            if let Some(spk) = layout.speakers.get_mut(index) {
                                spk.spatialize = if spatialize { 1 } else { 0 };
                            }
                        }
                    }
                }
                (
                    Some((
                        "speaker:spatialize",
                        serde_json::json!({ "id": id, "spatialize": if spatialize { 1 } else { 0 } }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateSpeakerName { id, name } => {
                if let Ok(index) = id.parse::<usize>() {
                    if let Some(layout_key) = s.selected_layout_key.clone() {
                        if let Some(layout) = s.layouts.iter_mut().find(|l| l.key == layout_key) {
                            if let Some(spk) = layout.speakers.get_mut(index) {
                                spk.id = name.clone();
                            }
                        }
                    }
                }
                (
                    Some((
                        "speaker:name",
                        serde_json::json!({ "id": id, "name": name }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateSpeakerFreqLow { id, freq_low } => {
                if let Ok(index) = id.parse::<usize>() {
                    if let Some(layout_key) = s.selected_layout_key.clone() {
                        if let Some(layout) = s.layouts.iter_mut().find(|l| l.key == layout_key) {
                            if let Some(spk) = layout.speakers.get_mut(index) {
                                spk.freq_low = freq_low;
                            }
                        }
                    }
                }
                (
                    Some((
                        "speaker:freq_low",
                        serde_json::json!({ "id": id, "freq_low": freq_low }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateSpeakerFreqHigh { id, freq_high } => {
                if let Ok(index) = id.parse::<usize>() {
                    if let Some(layout_key) = s.selected_layout_key.clone() {
                        if let Some(layout) = s.layouts.iter_mut().find(|l| l.key == layout_key) {
                            if let Some(spk) = layout.speakers.get_mut(index) {
                                spk.freq_high = freq_high;
                            }
                        }
                    }
                }
                (
                    Some((
                        "speaker:freq_high",
                        serde_json::json!({ "id": id, "freq_high": freq_high }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateCapabilities { value } => {
                s.producer_capabilities = serde_json::from_str(&value).ok();
                (None, removed_ids)
            }
            OscEvent::StateRenderer { value } => {
                if apply_renderer_domain_state(&mut s, &value) {
                    (
                        Some((
                            "state:snapshot_ready",
                            serde_json::to_value(&*s).unwrap_or_else(|_| serde_json::json!({})),
                        )),
                        removed_ids,
                    )
                } else {
                    (None, removed_ids)
                }
            }
            OscEvent::StateAudio { value } => {
                if apply_audio_domain_state(&mut s, &value) {
                    (
                        Some((
                            "state:snapshot_ready",
                            serde_json::to_value(&*s).unwrap_or_else(|_| serde_json::json!({})),
                        )),
                        removed_ids,
                    )
                } else {
                    (None, removed_ids)
                }
            }
            OscEvent::StateLayout { value } => {
                if apply_layout_domain_state(&mut s, &value) {
                    (
                        Some(("layouts:update", layout_update_payload(&s))),
                        removed_ids,
                    )
                } else {
                    (None, removed_ids)
                }
            }
            OscEvent::StateSpeakers { value } => {
                if apply_speakers_domain_state(&mut s, &value) {
                    (
                        Some((
                            "state:snapshot_ready",
                            serde_json::to_value(&*s).unwrap_or_else(|_| serde_json::json!({})),
                        )),
                        removed_ids,
                    )
                } else {
                    (None, removed_ids)
                }
            }
            OscEvent::StateInput { value } => {
                if apply_input_domain_state(&mut s, &value) {
                    (
                        Some((
                            "state:snapshot_ready",
                            serde_json::to_value(&*s).unwrap_or_else(|_| serde_json::json!({})),
                        )),
                        removed_ids,
                    )
                } else {
                    (None, removed_ids)
                }
            }
            OscEvent::StateLoudness { value } => {
                if apply_loudness_domain_state(&mut s, &value) {
                    (
                        Some((
                            "state:snapshot_ready",
                            serde_json::to_value(&*s).unwrap_or_else(|_| serde_json::json!({})),
                        )),
                        removed_ids,
                    )
                } else {
                    (None, removed_ids)
                }
            }
            OscEvent::StateMonitoring { value } => {
                if apply_monitoring_domain_state(&mut s, &value) {
                    (
                        Some((
                            "state:snapshot_ready",
                            serde_json::to_value(&*s).unwrap_or_else(|_| serde_json::json!({})),
                        )),
                        removed_ids,
                    )
                } else {
                    (None, removed_ids)
                }
            }
            OscEvent::StateSession { value } => {
                s.producer_session = serde_json::from_str(&value).ok();
                (None, removed_ids)
            }
            OscEvent::StateDebugSpeakerHeatmapMeta { value } => (
                Some((
                    "speaker_heatmap:meta",
                    serde_json::from_str(&value).unwrap_or_else(|_| serde_json::json!({})),
                )),
                removed_ids,
            ),

            OscEvent::StateDebugSpeakerHeatmapSliceXy { value } => (
                Some((
                    "speaker_heatmap:slice_xy",
                    serde_json::from_str(&value).unwrap_or_else(|_| serde_json::json!({})),
                )),
                removed_ids,
            ),

            OscEvent::StateDebugSpeakerHeatmapSliceXz { value } => (
                Some((
                    "speaker_heatmap:slice_xz",
                    serde_json::from_str(&value).unwrap_or_else(|_| serde_json::json!({})),
                )),
                removed_ids,
            ),

            OscEvent::StateDebugSpeakerHeatmapSliceYz { value } => (
                Some((
                    "speaker_heatmap:slice_yz",
                    serde_json::from_str(&value).unwrap_or_else(|_| serde_json::json!({})),
                )),
                removed_ids,
            ),

            OscEvent::StateDebugSpeakerHeatmapVolumeChunk { value } => (
                Some((
                    "speaker_heatmap:volume_chunk",
                    serde_json::from_str(&value).unwrap_or_else(|_| serde_json::json!({})),
                )),
                removed_ids,
            ),

            OscEvent::StateDebugSpeakerHeatmapUnavailable { value } => (
                Some((
                    "speaker_heatmap:unavailable",
                    serde_json::from_str(&value).unwrap_or_else(|_| serde_json::json!({})),
                )),
                removed_ids,
            ),

            OscEvent::StateSnapshotComplete => {
                s.osc_snapshot_ready = true;
                let snapshot = serde_json::to_value(&*s).unwrap_or(serde_json::Value::Null);
                (Some(("state:snapshot_ready", snapshot)), removed_ids)
            }

            OscEvent::StateRealtimeMasterGain { value, .. } => {
                s.master_gain = Some(value);
                (
                    Some(("master:gain", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateRealtimeSpeakerGain { id, value, .. } => {
                s.speaker_gains.insert(id.clone(), value);
                (
                    Some((
                        "speaker:gain",
                        serde_json::json!({ "id": id, "gain": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRealtimeObjectGain { id, value, .. } => {
                s.object_gains.insert(id.clone(), value);
                (
                    Some((
                        "object:gain",
                        serde_json::json!({ "id": id, "gain": value }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateLatency { value } => {
                let rounded = s.set_latency_value(value);
                (
                    Some(("latency", serde_json::json!({ "value": rounded }))),
                    removed_ids,
                )
            }
            OscEvent::StateLatencyInstant { value } => {
                let rounded = s.set_latency_instant_value(value);
                (
                    Some(("latency:instant", serde_json::json!({ "value": rounded }))),
                    removed_ids,
                )
            }
            OscEvent::StateLatencyControl { value } => {
                let rounded = s.set_latency_control_value(value);
                (
                    Some(("latency:control", serde_json::json!({ "value": rounded }))),
                    removed_ids,
                )
            }
            OscEvent::StateLatencySmoothed { value } => {
                let stored = s.set_latency_smoothed_value(value);
                (
                    Some(("latency:smoothed", serde_json::json!({ "value": stored }))),
                    removed_ids,
                )
            }
            OscEvent::StateLatencyDownstream { value } => {
                let rounded = s.set_latency_downstream_value(value);
                (
                    Some(("latency:downstream", serde_json::json!({ "value": rounded }))),
                    removed_ids,
                )
            }
            OscEvent::StateLatencyTarget { value } => {
                let rounded = s.set_latency_target_value(value);
                (
                    Some(("latency:target", serde_json::json!({ "value": rounded }))),
                    removed_ids,
                )
            }
            OscEvent::StateLatencyTargetRequested { value } => {
                let rounded = s.set_latency_requested_value(value);
                (
                    Some(("latency:requested", serde_json::json!({ "value": rounded }))),
                    removed_ids,
                )
            }
            OscEvent::StateLatencyAvailInput { value } => {
                let stored = s.set_latency_avail_input_value(value);
                (
                    Some(("latency:avail_input", serde_json::json!({ "value": stored }))),
                    removed_ids,
                )
            }
            OscEvent::StateLatencyOutputFifo { value } => {
                let stored = s.set_latency_output_fifo_value(value);
                (
                    Some(("latency:output_fifo", serde_json::json!({ "value": stored }))),
                    removed_ids,
                )
            }
            OscEvent::StateLatencyResamplerPending { value } => {
                let stored = s.set_latency_resampler_pending_value(value);
                (
                    Some(("latency:resampler_pending", serde_json::json!({ "value": stored }))),
                    removed_ids,
                )
            }
            OscEvent::StateDiagSchema { value } => {
                // Stash a parsed copy so the snapshot includes a real object
                // for late subscribers. The live event passes the raw JSON
                // string through — JS parses it once on arrival. Wrapping the
                // parsed Value::Object back in a json!{} payload triggered
                // Tauri to emit it as a stringified object on the JS side.
                s.latency.diag_schema = serde_json::from_str(&value).ok();
                (
                    Some(("diag:schema", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateDiagValues { value } => {
                s.latency.diag_values = serde_json::from_str(&value).ok();
                (
                    Some(("diag:values", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateDecodeTimeMs { value } => {
                s.decode_time_ms = Some(value);
                (
                    Some(("decode:time_ms", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateRenderTimeMs { value } => {
                s.render_time_ms = Some(value);
                (
                    Some(("render:time_ms", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateCrossoverTimeMs { value } => {
                s.crossover_time_ms = Some(value);
                (
                    Some(("crossover:time_ms", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateWriteTimeMs { value } => {
                s.write_time_ms = Some(value);
                (
                    Some(("write:time_ms", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateFrameDurationMs { value } => {
                s.frame_duration_ms = Some(value);
                (
                    Some(("frame:duration_ms", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }

            OscEvent::StateResampleRatio { value } => {
                s.resample_ratio = Some(value);
                (
                    Some(("resample_ratio", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateRenderBridgePath { value } => {
                s.render_bridge_path = if value.trim().is_empty() {
                    None
                } else {
                    Some(value.clone())
                };
                (
                    Some(("render:bridge_path", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateInputPipe { value } => {
                s.orender_input_pipe = if value.trim().is_empty() {
                    None
                } else {
                    Some(value.clone())
                };
                (
                    Some(("state:input_pipe", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }

            OscEvent::StateLogLevel { value } => {
                s.log_level = Some(value.clone());
                (
                    Some(("state:log_level", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }

            OscEvent::Log { entry } => (
                Some((
                    "omniphony:log",
                    serde_json::to_value::<LogEntry>(entry).unwrap_or_default(),
                )),
                removed_ids,
            ),

            OscEvent::StateRenderEvaluationCartesianXSize { value } => {
                s.vbap_cartesian.x_size = Some(value);
                (
                    Some((
                        "render_evaluation:cartesian:x_size",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRenderEvaluationCartesianYSize { value } => {
                s.vbap_cartesian.y_size = Some(value);
                (
                    Some((
                        "render_evaluation:cartesian:y_size",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRenderEvaluationCartesianZSize { value } => {
                s.vbap_cartesian.z_size = Some(value);
                (
                    Some((
                        "render_evaluation:cartesian:z_size",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRenderEvaluationCartesianZNegSize { value } => {
                s.vbap_cartesian.z_neg_size = Some(value);
                (
                    Some((
                        "render_evaluation:cartesian:z_neg_size",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRenderEvaluationPolarAzimuthResolution { value } => {
                s.vbap_polar.azimuth_resolution = Some(value);
                (
                    Some((
                        "render_evaluation:polar:azimuth_resolution",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRenderEvaluationPolarElevationResolution { value } => {
                s.vbap_polar.elevation_resolution = Some(value);
                (
                    Some((
                        "render_evaluation:polar:elevation_resolution",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRenderEvaluationPolarDistanceRes { value } => {
                s.vbap_polar.distance_res = Some(value);
                (
                    Some((
                        "render_evaluation:polar:distance_res",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRenderEvaluationPolarDistanceMax { value } => {
                s.vbap_polar.distance_max = Some(value);
                (
                    Some((
                        "render_evaluation:polar:distance_max",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRenderEvaluationPositionInterpolation { enabled } => {
                s.vbap_polar.position_interpolation = Some(enabled);
                (
                    Some((
                        "render_evaluation:position_interpolation",
                        serde_json::json!({ "enabled": enabled }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateVbapAllowNegativeZ { enabled } => {
                s.vbap_allow_negative_z = Some(enabled);
                (
                    Some((
                        "vbap:allow_negative_z",
                        serde_json::json!({ "enabled": enabled }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateSpeakersRecomputing { enabled } => {
                s.vbap_recomputing = Some(enabled);
                (
                    Some((
                        "vbap:recomputing",
                        serde_json::json!({ "enabled": enabled }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateSpeakersRecomputeError { message } => {
                s.recompute_error = if message.is_empty() {
                    None
                } else {
                    Some(message.clone())
                };
                (
                    Some((
                        "speakers:recompute_error",
                        serde_json::json!({ "message": message }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResampling { enabled } => {
                s.adaptive_resampling = Some(if enabled { 1 } else { 0 });
                (
                    Some((
                        "adaptive_resampling",
                        serde_json::json!({ "enabled": if enabled { 1 } else { 0 } }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingEnableFarMode { enabled } => {
                s.adaptive_resampling_enable_far_mode = Some(if enabled { 1 } else { 0 });
                (
                    Some((
                        "adaptive_resampling:enable_far_mode",
                        serde_json::json!({ "enabled": s.adaptive_resampling_enable_far_mode }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingForceSilenceInFarMode { enabled } => {
                s.adaptive_resampling_force_silence_in_far_mode = Some(if enabled { 1 } else { 0 });
                (
                    Some((
                        "adaptive_resampling:force_silence_in_far_mode",
                        serde_json::json!({
                            "enabled": s.adaptive_resampling_force_silence_in_far_mode
                        }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingHardRecoverHighInFarMode { enabled } => {
                s.adaptive_resampling_hard_recover_high_in_far_mode =
                    Some(if enabled { 1 } else { 0 });
                (
                    Some((
                        "adaptive_resampling:hard_recover_high_in_far_mode",
                        serde_json::json!({
                            "enabled": s.adaptive_resampling_hard_recover_high_in_far_mode
                        }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingHardRecoverLowInFarMode { enabled } => {
                s.adaptive_resampling_hard_recover_low_in_far_mode =
                    Some(if enabled { 1 } else { 0 });
                (
                    Some((
                        "adaptive_resampling:hard_recover_low_in_far_mode",
                        serde_json::json!({
                            "enabled": s.adaptive_resampling_hard_recover_low_in_far_mode
                        }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingFarModeReturnFadeInMs { value } => {
                s.adaptive_resampling_far_mode_return_fade_in_ms = Some(value.round() as i64);
                (
                    Some((
                        "adaptive_resampling:far_mode_return_fade_in_ms",
                        serde_json::json!({
                            "value": s.adaptive_resampling_far_mode_return_fade_in_ms
                        }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingKpNear { value } => {
                s.adaptive_resampling_kp_near = Some(value);
                (
                    Some((
                        "adaptive_resampling:kp_near",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingKi { value } => {
                s.adaptive_resampling_ki = Some(value);
                (
                    Some((
                        "adaptive_resampling:ki",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingIntegralDischargeRatio { value } => {
                s.adaptive_resampling_integral_discharge_ratio = Some(value);
                (
                    Some((
                        "adaptive_resampling:integral_discharge_ratio",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingMaxAdjust { value } => {
                s.adaptive_resampling_max_adjust = Some(value);
                (
                    Some((
                        "adaptive_resampling:max_adjust",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingUpdateIntervalCallbacks { value } => {
                s.adaptive_resampling_update_interval_callbacks = Some(value.round() as i64);
                (
                    Some((
                        "adaptive_resampling:update_interval_callbacks",
                        serde_json::json!({ "value": s.adaptive_resampling_update_interval_callbacks }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingHighRecoverEntryMarginMs { value } => {
                s.adaptive_resampling_high_recover_entry_margin_ms = Some(value.round() as i64);
                (
                    Some((
                        "adaptive_resampling:high_recover_entry_margin_ms",
                        serde_json::json!({ "value": s.adaptive_resampling_high_recover_entry_margin_ms }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingBand { value } => {
                s.adaptive_resampling_band = Some(value.clone());
                (
                    Some((
                        "adaptive_resampling:band",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateAdaptiveResamplingState { value } => {
                s.adaptive_resampling_state = Some(value.clone());
                (
                    Some((
                        "adaptive_resampling:state",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateAdaptiveResamplingPaused { enabled } => {
                s.adaptive_resampling_paused = Some(if enabled { 1 } else { 0 });
                (
                    Some((
                        "adaptive_resampling:pause",
                        serde_json::json!({ "enabled": if enabled { 1 } else { 0 } }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateConfigSaved { saved } => {
                s.config_saved = Some(if saved { 1 } else { 0 });
                (
                    Some((
                        "config:saved",
                        serde_json::json!({ "saved": if saved { 1 } else { 0 } }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::StateConfigSaveError { message } => {
                s.save_error = if message.is_empty() {
                    None
                } else {
                    Some(message.clone())
                };
                (
                    Some((
                        "config:save_error",
                        serde_json::json!({ "message": message }),
                    )),
                    removed_ids,
                )
            }
        }
    }; // mutex released here, before any emit

    for id in removed_ids {
        let _ = app.emit("source:remove", serde_json::json!({ "id": id }));
    }

    if let Some((event, payload)) = to_emit {
        let _ = app.emit(event, payload);
    }
}
