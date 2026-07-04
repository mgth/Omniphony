use rosc::{decoder, OscPacket, OscType};
use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::app_state::OutputDeviceOption;
use crate::app_state::{
    AppState, DistanceDiffuse, LiveOptionsState, Meter, RenderBackendState, RoomRatio, SpreadState,
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

// ── local-renderer auto-start watchdog ──────────────────────────────────────
/// Cadence of the watchdog checks inside the OSC thread loop.
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(1);
/// How long the connection must be down before auto-starting (a renderer
/// restart or a brief packet loss must not trigger a spawn).
const WATCHDOG_DISCONNECT_DEBOUNCE: Duration = Duration::from_secs(6);
/// Grace after a goodbye broadcast, giving the exiting renderer time to
/// actually release the OSC port before the probe.
const WATCHDOG_GOODBYE_GRACE: Duration = Duration::from_millis(500);
/// A child exiting within this window after spawn counts as a failure.
const WATCHDOG_FAST_FAIL_WINDOW: Duration = Duration::from_secs(5);
/// Backoff between failed spawn attempts.
const WATCHDOG_COOLDOWN: Duration = Duration::from_secs(5);
/// Failure streak after which the watchdog gives up until re-armed (settings
/// change or manual launch).
const WATCHDOG_MAX_ATTEMPTS: u8 = 3;

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AudioDomainState {
    output_devices: Option<Vec<OutputDeviceOption>>,
    output_device: Option<String>,
    output_device_effective: Option<String>,
    output_backend: Option<String>,
    output_file: Option<String>,
    output_file_format: Option<String>,
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
    object_size_intervals: Option<u32>,
    binaural: Option<serde_json::Value>,
    master_gain: Option<f64>,
    auto_gain: Option<bool>,
    auto_gain_ceiling_db: Option<f64>,
    ramp_mode: Option<String>,
    distance_model: Option<String>,
    distance_model_metric: Option<String>,
    room_ratio: Option<RoomRatio>,
    spread: Option<SpreadState>,
    distance_diffuse: Option<DistanceDiffuse>,
    vbap_cartesian: Option<VbapCartesian>,
    vbap_polar: Option<VbapPolar>,
    render_backend_state: Option<RenderBackendState>,
    /// Declared live options (registry RFC phase 1): the renderer's `options`
    /// block, passed through verbatim — no typed mirror needed per option.
    options: Option<serde_json::Value>,
    /// The live 2D-sources / routing options, collected by key (flatten) and
    /// mirrored into `AppState` verbatim — see [`LiveOptionsState`].
    #[serde(flatten)]
    live_options: LiveOptionsState,
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
    if let Some(output_backend) = parsed.output_backend {
        s.set_audio_output_backend(Some(output_backend));
    }
    if let Some(output_file) = parsed.output_file {
        s.set_audio_output_file(Some(output_file));
    }
    if let Some(output_file_format) = parsed.output_file_format {
        s.set_audio_output_file_format(Some(output_file_format));
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
    if let Some(object_size_intervals) = parsed.object_size_intervals {
        s.object_size_intervals = object_size_intervals;
    }
    if let Some(binaural) = parsed.binaural {
        s.binaural = Some(binaural);
    }
    if let Some(master_gain) = parsed.master_gain {
        s.master_gain = Some(master_gain);
    }
    if let Some(auto_gain) = parsed.auto_gain {
        s.auto_gain = Some(auto_gain);
    }
    if let Some(auto_gain_ceiling_db) = parsed.auto_gain_ceiling_db {
        s.auto_gain_ceiling_db = Some(auto_gain_ceiling_db);
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
    if let Some(options) = parsed.options {
        s.options = Some(options);
    }
    // The renderer domain always carries the full option set, so mirror it
    // wholesale (an explicit `virtualBed: null` must reach the UI — it means
    // "no saved bed", which triggers the one-shot canonical-bed materialise).
    s.live_options = parsed.live_options;
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

/// Inspect a `/heartbeat/ack` payload for the renderer's instance epoch and
/// update the latched value. Returns `true` only when it *changed* from a
/// previously latched epoch — i.e. a different renderer instance now answers on
/// the RX port (a CLI⇄mpv swap) behind an otherwise unbroken connection — so the
/// caller can force a full re-handshake. A first observation (fresh connection)
/// or an ack from an older renderer that carries no epoch is not a change.
fn producer_epoch_changed(state: &Arc<Mutex<AppState>>, args: &[OscType]) -> bool {
    let Some(epoch) = args.iter().find_map(|a| match a {
        OscType::Int(i) => Some(*i),
        _ => None,
    }) else {
        return false;
    };
    let mut s = state.lock().unwrap();
    match s.producer_epoch {
        None => {
            s.producer_epoch = Some(epoch);
            false
        }
        Some(prev) if prev == epoch => false,
        Some(_) => {
            s.producer_epoch = Some(epoch);
            true
        }
    }
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
    // The gain-table transfer arrives as a burst of many UDP datagrams (a large
    // compressed table, P1/P2); enlarge the receive buffer so the kernel doesn't
    // drop most of them before the loop drains. Capped by net.core.rmem_max.
    if let Err(e) = socket2::SockRef::from(&socket).set_recv_buffer_size(4 * 1024 * 1024) {
        log::warn!("[osc] could not enlarge recv buffer: {e}");
    }

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
    let mut last_batch_flush = Instant::now();
    let mut last_watchdog_check = Instant::now();
    let mut disconnected_since: Option<Instant> = Some(Instant::now());

    loop {
        if last_watchdog_check.elapsed() >= WATCHDOG_INTERVAL {
            last_watchdog_check = Instant::now();
            watchdog_tick(
                &app,
                &host,
                osc_rx_port,
                is_connected,
                &mut disconnected_since,
            );
        }

        // Flush the coalesced high-frequency emits at ~60 Hz, independent of how
        // many OSC messages arrived since the last tick.
        if last_batch_flush.elapsed() >= BATCH_FLUSH_INTERVAL {
            flush_emit_batch(&app);
            last_batch_flush = Instant::now();
        }

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
        // Re-request missing gain-table chunks if the transfer has stalled. Cheap
        // to check every loop tick (≤50ms cadence via the recv timeout).
        if let Some((version, missing)) = gaintable_check_nack(Instant::now()) {
            send_gaintable_nack(&socket, &host, osc_rx_port, version, &missing);
        }

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

/// Auto-start watchdog: when nothing answers on a loopback target, launch a
/// standby renderer (`orender render … --osc-yield`). It hands the OSC port
/// to an mpv-embedded renderer on demand and this watchdog brings it back
/// once mpv exits (goodbye broadcast or heartbeat timeout, then a port probe
/// confirming nobody holds the port).
fn watchdog_tick(
    app: &AppHandle,
    host: &str,
    osc_rx_port: u16,
    is_connected: bool,
    disconnected_since: &mut Option<Instant>,
) {
    let shared = app.state::<crate::SharedState>();
    let shared = shared.inner();

    // Reap a tracked child that exited, whatever the connection state, and
    // apply fast-fail backoff. Locks are taken sequentially (never nested) to
    // keep a single lock order with the spawn path and the exit hook.
    let exited = {
        let mut child_guard = shared.renderer_child.lock().unwrap();
        match child_guard.as_mut().map(|child| child.try_wait()) {
            Some(Ok(Some(status))) => {
                *child_guard = None;
                Some(status)
            }
            _ => None,
        }
    };
    if let Some(status) = exited {
        let mut wd = shared.watchdog.lock().unwrap();
        let fast_fail = wd
            .last_spawn_at
            .take()
            .map(|at| at.elapsed() < WATCHDOG_FAST_FAIL_WINDOW)
            .unwrap_or(false);
        if fast_fail {
            wd.attempts += 1;
            wd.cooldown_until = Some(Instant::now() + WATCHDOG_COOLDOWN);
            log::warn!(
                "[watchdog] local renderer exited right after launch ({status}); attempt {}/{}",
                wd.attempts,
                WATCHDOG_MAX_ATTEMPTS
            );
            if wd.attempts >= WATCHDOG_MAX_ATTEMPTS {
                log::error!(
                    "[watchdog] giving up on auto-start until settings change or a manual launch"
                );
                let _ = app.emit(
                    "orender:autostart",
                    serde_json::json!({ "status": "failed" }),
                );
            }
        } else {
            // Normal lifecycle exit: yielded to mpv, or a manual/Studio quit.
            log::info!("[watchdog] local renderer exited ({status})");
            wd.attempts = 0;
        }
    }

    if is_connected {
        *disconnected_since = None;
        shared.watchdog.lock().unwrap().check_requested_at = None;
        return;
    }
    let since = *disconnected_since.get_or_insert_with(Instant::now);

    let goodbye_ready = shared
        .watchdog
        .lock()
        .unwrap()
        .check_requested_at
        .map(|at| at.elapsed() >= WATCHDOG_GOODBYE_GRACE)
        .unwrap_or(false);
    if !goodbye_ready && since.elapsed() < WATCHDOG_DISCONNECT_DEBOUNCE {
        return;
    }

    // Re-read the settings at check time so panel edits apply immediately.
    let cfg = crate::config::load_config(&shared.config_dir);
    if !cfg.auto_start_renderer || !crate::commands::app::host_is_local(host) {
        return;
    }
    {
        let wd = shared.watchdog.lock().unwrap();
        // A manual Stop keeps the renderer stopped until the user explicitly
        // acts again (a manual launch or a settings save, both of which re-arm).
        if wd.suppressed {
            return;
        }
        if wd.attempts >= WATCHDOG_MAX_ATTEMPTS {
            return;
        }
        if wd
            .cooldown_until
            .is_some_and(|until| Instant::now() < until)
        {
            return;
        }
    }
    // A live child we already spawned is still starting up — give it time.
    if shared
        .renderer_child
        .lock()
        .unwrap()
        .as_mut()
        .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
    {
        return;
    }
    // A service-managed renderer is someone else's responsibility.
    if crate::commands::orender::orender_service_running() {
        return;
    }
    // Port probe (wildcard, matching how renderers bind): if anything holds
    // the OSC RX port — e.g. an mpv-embedded renderer we temporarily lost
    // contact with — do not spawn.
    match UdpSocket::bind(("0.0.0.0", osc_rx_port)) {
        Ok(probe) => drop(probe),
        Err(_) => {
            shared.watchdog.lock().unwrap().check_requested_at = None;
            return;
        }
    }

    log::info!("[watchdog] no renderer on local port {osc_rx_port}; launching a standby instance");
    match crate::commands::orender::autostart_orender(app, shared) {
        Ok(info) => {
            log::info!("[watchdog] standby renderer launched: {}", info["command"]);
            let _ = app.emit(
                "orender:autostart",
                serde_json::json!({ "status": "launched" }),
            );
        }
        Err(e) => {
            let mut wd = shared.watchdog.lock().unwrap();
            wd.attempts += 1;
            wd.cooldown_until = Some(Instant::now() + WATCHDOG_COOLDOWN);
            log::error!("[watchdog] auto-start failed: {e}");
            if wd.attempts >= WATCHDOG_MAX_ATTEMPTS {
                let _ = app.emit(
                    "orender:autostart",
                    serde_json::json!({ "status": "failed", "error": e }),
                );
            }
        }
    }
}

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
            // Goodbye broadcast: the renderer is shutting down gracefully
            // (quit, yield to mpv, or mpv exiting). Flip to reconnecting at
            // once and ask the watchdog to check (after a short grace for the
            // port to actually close) instead of waiting out the ack timeout.
            if msg.addr == "/omniphony/state/shutdown" {
                log::info!("[osc] renderer announced shutdown");
                *is_connected = false;
                emit_osc_status(app, state, "reconnecting");
                let shared = app.state::<crate::SharedState>();
                shared.watchdog.lock().unwrap().check_requested_at = Some(Instant::now());
                return;
            }

            match is_heartbeat_address(&msg.addr) {
                HeartbeatResponse::Ack => {
                    *last_ack_at = Instant::now();
                    if producer_epoch_changed(state, &msg.args) {
                        // A different renderer instance now answers on this port
                        // (a CLI⇄mpv swap behind an unbroken link). Re-handshake
                        // so capabilities, the object snapshot (names) and the
                        // metering subscription are refreshed for the new producer.
                        send_register(socket, host, osc_rx_port, listen_port);
                        send_metering_enabled(socket, host, osc_rx_port, metering_enabled);
                        *is_connected = false;
                        emit_osc_status(app, state, "reconnecting");
                    } else if !*is_connected {
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
                                if producer_epoch_changed(state, &msg.args) {
                                    // Producer swap behind an unbroken link → re-handshake.
                                    send_register(socket, host, osc_rx_port, listen_port);
                                    send_metering_enabled(
                                        socket, host, osc_rx_port, metering_enabled,
                                    );
                                    *is_connected = false;
                                    emit_osc_status(app, state, "reconnecting");
                                } else if !*is_connected {
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

// ── speaker gain table reassembly (chunked, compressed artifact over OSC) ──────
// Chunks arrive as `[version u32 LE][chunk_index u32 LE][artifact bytes]`. We
// reassemble per version, then inflate + decode the evaluation-artifact byte
// format (mirror of renderer's evaluation_artifact: MAGIC "OEVL" + version +
// metadata_len + payload_len + metadata JSON + zlib payload of f32 positions and
// gains) and emit the whole table to JS once. Kept in a module static — the OSC
// receive path is the only writer — to avoid threading a buffer through AppState.
struct GainTableAsm {
    version: u32,
    chunk_count: usize,
    chunks: std::collections::BTreeMap<u32, Vec<u8>>,
    /// Last time a chunk (or the meta) arrived; drives the stall → NACK timer.
    last_activity: Option<Instant>,
    nack_rounds: u8,
}

static GAINTABLE: Mutex<GainTableAsm> = Mutex::new(GainTableAsm {
    version: 0,
    chunk_count: 0,
    chunks: std::collections::BTreeMap::new(),
    last_activity: None,
    nack_rounds: 0,
});

// Reliability for the chunked UDP transfer: if the burst stalls (lost datagrams),
// re-request just the missing chunk indices. The receive buffer already absorbs the
// burst itself; this recovers real network loss for the remote (Studio ≠ renderer
// host) case. The renderer's `/nack` handler resends from a deterministic rebuild.
const GAINTABLE_NACK_TIMEOUT: Duration = Duration::from_millis(120);
const GAINTABLE_MAX_NACK_ROUNDS: u8 = 12;
// Cap indices per NACK datagram to stay under a typical MTU.
const GAINTABLE_NACK_MAX_INDICES: usize = 256;

fn gaintable_on_meta(json: &str) {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let version = v.get("version").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    let chunk_count = v.get("chunk_count").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
    if let Ok(mut g) = GAINTABLE.lock() {
        g.version = version;
        g.chunk_count = chunk_count;
        g.chunks.clear();
        g.last_activity = Some(Instant::now());
        g.nack_rounds = 0;
    }
}

/// If the current transfer has stalled, return `(version, missing_indices)` to
/// re-request (and arm the next round); give up after `GAINTABLE_MAX_NACK_ROUNDS`.
fn gaintable_check_nack(now: Instant) -> Option<(u32, Vec<u32>)> {
    let mut g = GAINTABLE.lock().ok()?;
    if g.chunk_count == 0 {
        return None; // no active transfer (idle or just completed)
    }
    let last = g.last_activity?;
    if now.duration_since(last) < GAINTABLE_NACK_TIMEOUT {
        return None;
    }
    if g.nack_rounds >= GAINTABLE_MAX_NACK_ROUNDS {
        log::warn!(
            "[osc] gaintable transfer abandoned: {}/{} chunks after {} NACK rounds",
            g.chunks.len(),
            g.chunk_count,
            g.nack_rounds
        );
        g.chunk_count = 0;
        g.chunks.clear();
        return None;
    }
    let total = g.chunk_count as u32;
    let missing: Vec<u32> = (0..total).filter(|i| !g.chunks.contains_key(i)).collect();
    if missing.is_empty() {
        return None;
    }
    g.nack_rounds += 1;
    g.last_activity = Some(now);
    Some((g.version, missing))
}

fn send_gaintable_nack(
    socket: &UdpSocket,
    host: &str,
    rx_port: u16,
    version: u32,
    missing: &[u32],
) {
    use rosc::{encoder, OscMessage};
    for group in missing.chunks(GAINTABLE_NACK_MAX_INDICES) {
        let mut args = Vec::with_capacity(group.len() + 1);
        args.push(OscType::Int(version as i32));
        args.extend(group.iter().map(|&i| OscType::Int(i as i32)));
        let msg = OscPacket::Message(OscMessage {
            addr: "/omniphony/control/debug/speaker_gaintable/nack".to_string(),
            args,
        });
        if let Ok(bytes) = encoder::encode(&msg) {
            let _ = socket.send_to(&bytes, format!("{host}:{rx_port}"));
        }
    }
}

fn gaintable_on_chunk(bytes: &[u8]) -> Option<serde_json::Value> {
    if bytes.len() < 8 {
        return None;
    }
    let version = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
    let index = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let artifact = {
        let mut g = GAINTABLE.lock().ok()?;
        if g.chunk_count == 0 || version != g.version {
            return None; // stale/foreign chunk (meta not seen, or newer in flight)
        }
        g.chunks.insert(index, bytes[8..].to_vec());
        g.last_activity = Some(Instant::now());
        if g.chunks.len() != g.chunk_count {
            return None;
        }
        let mut artifact = Vec::new();
        for c in g.chunks.values() {
            artifact.extend_from_slice(c);
        }
        g.chunks.clear();
        g.chunk_count = 0;
        artifact
    };
    decode_evaluation_artifact(&artifact, version)
}

/// Standard base64 encode (no line breaks). Used to hand the raw f32 payload to
/// the UI as a compact string instead of a giant JSON array of numbers — the JS
/// side rebuilds Float32Array views directly, avoiding per-number text parsing.
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Decode the band-aware gain table ("OBGT") for one speaker. Inflates the payload
/// and hands it to the UI as base64 (`dataB64`) + metadata — no float parsing here;
/// the JS side slices Float32Array views (positions, then per-band gains).
fn decode_band_gaintable(bytes: &[u8], version: u32) -> Option<serde_json::Value> {
    if bytes.len() < 16 || &bytes[0..4] != b"OBGT" {
        return None;
    }
    let meta_len = u32::from_le_bytes(bytes[8..12].try_into().ok()?) as usize;
    let payload_len = u32::from_le_bytes(bytes[12..16].try_into().ok()?) as usize;
    let meta_end = 16 + meta_len;
    let payload_end = meta_end + payload_len;
    if bytes.len() < payload_end {
        return None;
    }
    let metadata: serde_json::Value = serde_json::from_slice(&bytes[16..meta_end]).ok()?;

    let mut raw = Vec::new();
    {
        use std::io::Read as _;
        let mut dec = flate2::read::ZlibDecoder::new(&bytes[meta_end..payload_end]);
        dec.read_to_end(&mut raw).ok()?;
    }

    let dim = |k: &str| metadata.get(k).and_then(|x| x.as_u64()).map(|x| x as usize);
    let nx = dim("x_count")?;
    let ny = dim("y_count")?;
    let nz = dim("z_count")?;
    let nb = dim("band_count")?;
    let speaker = metadata
        .get("speaker_index")
        .and_then(|x| x.as_u64())
        .map(|x| x as usize)
        .unwrap_or(0);

    // Band frequency edges → {lowHz, highHz|null}.
    let bands: Vec<serde_json::Value> = metadata
        .get("bands")
        .and_then(|b| b.as_array())
        .map(|arr| {
            arr.iter()
                .map(|b| {
                    serde_json::json!({
                        "lowHz": b.get("low_hz").and_then(|x| x.as_f64()).unwrap_or(0.0),
                        "highHz": b.get("high_hz").and_then(|x| x.as_f64()),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Some(serde_json::json!({
        "version": version,
        "domain": "cartesian_bands",
        "speakerIndex": speaker,
        "xCount": nx, "yCount": ny, "zCount": nz,
        "bandCount": nb,
        "bands": bands,
        "dataB64": base64_encode(&raw),
    }))
}

fn decode_evaluation_artifact(bytes: &[u8], version: u32) -> Option<serde_json::Value> {
    if bytes.len() < 16 {
        return None;
    }
    if &bytes[0..4] == b"OBGT" {
        return decode_band_gaintable(bytes, version);
    }
    if &bytes[0..4] != b"OEVL" {
        return None;
    }
    let metadata_len = u32::from_le_bytes(bytes[8..12].try_into().ok()?) as usize;
    let payload_len = u32::from_le_bytes(bytes[12..16].try_into().ok()?) as usize;
    let meta_end = 16 + metadata_len;
    let payload_end = meta_end + payload_len;
    if bytes.len() < payload_end {
        return None;
    }
    let metadata: serde_json::Value = serde_json::from_slice(&bytes[16..meta_end]).ok()?;

    let mut raw = Vec::new();
    {
        use std::io::Read as _;
        let mut dec = flate2::read::ZlibDecoder::new(&bytes[meta_end..payload_end]);
        dec.read_to_end(&mut raw).ok()?;
    }

    let domain = metadata.get("domain")?;
    let kind = domain.get("kind")?.as_str()?;
    let dim = |k: &str| domain.get(k).and_then(|x| x.as_u64()).map(|x| x as usize);
    let mut off = 0usize;
    let mut read_f32 = |count: usize| -> Option<Vec<f32>> {
        let end = off + count * 4;
        if end > raw.len() {
            return None;
        }
        let mut v = Vec::with_capacity(count);
        for i in 0..count {
            let b = off + i * 4;
            v.push(f32::from_le_bytes(raw[b..b + 4].try_into().ok()?));
        }
        off = end;
        Some(v)
    };

    match kind {
        "cartesian" => {
            let (xc, yc, zc, sc) = (
                dim("x_count")?,
                dim("y_count")?,
                dim("z_count")?,
                dim("speaker_count")?,
            );
            let xs = read_f32(xc)?;
            let ys = read_f32(yc)?;
            let zs = read_f32(zc)?;
            let gains = read_f32(xc * yc * zc * sc)?;
            Some(serde_json::json!({
                "version": version, "domain": "cartesian", "speakerCount": sc,
                "xCount": xc, "yCount": yc, "zCount": zc,
                "xPositions": xs, "yPositions": ys, "zPositions": zs, "gains": gains,
            }))
        }
        "polar" => {
            let (ac, ec, dc, sc) = (
                dim("azimuth_count")?,
                dim("elevation_count")?,
                dim("distance_count")?,
                dim("speaker_count")?,
            );
            let az = read_f32(ac)?;
            let el = read_f32(ec)?;
            let di = read_f32(dc)?;
            let gains = read_f32(ac * ec * dc * sc)?;
            Some(serde_json::json!({
                "version": version, "domain": "polar", "speakerCount": sc,
                "azimuthCount": ac, "elevationCount": ec, "distanceCount": dc,
                "azimuthPositions": az, "elevationPositions": el, "distancePositions": di, "gains": gains,
            }))
        }
        _ => None,
    }
}

// ── High-frequency emit coalescing ────────────────────────────────────────
//
// During playback the renderer pushes one OSC message per object/speaker per
// frame (positions + meters), and each was turned into its own `app.emit`.
// That global broadcast is serialised to JSON and posted over the WebView IPC;
// at thousands of messages/s it grows WebView2 native memory without bound on
// Windows (Linux/WebKitGTK absorbs it). We coalesce these into a single
// `state:batch` event flushed at ~60 Hz: only the latest payload per (event,id)
// in the window survives, collapsing N×(objects+speakers) emits/frame into one.
// Low-frequency events (config/state/layout changes) keep emitting immediately.
const BATCH_FLUSH_INTERVAL: Duration = Duration::from_millis(16);

const BATCHED_EVENTS: &[&str] = &[
    "binaural:head_pose",
    "source:update",
    "source:meter",
    "source:gains",
    "source:band_gains",
    "speaker:meter",
    "master:meter",
    "meter:drc_gain",
];

thread_local! {
    // (event, latest payload) keyed by a dedup string. Lives on the OSC thread,
    // which is the only thread that queues/flushes it.
    static EMIT_BATCH: std::cell::RefCell<
        std::collections::HashMap<String, (&'static str, serde_json::Value)>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
}

fn is_batched_event(event: &str) -> bool {
    BATCHED_EVENTS.contains(&event)
}

fn batch_dedup_key(event: &str, payload: &serde_json::Value) -> String {
    let id = payload.get("id").map(|v| v.to_string()).unwrap_or_default();
    if event == "source:band_gains" {
        let band = payload
            .get("band")
            .map(|v| v.to_string())
            .unwrap_or_default();
        format!("{event}|{id}|{band}")
    } else {
        format!("{event}|{id}")
    }
}

fn queue_batched_emit(event: &'static str, payload: serde_json::Value) {
    EMIT_BATCH.with(|b| {
        b.borrow_mut()
            .insert(batch_dedup_key(event, &payload), (event, payload));
    });
}

fn flush_emit_batch(app: &AppHandle) {
    let drained: Vec<(&'static str, serde_json::Value)> = EMIT_BATCH.with(|b| {
        let mut map = b.borrow_mut();
        if map.is_empty() {
            Vec::new()
        } else {
            map.drain().map(|(_, v)| v).collect()
        }
    });
    if drained.is_empty() {
        return;
    }
    let events: Vec<serde_json::Value> = drained
        .into_iter()
        .map(|(event, payload)| serde_json::json!({ "event": event, "payload": payload }))
        .collect();
    let _ = app.emit("state:batch", serde_json::json!({ "events": events }));
}

fn handle_event(ev: OscEvent, app: &AppHandle, state: &Arc<Mutex<AppState>>) {
    // Per-object mutes preserved across a seek/reset so they can be re-emitted to
    // the frontend after the `source:remove` wipe (see the SpatialFrame arm).
    let mut restore_object_mutes: Vec<String> = Vec::new();
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

                // On a seek/reset the renderer keeps each object's mute keyed by
                // slot index (audio stays correctly soloed), but every source is
                // dropped here and re-emitted as `source:remove`, which makes the
                // frontend forget `objectMuted`. Preserve the authoritative mute
                // mirror and schedule a re-emit so the visual solo/mute state is
                // restored immediately instead of drifting until the next snapshot
                // heartbeat. Non-reset stale removals (object count shrank) genuinely
                // drop the slot, so they still clear the mute.
                if is_reset {
                    restore_object_mutes = s
                        .object_mutes
                        .iter()
                        .filter_map(|(id, &m)| (m != 0).then(|| id.clone()))
                        .collect();
                }
                for id in &stale_ids {
                    s.sources.remove(id);
                    s.source_levels.remove(id);
                    s.object_speaker_gains.remove(id);
                    s.object_band_gains.remove(id);
                    if !is_reset {
                        s.object_mutes.remove(id);
                    }
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

            OscEvent::UpdateSize {
                id,
                size,
                generation,
            } => {
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
                s.object_band_gains.remove(&id);
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

            OscEvent::MeterMaster {
                peak_dbfs,
                rms_dbfs,
            } => {
                s.master_level = Some(Meter {
                    peak_dbfs,
                    rms_dbfs,
                });
                (
                    Some((
                        "master:meter",
                        serde_json::json!({
                            "meter": { "peakDbfs": peak_dbfs, "rmsDbfs": rms_dbfs }
                        }),
                    )),
                    removed_ids,
                )
            }

            OscEvent::MeterDrcGain { value } => (
                Some(("meter:drc_gain", serde_json::json!({ "value": value }))),
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
            OscEvent::StateClip { speaker } => (
                Some(("clip:detected", serde_json::json!({ "speaker": speaker }))),
                removed_ids,
            ),
            OscEvent::StateHeadPose { w, x, y, z } => (
                // Fast path for the 3D head: no AppState mutation, just a
                // coalesced event straight to the webview (latest pose wins).
                Some((
                    "binaural:head_pose",
                    serde_json::json!({ "w": w, "x": x, "y": y, "z": z }),
                )),
                removed_ids,
            ),
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
            OscEvent::StateDebugSpeakerGaintableMeta { value } => {
                gaintable_on_meta(&value);
                (None, removed_ids)
            }

            OscEvent::StateDebugSpeakerGaintableChunk { bytes } => {
                // Reassemble in Rust; only emit the decoded table once complete.
                (
                    gaintable_on_chunk(&bytes).map(|v| ("speaker_gaintable", v)),
                    removed_ids,
                )
            }

            OscEvent::StateDebugSpeakerGaintableUnavailable { value } => (
                Some((
                    "speaker_gaintable:unavailable",
                    serde_json::from_str(&value).unwrap_or_else(|_| serde_json::json!({})),
                )),
                removed_ids,
            ),

            OscEvent::StateDebugSpeakerGaintableUptodate { version } => (
                Some((
                    "speaker_gaintable:uptodate",
                    serde_json::json!({ "version": version }),
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
                    Some((
                        "latency:downstream",
                        serde_json::json!({ "value": rounded }),
                    )),
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
                    Some((
                        "latency:avail_input",
                        serde_json::json!({ "value": stored }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateLatencyOutputFifo { value } => {
                let stored = s.set_latency_output_fifo_value(value);
                (
                    Some((
                        "latency:output_fifo",
                        serde_json::json!({ "value": stored }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateLatencyResamplerPending { value } => {
                let stored = s.set_latency_resampler_pending_value(value);
                (
                    Some((
                        "latency:resampler_pending",
                        serde_json::json!({ "value": stored }),
                    )),
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
            OscEvent::StateObjectGenerators { value } => {
                // Declared bed→height generator schema (id/label/param specs). JS
                // parses the JSON once on arrival to build the selector + sliders.
                (
                    Some((
                        "objectGenerators:schema",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StatePhantom { value } => {
                // Declared phantom-extraction param schema. JS builds the sliders.
                (
                    Some(("phantom:schema", serde_json::json!({ "value": value }))),
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
            OscEvent::StateRenderConfigPath { value } => {
                s.render_config_path = if value.trim().is_empty() {
                    None
                } else {
                    Some(value.clone())
                };
                (
                    Some(("render:config_path", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateRenderConfigStatus { value } => {
                s.render_config_status = if value.trim().is_empty() {
                    None
                } else {
                    Some(value.clone())
                };
                (
                    Some((
                        "render:config_status",
                        serde_json::json!({ "value": value }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateRenderVersion { value } => {
                s.render_version = if value.trim().is_empty() {
                    None
                } else {
                    Some(value.clone())
                };
                (
                    Some(("render:version", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateRenderAbi { value } => {
                s.render_abi = if value.trim().is_empty() {
                    None
                } else {
                    Some(value.clone())
                };
                (
                    Some(("render:abi", serde_json::json!({ "value": value }))),
                    removed_ids,
                )
            }
            OscEvent::StateRenderBridgeError { value } => {
                s.render_bridge_error = if value.trim().is_empty() {
                    None
                } else {
                    Some(value.clone())
                };
                (
                    Some(("render:bridge_error", serde_json::json!({ "value": value }))),
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
            OscEvent::StateBackendFileContent {
                backend,
                key,
                name,
                content,
            } => (
                Some((
                    "backend-file:content",
                    serde_json::json!({ "backend": backend, "key": key, "name": name, "content": content }),
                )),
                removed_ids,
            ),
            OscEvent::StateBackendFileList { backend, json } => {
                let names = serde_json::from_str::<serde_json::Value>(&json)
                    .unwrap_or_else(|_| serde_json::Value::Array(Vec::new()));
                (
                    Some((
                        "backend-file:list",
                        serde_json::json!({ "backend": backend, "names": names }),
                    )),
                    removed_ids,
                )
            }
            OscEvent::StateBackendFileError {
                backend,
                key,
                message,
            } => (
                Some((
                    "backend-file:error",
                    serde_json::json!({ "backend": backend, "key": key, "message": message }),
                )),
                removed_ids,
            ),
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

    // Re-emit the mutes preserved across a reset after the `source:remove` wipe so
    // the frontend rebuilds `objectMuted` before the spatial:frame recreates the
    // objects — keeping the visual solo/mute state in sync with the audio.
    for id in restore_object_mutes {
        let _ = app.emit("object:mute", serde_json::json!({ "id": id, "muted": 1 }));
    }

    if let Some((event, payload)) = to_emit {
        if is_batched_event(event) {
            queue_batched_emit(event, payload);
        } else {
            let _ = app.emit(event, payload);
        }
    }
}
