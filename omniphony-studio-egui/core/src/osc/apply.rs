#![allow(dead_code)]
//! Domain-state appliers and the chunked gain-table reassembly, copied from the
//! Tauri host (`src-tauri/src/osc_listener.rs`). These are pure functions over
//! `AppState` and the OSC socket; keep them in sync with the host.

use std::net::UdpSocket;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rosc::{OscPacket, OscType};

use crate::model::app_state::*;
use crate::model::layouts::{Layout, Speaker};
use omniphony_geometry::f64 as geometry;

// ── domain-state payloads (`/omniphony/state/*` JSON documents) ──────────────
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
    /// The live fixed-channel-source / routing options, collected by key (flatten) and
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
struct ProfilesDomainState {
    active: Option<String>,
    names: Option<Vec<String>>,
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

// Conversions come from `omniphony-geometry`, shared with the renderer. The
// copies that lived here read the ADM coordinates the renderer publishes as if
// they were Three.js scene coordinates — the same missing axis swizzle as
// `layouts.rs`, but on the LIVE layout rather than a file.

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
            geometry::to_spherical(x, y, z);
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
    // hydrate_from_spherical already clamps to the normalised cube.
    let (x, y, z) = geometry::hydrate_from_spherical(azimuth, elevation, distance_m);
    Speaker {
        id,
        x,
        y,
        z,
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

pub fn layout_update_payload(s: &AppState) -> serde_json::Value {
    serde_json::json!({
        "layouts": s.layouts,
        "selectedLayoutKey": s.selected_layout_key
    })
}

pub fn apply_layout_domain_state(s: &mut AppState, value: &str) -> bool {
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

pub fn apply_speakers_domain_state(s: &mut AppState, value: &str) -> bool {
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

pub fn apply_audio_domain_state(s: &mut AppState, value: &str) -> bool {
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

pub fn apply_input_domain_state(s: &mut AppState, value: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<InputDomainState>(value) else {
        return false;
    };
    // Canonicalise before the state is stored, so what the frontend receives
    // never carries a protocol alias. An unrecognised mode is dropped rather
    // than stored: a mode nothing can act on is worse than the last good one.
    if let Some(mode) = parsed.mode {
        if let Some(normalized) = normalize_input_mode(&mode) {
            s.input_mode = Some(normalized.to_string());
        }
    }
    if let Some(active_mode) = parsed.active_mode {
        if let Some(normalized) = normalize_input_mode(&active_mode) {
            s.input_active_mode = Some(normalized.to_string());
        }
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

pub fn apply_renderer_domain_state(s: &mut AppState, value: &str) -> bool {
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
    if let Some(mut render_backend_state) = parsed.render_backend_state {
        // Validate before storing, so the snapshot the frontend receives is
        // already correct rather than merely reported.
        render_backend_state.sanitize();
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

pub fn apply_loudness_domain_state(s: &mut AppState, value: &str) -> bool {
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

pub fn apply_profiles_domain_state(s: &mut AppState, value: &str) -> bool {
    let Ok(parsed) = serde_json::from_str::<ProfilesDomainState>(value) else {
        return false;
    };
    if let Some(active) = parsed.active {
        s.active_profile = Some(active);
    }
    if let Some(names) = parsed.names {
        s.profile_names = names;
    }
    true
}

pub fn apply_monitoring_domain_state(s: &mut AppState, value: &str) -> bool {
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

// ── speaker gain table reassembly (chunked, compressed artifact over OSC) ──────
// Chunks arrive as `[version u32 LE][chunk_index u32 LE][artifact bytes]`. We
// reassemble per version, then inflate + decode the evaluation-artifact byte
// format (mirror of renderer's evaluation_artifact: MAGIC "OEVL" + version +
// metadata_len + payload_len + metadata JSON + zlib payload of f32 positions and
// gains) and emit the whole table to JS once. Kept in a module static — the OSC
// receive path is the only writer — to avoid threading a buffer through AppState.
/// One in-flight chunked transfer.
struct GainTableAsm {
    chunk_count: usize,
    chunks: std::collections::BTreeMap<u32, Vec<u8>>,
    /// Last time a chunk (or the meta) arrived; drives the stall → NACK timer.
    last_activity: Option<Instant>,
    nack_rounds: u8,
    /// Arrival order, used to evict the oldest when over capacity.
    started: Instant,
}

/// In-flight transfers, keyed by payload version.
///
/// A **map**, not a single slot: the renderer pushes one field per subscribed
/// target, and rapid topology changes queue several transfers back to back. A
/// single slot meant an arriving transfer wiped the one still streaming, whose
/// remaining chunks were then dropped as foreign — the client never completed
/// it and never asked again, so a display could sit on a stale field
/// indefinitely. Keyed by version, concurrent transfers simply coexist.
static GAINTABLE: Mutex<std::collections::BTreeMap<u32, GainTableAsm>> =
    Mutex::new(std::collections::BTreeMap::new());

/// Most transfers reassembled at once. Beyond this the oldest is dropped: it is
/// either finished or hopeless, and an unbounded map is a memory leak on a
/// client that keeps missing chunks.
const GAINTABLE_MAX_INFLIGHT: usize = 6;

// Reliability for the chunked UDP transfer: if the burst stalls (lost datagrams),
// re-request just the missing chunk indices. The receive buffer already absorbs the
// burst itself; this recovers real network loss for the remote (Studio ≠ renderer
// host) case. The renderer's `/nack` handler resends from a deterministic rebuild.
const GAINTABLE_NACK_TIMEOUT: Duration = Duration::from_millis(120);
const GAINTABLE_MAX_NACK_ROUNDS: u8 = 12;
// Cap indices per NACK datagram to stay under a typical MTU.
const GAINTABLE_NACK_MAX_INDICES: usize = 256;

pub fn gaintable_on_meta(json: &str) {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let version = v.get("version").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
    let chunk_count = v.get("chunk_count").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
    if chunk_count == 0 {
        return;
    }
    let now = Instant::now();
    if let Ok(mut map) = GAINTABLE.lock() {
        // A repeated meta for a version already in flight restarts *that*
        // transfer only, leaving the others untouched.
        map.insert(
            version,
            GainTableAsm {
                chunk_count,
                chunks: std::collections::BTreeMap::new(),
                last_activity: Some(now),
                nack_rounds: 0,
                started: now,
            },
        );
        while map.len() > GAINTABLE_MAX_INFLIGHT {
            if let Some(oldest) = map
                .iter()
                .min_by_key(|(_, asm)| asm.started)
                .map(|(version, _)| *version)
            {
                log::warn!("[osc] gaintable: dropping oldest in-flight transfer {oldest}");
                map.remove(&oldest);
            } else {
                break;
            }
        }
    }
}

/// Every stalled transfer's `(version, missing_indices)` to re-request (arming
/// their next round); a transfer is abandoned after `GAINTABLE_MAX_NACK_ROUNDS`.
///
/// All of them, not just one: with several transfers in flight, recovering only
/// the first would leave the others to rot — the silent-staleness failure this
/// map exists to prevent.
pub fn gaintable_check_nack(now: Instant) -> Vec<(u32, Vec<u32>)> {
    let Ok(mut map) = GAINTABLE.lock() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut abandoned = Vec::new();
    for (version, asm) in map.iter_mut() {
        let Some(last) = asm.last_activity else {
            continue;
        };
        if now.duration_since(last) < GAINTABLE_NACK_TIMEOUT {
            continue;
        }
        if asm.nack_rounds >= GAINTABLE_MAX_NACK_ROUNDS {
            log::warn!(
                "[osc] gaintable transfer {version} abandoned: {}/{} chunks after {} NACK rounds",
                asm.chunks.len(),
                asm.chunk_count,
                asm.nack_rounds
            );
            abandoned.push(*version);
            continue;
        }
        let missing: Vec<u32> = (0..asm.chunk_count as u32)
            .filter(|i| !asm.chunks.contains_key(i))
            .collect();
        if missing.is_empty() {
            continue;
        }
        asm.nack_rounds += 1;
        asm.last_activity = Some(now);
        out.push((*version, missing));
    }
    for version in abandoned {
        map.remove(&version);
    }
    out
}

pub fn send_gaintable_nack(
    socket: &UdpSocket,
    host: &str,
    rx_port: u16,
    version: u32,
    missing: &[u32],
) {
    use rosc::{OscMessage, encoder};
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

/// Copied from the host's `commands/input.rs`.
pub fn normalize_input_mode(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "bridge" | "pipe_bridge" => Some("pipe_bridge"),
        "live" | "pipewire" | "pipewire_bridge" => Some("pipewire"),
        _ => None,
    }
}

/// One crossover band of a band-aware gain table.
#[derive(Debug, Clone)]
pub struct BandEdge {
    pub low_hz: f64,
    pub high_hz: Option<f64>,
}

/// A decoded gain-table artifact. The host hands these to the webview as JSON
/// (`speaker_gaintable` event); here they stay typed.
#[derive(Debug, Clone)]
pub enum GainTable {
    /// "OBGT": one speaker's band-aware cartesian table (or the all-speaker
    /// energy field when `speaker_index == -1`). `data` is the inflated f32
    /// payload: grid positions first, then per-band gains, in the order the
    /// webview's `Float32Array` views slice it.
    CartesianBands {
        version: u32,
        speaker_index: i64,
        x_count: usize,
        y_count: usize,
        z_count: usize,
        band_count: usize,
        bands: Vec<BandEdge>,
        data: Vec<f32>,
    },
    /// "OEVL" cartesian domain: per-speaker gains over an x/y/z grid.
    Cartesian {
        version: u32,
        speaker_count: usize,
        x_positions: Vec<f32>,
        y_positions: Vec<f32>,
        z_positions: Vec<f32>,
        /// `x_count * y_count * z_count * speaker_count`.
        gains: Vec<f32>,
    },
    /// "OEVL" polar domain.
    Polar {
        version: u32,
        speaker_count: usize,
        azimuth_positions: Vec<f32>,
        elevation_positions: Vec<f32>,
        distance_positions: Vec<f32>,
        gains: Vec<f32>,
    },
}

impl GainTable {
    /// Speaker index this table describes; whole-layout tables report `-1`.
    pub fn speaker_index(&self) -> i64 {
        match self {
            GainTable::CartesianBands { speaker_index, .. } => *speaker_index,
            GainTable::Cartesian { .. } | GainTable::Polar { .. } => -1,
        }
    }

    pub fn version(&self) -> u32 {
        match self {
            GainTable::CartesianBands { version, .. }
            | GainTable::Cartesian { version, .. }
            | GainTable::Polar { version, .. } => *version,
        }
    }
}

pub fn gaintable_on_chunk(bytes: &[u8]) -> Option<GainTable> {
    if bytes.len() < 8 {
        return None;
    }
    let version = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
    let index = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let artifact = {
        let mut map = GAINTABLE.lock().ok()?;
        // Route the chunk to ITS transfer: another one being in flight is no
        // longer a reason to drop it.
        let asm = map.get_mut(&version)?;
        asm.chunks.insert(index, bytes[8..].to_vec());
        asm.last_activity = Some(Instant::now());
        if asm.chunks.len() != asm.chunk_count {
            return None;
        }
        let mut artifact = Vec::new();
        for c in asm.chunks.values() {
            artifact.extend_from_slice(c);
        }
        map.remove(&version);
        artifact
    };
    decode_evaluation_artifact(&artifact, version)
}

fn inflate(bytes: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read as _;
    let mut raw = Vec::new();
    flate2::read::ZlibDecoder::new(bytes)
        .read_to_end(&mut raw)
        .ok()?;
    Some(raw)
}

fn f32_le(raw: &[u8]) -> Vec<f32> {
    raw.chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

/// Decode the band-aware gain table ("OBGT") for one speaker.
fn decode_band_gaintable(bytes: &[u8], version: u32) -> Option<GainTable> {
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
    let raw = inflate(&bytes[meta_end..payload_end])?;

    let dim = |k: &str| metadata.get(k).and_then(|x| x.as_u64()).map(|x| x as usize);
    let x_count = dim("x_count")?;
    let y_count = dim("y_count")?;
    let z_count = dim("z_count")?;
    let band_count = dim("band_count")?;
    // Signed: -1 (GLOBAL_ENERGY_INDEX) marks the all-speaker energy field, and
    // an unsigned read would silently fold it onto speaker 0.
    let speaker_index = metadata
        .get("speaker_index")
        .and_then(|x| x.as_i64())
        .unwrap_or(0);
    let bands = metadata
        .get("bands")
        .and_then(|b| b.as_array())
        .map(|arr| {
            arr.iter()
                .map(|b| BandEdge {
                    low_hz: b.get("low_hz").and_then(|x| x.as_f64()).unwrap_or(0.0),
                    high_hz: b.get("high_hz").and_then(|x| x.as_f64()),
                })
                .collect()
        })
        .unwrap_or_default();

    Some(GainTable::CartesianBands {
        version,
        speaker_index,
        x_count,
        y_count,
        z_count,
        band_count,
        bands,
        data: f32_le(&raw),
    })
}

fn decode_evaluation_artifact(bytes: &[u8], version: u32) -> Option<GainTable> {
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
    let raw = inflate(&bytes[meta_end..payload_end])?;

    let domain = metadata.get("domain")?;
    let kind = domain.get("kind")?.as_str()?;
    let dim = |k: &str| domain.get(k).and_then(|x| x.as_u64()).map(|x| x as usize);
    let mut off = 0usize;
    let mut read_f32 = |count: usize| -> Option<Vec<f32>> {
        let end = off + count * 4;
        if end > raw.len() {
            return None;
        }
        let v = f32_le(&raw[off..end]);
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
            let x_positions = read_f32(xc)?;
            let y_positions = read_f32(yc)?;
            let z_positions = read_f32(zc)?;
            let gains = read_f32(xc * yc * zc * sc)?;
            Some(GainTable::Cartesian {
                version,
                speaker_count: sc,
                x_positions,
                y_positions,
                z_positions,
                gains,
            })
        }
        "polar" => {
            let (ac, ec, dc, sc) = (
                dim("azimuth_count")?,
                dim("elevation_count")?,
                dim("distance_count")?,
                dim("speaker_count")?,
            );
            let azimuth_positions = read_f32(ac)?;
            let elevation_positions = read_f32(ec)?;
            let distance_positions = read_f32(dc)?;
            let gains = read_f32(ac * ec * dc * sc)?;
            Some(GainTable::Polar {
                version,
                speaker_count: sc,
                azimuth_positions,
                elevation_positions,
                distance_positions,
                gains,
            })
        }
        _ => None,
    }
}
