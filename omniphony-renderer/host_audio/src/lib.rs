//! Host audio layer for the orender renderer.
//!
//! This crate owns audio **output** and **input** (device I/O, the adaptive
//! resampler, the pacer) and the OSC control surface for them
//! (`/omniphony/control/audio/*` and `/omniphony/control/input/*`). It sits
//! *above* the engine: it depends on `runtime_control` (the audio-free core),
//! `audio_output` and `audio_input` — never the other way around.
//!
//! Hosts that own audio (the `orender` CLI/service) build a [`HostAudio`] and
//! register it with the engine's OSC server via
//! [`runtime_control::HostControlHandler`]. The embedded host (mpv via
//! `liborender`) registers nothing, so the core stays audio-free and
//! cross-compiles without cpal/pipewire/asio.

use std::path::PathBuf;
use std::sync::Arc;

use audio_input::{
    InputBackend, InputClockMode, InputControl, InputLfeMode, InputMapMode, InputMode,
    InputSampleFormat,
};
use audio_output::AudioControl;
use renderer::live_params::RendererControl;
use rosc::{OscMessage, OscPacket, OscType};
use runtime_control::HostControlHandler;
use runtime_control::osc::{
    BroadcastUpdate, BroadcastValue, ControlEffects, parse_bool_arg, parse_input_layout_arg,
    parse_json_string_arg, parse_nonnegative_f32_arg, parse_nonnegative_u32_arg,
    parse_positive_f32_arg, parse_positive_u32_arg, parse_string_arg,
};
use runtime_control::osc_contract;
use serde::Deserialize;
use serde_json::json;

// ─── Patch structs (deserialised from /control/config/{audio,input} JSON) ──────

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AdaptiveResamplingPatch {
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
    #[serde(alias = "nearFarThresholdMs")]
    high_recover_entry_margin_ms: Option<u32>,
    update_interval_callbacks: Option<u32>,
    low_recover_settle_stable_ms: Option<f32>,
    low_recover_entry_margin_ms: Option<f32>,
    low_recover_exit_margin_ms: Option<f32>,
    low_recover_settle_margin_ms: Option<f32>,
    low_recover_refill_delta_alpha: Option<f32>,
    control_smoothing_cutoff_hz: Option<f32>,
    control_smoothing_order: Option<u32>,
    paused: Option<bool>,
    use_pre_bridge_clock: Option<bool>,
    use_output_pacing: Option<bool>,
    disable_backpressure: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AudioConfigPatch {
    output_device: Option<Option<String>>,
    output_backend: Option<Option<String>>,
    output_file: Option<Option<String>>,
    output_file_format: Option<Option<String>>,
    sample_rate: Option<Option<u32>>,
    latency_target_ms: Option<Option<u32>>,
    adaptive_resampling: Option<AdaptiveResamplingPatch>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LiveInputPatch {
    backend: Option<Option<InputBackend>>,
    node: Option<Option<String>>,
    description: Option<Option<String>>,
    layout: Option<Option<String>>,
    clock_mode: Option<InputClockMode>,
    channels: Option<Option<u16>>,
    sample_rate: Option<Option<u32>>,
    format: Option<Option<InputSampleFormat>>,
    map: Option<InputMapMode>,
    lfe_mode: Option<InputLfeMode>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct InputConfigPatch {
    mode: Option<InputMode>,
    live_input: Option<LiveInputPatch>,
}

// ─── Enum-to-string helpers (for snapshot JSON; mirror snapshot.rs) ────────────

fn input_mode_name(mode: InputMode) -> &'static str {
    match mode {
        InputMode::Bridge => "pipe_bridge",
        InputMode::Live => "pipewire",
        InputMode::PipewireBridge => "pipewire_bridge",
    }
}

fn input_backend_name(backend: InputBackend) -> &'static str {
    match backend {
        InputBackend::Pipewire => "pipewire",
        InputBackend::Asio => "asio",
    }
}

fn input_map_mode_name(mode: InputMapMode) -> &'static str {
    match mode {
        InputMapMode::SevenOneFixed => "7.1-fixed",
    }
}

fn input_lfe_mode_name(mode: InputLfeMode) -> &'static str {
    match mode {
        InputLfeMode::Object => "object",
        InputLfeMode::Direct => "direct",
        InputLfeMode::Drop => "drop",
    }
}

fn input_sample_format_name(format: InputSampleFormat) -> &'static str {
    match format {
        InputSampleFormat::F32 => "f32",
        InputSampleFormat::S16 => "s16",
    }
}

fn input_clock_mode_name(mode: InputClockMode) -> &'static str {
    match mode {
        InputClockMode::Dac => "dac",
        InputClockMode::Pipewire => "pipewire",
        InputClockMode::Upstream => "upstream",
    }
}

/// Trim a patched string value, mapping empty to `None` (= "leave unset").
fn trim_to_opt(value: Option<String>) -> Option<String> {
    value.and_then(|v| {
        let trimmed = v.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

// ─── JSON / broadcast helpers (raw-enum format used by the live-edit broadcasts)

fn build_audio_state_json(audio: &AudioControl) -> String {
    let requested = audio.requested_snapshot();
    let (_, sample_format) = audio.audio_state();
    json!({
        "outputDevices": audio.available_output_devices(),
        "outputDevice": requested.output_device,
        "outputDeviceEffective": audio.effective_output_device(),
        "outputBackend": requested.output_backend,
        "outputFile": requested.output_file,
        "outputFileFormat": requested.output_file_format,
        "sampleRate": requested.output_sample_rate_hz,
        "sampleFormat": sample_format,
        "error": audio.audio_error(),
        "adaptiveResampling": {
            "enabled": requested.adaptive_enabled,
            "enableFarMode": requested.adaptive.enable_far_mode,
            "forceSilenceInFarMode": requested.adaptive.force_silence_in_far_mode,
            "hardRecoverHighInFarMode": requested.adaptive.hard_recover_high_in_far_mode,
            "hardRecoverLowInFarMode": requested.adaptive.hard_recover_low_in_far_mode,
            "farModeReturnFadeInMs": requested.adaptive.far_mode_return_fade_in_ms,
            "kpNear": requested.adaptive.kp_near,
            "ki": requested.adaptive.ki,
            "integralDischargeRatio": requested.adaptive.integral_discharge_ratio,
            "maxAdjust": requested.adaptive.max_adjust,
            "updateIntervalCallbacks": requested.adaptive.update_interval_callbacks,
            "highRecoverEntryMarginMs": requested.adaptive.high_recover_entry_margin_ms,
            "lowRecoverSettleStableMs": requested.adaptive.low_recover_settle_stable_ms,
            "lowRecoverEntryMarginMs": requested.adaptive.low_recover_entry_margin_ms,
            "lowRecoverExitMarginMs": requested.adaptive.low_recover_exit_margin_ms,
            "lowRecoverSettleMarginMs": requested.adaptive.low_recover_settle_margin_ms,
            "lowRecoverRefillDeltaAlpha": requested.adaptive.low_recover_refill_delta_alpha,
            "controlSmoothingCutoffHz": requested.adaptive.control_smoothing_cutoff_hz,
            "controlSmoothingOrder": requested.adaptive.control_smoothing_order,
            "paused": requested.adaptive.paused,
            "usePreBridgeClock": requested.adaptive.use_pre_bridge_clock,
            "useOutputPacing": requested.adaptive.use_output_pacing,
            "disableBackpressure": requested.adaptive.disable_backpressure
        },
        "latencyTargetMs": requested.latency_target_ms
    })
    .to_string()
}

fn build_input_state_json(input: &InputControl) -> String {
    let requested = input.requested_snapshot();
    let applied = input.applied_snapshot();
    json!({
        "mode": requested.mode,
        "activeMode": applied.active_mode,
        "applyPending": input.is_apply_pending(),
        "requested": {
            "backend": requested.backend,
            "node": requested.node_name,
            "description": requested.node_description,
            "layout": requested.layout_path.as_ref().map(|path| path.display().to_string()),
            "clockMode": requested.clock_mode,
            "channels": requested.channels,
            "sampleRate": requested.sample_rate_hz,
            "format": requested.sample_format,
            "map": requested.map_mode,
            "lfeMode": requested.lfe_mode
        },
        "applied": {
            "backend": applied.backend,
            "channels": applied.channels,
            "sampleRate": applied.sample_rate_hz,
            "node": applied.node_name,
            "description": applied.node_description,
            "streamFormat": applied.stream_format,
            "error": applied.input_error
        }
    })
    .to_string()
}

fn push_audio_domain_broadcasts(
    effects: &mut ControlEffects,
    audio: &AudioControl,
    include_logical_apply: bool,
) {
    effects.broadcasts.push(BroadcastUpdate {
        addr: osc_contract::STATE_AUDIO.to_string(),
        value: BroadcastValue::String(build_audio_state_json(audio)),
    });
    if include_logical_apply {
        effects.log_message = Some("OSC: audio config staged".to_string());
    }
}

fn push_input_domain_broadcasts(
    effects: &mut ControlEffects,
    input: &InputControl,
    include_logical_apply: bool,
) {
    effects.broadcasts.push(BroadcastUpdate {
        addr: osc_contract::STATE_INPUT.to_string(),
        value: BroadcastValue::String(build_input_state_json(input)),
    });
    if include_logical_apply {
        effects.log_message = Some("OSC: input config staged".to_string());
    }
}

// ─── HostAudio: the host-owned audio I/O + OSC control implementation ──────────

pub struct HostAudio {
    pub renderer: Arc<RendererControl>,
    pub audio: Arc<AudioControl>,
    pub input: Arc<InputControl>,
}

impl HostAudio {
    pub fn new(
        renderer: Arc<RendererControl>,
        audio: Arc<AudioControl>,
        input: Arc<InputControl>,
    ) -> Self {
        Self {
            renderer,
            audio,
            input,
        }
    }
}

impl HostControlHandler for HostAudio {
    fn handle(&self, addr: &str, msg: &OscMessage) -> Option<ControlEffects> {
        let audio = &self.audio;
        let input = &self.input;
        let mut effects = ControlEffects::default();

        // ── /control/config/audio (batch apply) ──
        if addr == osc_contract::CONTROL_CONFIG_AUDIO {
            let patch = parse_json_string_arg::<AudioConfigPatch>(msg.args.first());
            if let Some(patch) = patch {
                if let Some(output_device) = patch.output_device {
                    audio.set_requested_output_device(output_device.and_then(|value| {
                        let trimmed = value.trim();
                        if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed.to_string())
                        }
                    }));
                }
                if let Some(output_backend) = patch.output_backend {
                    audio.set_requested_output_backend(trim_to_opt(output_backend));
                }
                if let Some(output_file) = patch.output_file {
                    audio.set_requested_output_file(trim_to_opt(output_file));
                }
                if let Some(output_file_format) = patch.output_file_format {
                    audio.set_requested_output_file_format(trim_to_opt(output_file_format));
                }
                if let Some(sample_rate) = patch.sample_rate {
                    audio.set_requested_output_sample_rate(sample_rate.filter(|value| *value > 0));
                }
                if let Some(latency_target_ms) = patch.latency_target_ms {
                    audio.set_requested_latency_target_ms(
                        latency_target_ms.filter(|value| *value > 0),
                    );
                }
                if let Some(adaptive) = patch.adaptive_resampling {
                    if let Some(enabled) = adaptive.enabled {
                        audio.set_requested_adaptive_resampling(enabled);
                    }
                    if let Some(enabled) = adaptive.enable_far_mode {
                        audio.set_requested_adaptive_resampling_enable_far_mode(enabled);
                    }
                    if let Some(enabled) = adaptive.force_silence_in_far_mode {
                        audio.set_requested_adaptive_resampling_force_silence_in_far_mode(enabled);
                    }
                    if let Some(enabled) = adaptive.hard_recover_high_in_far_mode {
                        audio.set_requested_adaptive_resampling_hard_recover_high_in_far_mode(
                            enabled,
                        );
                    }
                    if let Some(enabled) = adaptive.hard_recover_low_in_far_mode {
                        audio.set_requested_adaptive_resampling_hard_recover_low_in_far_mode(
                            enabled,
                        );
                    }
                    if let Some(value) = adaptive.far_mode_return_fade_in_ms {
                        audio.set_requested_adaptive_resampling_far_mode_return_fade_in_ms(value);
                    }
                    if let Some(value) = adaptive.kp_near.filter(|value| *value > 0.0) {
                        audio.set_requested_adaptive_resampling_kp_near(value as f32);
                    }
                    if let Some(value) = adaptive.ki.filter(|value| *value >= 0.0) {
                        audio.set_requested_adaptive_resampling_ki(value as f32);
                    }
                    if let Some(value) = adaptive
                        .integral_discharge_ratio
                        .map(|value| value.clamp(0.0, 1.0))
                    {
                        audio.set_requested_adaptive_resampling_integral_discharge_ratio(
                            value as f32,
                        );
                    }
                    if let Some(value) = adaptive.max_adjust.filter(|value| *value > 0.0) {
                        audio.set_requested_adaptive_resampling_max_adjust(value as f32);
                    }
                    if let Some(value) = adaptive
                        .high_recover_entry_margin_ms
                        .filter(|value| *value > 0)
                    {
                        audio.set_requested_adaptive_resampling_high_recover_entry_margin_ms(value);
                    }
                    if let Some(value) = adaptive
                        .update_interval_callbacks
                        .filter(|value| *value > 0)
                    {
                        audio.set_requested_adaptive_resampling_update_interval_callbacks(value);
                    }
                    if let Some(value) = adaptive
                        .low_recover_settle_stable_ms
                        .filter(|value| value.is_finite() && *value >= 0.0)
                    {
                        audio.set_requested_adaptive_resampling_low_recover_settle_stable_ms(value);
                    }
                    if let Some(value) = adaptive
                        .low_recover_entry_margin_ms
                        .filter(|value| value.is_finite() && *value >= 0.0)
                    {
                        audio.set_requested_adaptive_resampling_low_recover_entry_margin_ms(value);
                    }
                    if let Some(value) = adaptive
                        .low_recover_exit_margin_ms
                        .filter(|value| value.is_finite() && *value >= 0.0)
                    {
                        audio.set_requested_adaptive_resampling_low_recover_exit_margin_ms(value);
                    }
                    if let Some(value) = adaptive
                        .low_recover_settle_margin_ms
                        .filter(|value| value.is_finite() && *value >= 0.0)
                    {
                        audio.set_requested_adaptive_resampling_low_recover_settle_margin_ms(value);
                    }
                    if let Some(value) = adaptive
                        .low_recover_refill_delta_alpha
                        .map(|value| value.clamp(0.0, 1.0))
                    {
                        audio.set_requested_adaptive_resampling_low_recover_refill_delta_alpha(
                            value,
                        );
                    }
                    if let Some(value) = adaptive
                        .control_smoothing_cutoff_hz
                        .map(|value| value.clamp(0.001, 1000.0))
                    {
                        audio.set_requested_adaptive_resampling_control_smoothing_cutoff_hz(value);
                    }
                    if let Some(value) = adaptive.control_smoothing_order {
                        audio.set_requested_adaptive_resampling_control_smoothing_order(value);
                    }
                    if let Some(paused) = adaptive.paused {
                        audio.set_requested_adaptive_resampling_paused(paused);
                    }
                    if let Some(enabled) = adaptive.use_pre_bridge_clock {
                        audio.set_requested_adaptive_resampling_use_pre_bridge_clock(enabled);
                    }
                    if let Some(enabled) = adaptive.use_output_pacing {
                        audio.set_requested_adaptive_resampling_use_output_pacing(enabled);
                    }
                    if let Some(disabled) = adaptive.disable_backpressure {
                        audio.set_requested_adaptive_resampling_disable_backpressure(disabled);
                    }
                }
                effects.mark_dirty = true;
                push_audio_domain_broadcasts(&mut effects, audio, true);
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_CONFIG_AUDIO_APPLY {
            push_audio_domain_broadcasts(&mut effects, audio, false);
            effects.log_message = Some("OSC: audio config apply".to_string());
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_CONFIG_INPUT {
            let patch = parse_json_string_arg::<InputConfigPatch>(msg.args.first());
            if let Some(patch) = patch {
                if let Some(mode) = patch.mode {
                    input.set_requested_mode(mode);
                }
                if let Some(live_input) = patch.live_input {
                    if let Some(backend) = live_input.backend {
                        input.set_requested_backend(backend);
                    }
                    if let Some(node) = live_input.node {
                        input.set_requested_node_name(node.and_then(|value| {
                            let trimmed = value.trim();
                            if trimmed.is_empty() {
                                None
                            } else {
                                Some(trimmed.to_string())
                            }
                        }));
                    }
                    if let Some(description) = live_input.description {
                        input.set_requested_node_description(description.and_then(|value| {
                            let trimmed = value.trim();
                            if trimmed.is_empty() {
                                None
                            } else {
                                Some(trimmed.to_string())
                            }
                        }));
                    }
                    if let Some(layout) = live_input.layout {
                        input.set_requested_layout_path(layout.and_then(|value| {
                            let trimmed = value.trim();
                            if trimmed.is_empty() {
                                None
                            } else {
                                Some(PathBuf::from(trimmed))
                            }
                        }));
                        input.set_requested_current_layout(None);
                    }
                    if let Some(clock_mode) = live_input.clock_mode {
                        input.set_requested_clock_mode(clock_mode);
                    }
                    if let Some(channels) = live_input.channels {
                        input.set_requested_channels(channels.filter(|value| *value > 0));
                    }
                    if let Some(sample_rate) = live_input.sample_rate {
                        input.set_requested_sample_rate_hz(sample_rate.filter(|value| *value > 0));
                    }
                    if let Some(sample_format) = live_input.format {
                        input.set_requested_sample_format(sample_format);
                    }
                    if let Some(map_mode) = live_input.map {
                        input.set_requested_map_mode(map_mode);
                    }
                    if let Some(lfe_mode) = live_input.lfe_mode {
                        input.set_requested_lfe_mode(lfe_mode);
                    }
                }
                effects.mark_dirty = true;
                push_input_domain_broadcasts(&mut effects, input, true);
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_CONFIG_INPUT_APPLY {
            input.request_apply();
            effects.mark_dirty = true;
            push_input_domain_broadcasts(&mut effects, input, false);
            effects.log_message = Some("OSC: input config apply requested".to_string());
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_AUDIO_OUTPUT_DEVICES_REFRESH {
            if let Some(devices) = audio.refresh_available_output_devices() {
                effects.broadcasts.push(BroadcastUpdate {
                    addr: osc_contract::STATE_AUDIO.to_string(),
                    value: BroadcastValue::String(build_audio_state_json(audio)),
                });
                effects.log_message = Some(format!(
                    "OSC: output_devices/refresh → {} device(s)",
                    devices.len()
                ));
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_AUDIO_OUTPUT_DEVICE {
            let requested = msg.args.first().and_then(|arg| match arg {
                OscType::String(s) => {
                    let trimmed = s.trim();
                    if trimmed.is_empty() {
                        None
                    } else {
                        Some(trimmed.to_string())
                    }
                }
                _ => None,
            });
            audio.set_requested_output_device(requested);
            effects.mark_dirty = true;
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_AUDIO_OUTPUT_BACKEND {
            audio.set_requested_output_backend(trim_to_opt(parse_string_arg(msg.args.first())));
            effects.mark_dirty = true;
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_AUDIO_OUTPUT_FILE {
            audio.set_requested_output_file(trim_to_opt(parse_string_arg(msg.args.first())));
            effects.mark_dirty = true;
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_AUDIO_OUTPUT_FILE_FORMAT {
            audio.set_requested_output_file_format(trim_to_opt(parse_string_arg(msg.args.first())));
            effects.mark_dirty = true;
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_INPUT_MODE {
            let requested = parse_string_arg(msg.args.first()).and_then(|value| {
                match value.to_ascii_lowercase().as_str() {
                    "bridge" | "pipe_bridge" => Some(InputMode::Bridge),
                    "live" | "pipewire" => Some(InputMode::Live),
                    "pipewire_bridge" => Some(InputMode::PipewireBridge),
                    _ => None,
                }
            });
            if let Some(requested) = requested {
                input.set_requested_mode(requested);
                effects.mark_dirty = true;
                effects.log_message = Some(format!(
                    "OSC: input mode staged → {}",
                    input_mode_name(requested)
                ));
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_INPUT_LIVE_BACKEND {
            let requested = parse_string_arg(msg.args.first()).and_then(|value| {
                match value.to_ascii_lowercase().as_str() {
                    "pipewire" => Some(InputBackend::Pipewire),
                    "asio" => Some(InputBackend::Asio),
                    _ => None,
                }
            });
            if let Some(requested) = requested {
                input.set_requested_backend(Some(requested));
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_INPUT_LIVE_NODE {
            let requested = parse_string_arg(msg.args.first());
            input.set_requested_node_name(requested);
            effects.mark_dirty = true;
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_INPUT_LIVE_DESCRIPTION {
            let requested = parse_string_arg(msg.args.first());
            input.set_requested_node_description(requested);
            effects.mark_dirty = true;
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_INPUT_LIVE_LAYOUT {
            let requested = parse_string_arg(msg.args.first()).map(PathBuf::from);
            input.set_requested_layout_path(requested);
            input.set_requested_current_layout(None);
            effects.mark_dirty = true;
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_INPUT_LIVE_LAYOUT_IMPORT {
            let requested = parse_input_layout_arg(msg.args.first());
            input.set_requested_current_layout(requested);
            effects.mark_dirty = true;
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_INPUT_LIVE_CHANNELS {
            let requested = match msg.args.first() {
                Some(OscType::Int(i)) if *i > 0 => Some(*i as u16),
                Some(OscType::Float(f)) if *f > 0.0 => Some(*f as u16),
                _ => None,
            };
            if let Some(requested) = requested {
                input.set_requested_channels(Some(requested));
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_INPUT_LIVE_SAMPLE_RATE {
            if let Some(requested) = parse_positive_u32_arg(msg.args.first()) {
                input.set_requested_sample_rate_hz(Some(requested));
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_INPUT_LIVE_FORMAT {
            let requested = parse_string_arg(msg.args.first()).and_then(|value| {
                match value.to_ascii_lowercase().as_str() {
                    "f32" => Some(InputSampleFormat::F32),
                    "s16" => Some(InputSampleFormat::S16),
                    _ => None,
                }
            });
            if let Some(requested) = requested {
                input.set_requested_sample_format(Some(requested));
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_INPUT_LIVE_MAP {
            let requested = parse_string_arg(msg.args.first()).and_then(|value| {
                match value.to_ascii_lowercase().as_str() {
                    "7.1-fixed" => Some(InputMapMode::SevenOneFixed),
                    _ => None,
                }
            });
            if let Some(requested) = requested {
                input.set_requested_map_mode(requested);
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_INPUT_LIVE_LFE_MODE {
            let requested = parse_string_arg(msg.args.first()).and_then(|value| {
                match value.to_ascii_lowercase().as_str() {
                    "object" => Some(InputLfeMode::Object),
                    "direct" => Some(InputLfeMode::Direct),
                    "drop" => Some(InputLfeMode::Drop),
                    _ => None,
                }
            });
            if let Some(requested) = requested {
                input.set_requested_lfe_mode(requested);
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_INPUT_LIVE_CLOCK_MODE {
            let requested = parse_string_arg(msg.args.first()).and_then(|value| {
                match value.to_ascii_lowercase().as_str() {
                    "dac" => Some(InputClockMode::Dac),
                    "pipewire" => Some(InputClockMode::Pipewire),
                    "upstream" => Some(InputClockMode::Upstream),
                    _ => None,
                }
            });
            if let Some(requested) = requested {
                input.set_requested_clock_mode(requested);
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_INPUT_APPLY {
            input.request_apply();
            effects.mark_dirty = true;
            effects.log_message = Some("OSC: input apply requested".to_string());
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_AUDIO_SAMPLE_RATE {
            let requested_hz = match msg.args.first() {
                Some(OscType::Int(i)) if *i > 0 => Some(*i as u32),
                Some(OscType::Float(f)) if *f > 0.0 => Some(*f as u32),
                _ => None,
            };
            audio.set_requested_output_sample_rate(requested_hz);
            effects.mark_dirty = true;
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_ADAPTIVE_RESAMPLING {
            if let Some(enabled) = parse_bool_arg(msg.args.first()) {
                audio.set_requested_adaptive_resampling(enabled);
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_ADAPTIVE_RESAMPLING_ENABLE_FAR_MODE {
            if let Some(enabled) = parse_bool_arg(msg.args.first()) {
                audio.set_requested_adaptive_resampling_enable_far_mode(enabled);
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_ADAPTIVE_RESAMPLING_FORCE_SILENCE_IN_FAR_MODE {
            if let Some(enabled) = parse_bool_arg(msg.args.first()) {
                audio.set_requested_adaptive_resampling_force_silence_in_far_mode(enabled);
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_ADAPTIVE_RESAMPLING_HARD_RECOVER_HIGH_IN_FAR_MODE
            || addr == osc_contract::CONTROL_ADAPTIVE_RESAMPLING_HARD_RECOVER_IN_FAR_MODE
        {
            if let Some(enabled) = parse_bool_arg(msg.args.first()) {
                audio.set_requested_adaptive_resampling_hard_recover_high_in_far_mode(enabled);
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_ADAPTIVE_RESAMPLING_HARD_RECOVER_LOW_IN_FAR_MODE {
            if let Some(enabled) = parse_bool_arg(msg.args.first()) {
                audio.set_requested_adaptive_resampling_hard_recover_low_in_far_mode(enabled);
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_ADAPTIVE_RESAMPLING_FAR_MODE_RETURN_FADE_IN_MS {
            if let Some(value) = parse_nonnegative_u32_arg(msg.args.first()) {
                audio.set_requested_adaptive_resampling_far_mode_return_fade_in_ms(value);
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_ADAPTIVE_RESAMPLING_KP_NEAR {
            if let Some(value) = parse_positive_f32_arg(msg.args.first()) {
                audio.set_requested_adaptive_resampling_kp_near(value);
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_ADAPTIVE_RESAMPLING_KI {
            if let Some(value) = parse_nonnegative_f32_arg(msg.args.first()) {
                audio.set_requested_adaptive_resampling_ki(value);
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_ADAPTIVE_RESAMPLING_INTEGRAL_DISCHARGE_RATIO {
            if let Some(value) = parse_nonnegative_f32_arg(msg.args.first()).map(|v| v.min(1.0)) {
                audio.set_requested_adaptive_resampling_integral_discharge_ratio(value);
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_ADAPTIVE_RESAMPLING_MAX_ADJUST {
            if let Some(value) = parse_positive_f32_arg(msg.args.first()) {
                audio.set_requested_adaptive_resampling_max_adjust(value);
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_ADAPTIVE_RESAMPLING_UPDATE_INTERVAL_CALLBACKS {
            if let Some(value) = parse_positive_u32_arg(msg.args.first()) {
                audio.set_requested_adaptive_resampling_update_interval_callbacks(value);
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_ADAPTIVE_RESAMPLING_HIGH_RECOVER_ENTRY_MARGIN_MS
            || addr == osc_contract::CONTROL_ADAPTIVE_RESAMPLING_NEAR_FAR_THRESHOLD_MS
        {
            if let Some(value) = parse_positive_u32_arg(msg.args.first()) {
                audio.set_requested_adaptive_resampling_high_recover_entry_margin_ms(value);
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_ADAPTIVE_RESAMPLING_PAUSE {
            if let Some(paused) = parse_bool_arg(msg.args.first()) {
                audio.set_requested_adaptive_resampling_paused(paused);
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_ADAPTIVE_RESAMPLING_RESET_RATIO {
            audio.request_ratio_reset();
            effects.mark_dirty = true;
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_LATENCY_TARGET {
            if let Some(latency_ms) = parse_positive_u32_arg(msg.args.first()) {
                audio.set_requested_latency_target_ms(Some(latency_ms));
                effects.mark_dirty = true;
            }
            return Some(effects);
        }

        // Not ours.
        None
    }

    fn extend_snapshot(&self) -> Vec<OscPacket> {
        let audio = &self.audio;
        let input = &self.input;
        let mut messages = Vec::with_capacity(2);

        // /state/audio: full output-device + adaptive-resampling state.
        let requested = audio.requested_snapshot();
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_AUDIO.to_string(),
            args: vec![OscType::String(
                json!({
                    "outputDevices": audio.available_output_devices(),
                    "outputDevice": requested.output_device.clone(),
                    "outputDeviceEffective": audio.effective_output_device(),
                    "outputBackend": requested.output_backend.clone(),
                    "outputFile": requested.output_file.clone(),
                    "outputFileFormat": requested.output_file_format.clone(),
                    "sampleRate": requested.output_sample_rate_hz,
                    "sampleFormat": audio.audio_state().1,
                    "error": audio.audio_error(),
                    "adaptiveResampling": {
                        "enabled": requested.adaptive_enabled,
                        "enableFarMode": requested.adaptive.enable_far_mode,
                        "forceSilenceInFarMode": requested.adaptive.force_silence_in_far_mode,
                        "hardRecoverHighInFarMode": requested.adaptive.hard_recover_high_in_far_mode,
                        "hardRecoverLowInFarMode": requested.adaptive.hard_recover_low_in_far_mode,
                        "farModeReturnFadeInMs": requested.adaptive.far_mode_return_fade_in_ms,
                        "kpNear": requested.adaptive.kp_near,
                        "ki": requested.adaptive.ki,
                        "integralDischargeRatio": requested.adaptive.integral_discharge_ratio,
                        "maxAdjust": requested.adaptive.max_adjust,
                        "updateIntervalCallbacks": requested.adaptive.update_interval_callbacks,
                        "highRecoverEntryMarginMs": requested.adaptive.high_recover_entry_margin_ms,
                        "lowRecoverSettleStableMs": requested.adaptive.low_recover_settle_stable_ms,
                        "lowRecoverEntryMarginMs": requested.adaptive.low_recover_entry_margin_ms,
                        "lowRecoverExitMarginMs": requested.adaptive.low_recover_exit_margin_ms,
                        "lowRecoverSettleMarginMs": requested.adaptive.low_recover_settle_margin_ms,
                        "lowRecoverRefillDeltaAlpha": requested.adaptive.low_recover_refill_delta_alpha,
                        "controlSmoothingCutoffHz": requested.adaptive.control_smoothing_cutoff_hz,
                        "controlSmoothingOrder": requested.adaptive.control_smoothing_order,
                        "paused": requested.adaptive.paused,
                        "usePreBridgeClock": requested.adaptive.use_pre_bridge_clock,
                        "useOutputPacing": requested.adaptive.use_output_pacing,
                        "disableBackpressure": requested.adaptive.disable_backpressure
                    },
                    "latencyTargetMs": requested.latency_target_ms
                })
                .to_string(),
            )],
        }));

        // /state/input: live-input device state. DRC fields (drcMode/drcWeight/
        // supportedDrcModes) are owned by the core (decode-stage) and emitted by
        // the core's build_live_state_bundle in a separate /state/input message;
        // studio's Tauri InputDomainState parser merges partial payloads, so two
        // /state/input messages in one bundle compose cleanly.
        let requested = input.requested_snapshot();
        let applied = input.applied_snapshot();
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_INPUT.to_string(),
            args: vec![OscType::String(
                json!({
                    "mode": input_mode_name(requested.mode),
                    "activeMode": input_mode_name(applied.active_mode),
                    "applyPending": input.is_apply_pending(),
                    "requested": {
                        "backend": requested.backend.map(input_backend_name),
                        "node": requested.node_name.clone(),
                        "description": requested.node_description.clone(),
                        "layout": requested.layout_path.as_ref().map(|path| path.display().to_string()),
                        "clockMode": input_clock_mode_name(requested.clock_mode),
                        "channels": requested.channels,
                        "sampleRate": requested.sample_rate_hz,
                        "format": requested.sample_format.map(input_sample_format_name),
                        "map": input_map_mode_name(requested.map_mode),
                        "lfeMode": input_lfe_mode_name(requested.lfe_mode)
                    },
                    "applied": {
                        "backend": applied.backend.map(input_backend_name),
                        "channels": applied.channels,
                        "sampleRate": applied.sample_rate_hz,
                        "node": applied.node_name.clone(),
                        "description": applied.node_description.clone(),
                        "streamFormat": applied.stream_format.clone(),
                        "error": applied.input_error.clone()
                    }
                })
                .to_string(),
            )],
        }));

        messages
    }

    fn state_generation(&self) -> u64 {
        self.input.state_generation()
    }

    fn amend_saved_config(&self, render: &mut renderer::config::RenderConfig) {
        // ── Audio output ──
        let audio = &self.audio;
        let requested = audio.requested_snapshot();
        render.output_device = requested.output_device;
        render.output_sample_rate = requested.output_sample_rate_hz;
        renderer::config_fields::enable_adaptive_resampling::store(
            render,
            requested.adaptive_enabled,
        );
        render.adaptive_resampling_enable_far_mode = Some(requested.adaptive.enable_far_mode);
        render.adaptive_resampling_force_silence_in_far_mode =
            Some(requested.adaptive.force_silence_in_far_mode);
        render.adaptive_resampling_hard_recover_high_in_far_mode =
            Some(requested.adaptive.hard_recover_high_in_far_mode);
        render.adaptive_resampling_hard_recover_low_in_far_mode =
            Some(requested.adaptive.hard_recover_low_in_far_mode);
        render.adaptive_resampling_far_mode_return_fade_in_ms =
            Some(requested.adaptive.far_mode_return_fade_in_ms);
        render.latency_target = requested.latency_target_ms;
        render.adaptive_resampling_kp_near = Some(requested.adaptive.kp_near as f32);
        render.adaptive_resampling_ki = Some(requested.adaptive.ki as f32);
        render.adaptive_resampling_integral_discharge_ratio =
            Some(requested.adaptive.integral_discharge_ratio as f32);
        render.adaptive_resampling_max_adjust = Some(requested.adaptive.max_adjust as f32);
        render.adaptive_resampling_update_interval_callbacks =
            Some(requested.adaptive.update_interval_callbacks);
        render.adaptive_resampling_high_recover_entry_margin_ms =
            Some(requested.adaptive.high_recover_entry_margin_ms);
        render.adaptive_resampling_low_recover_settle_stable_ms =
            Some(requested.adaptive.low_recover_settle_stable_ms);
        render.adaptive_resampling_low_recover_entry_margin_ms =
            Some(requested.adaptive.low_recover_entry_margin_ms);
        render.adaptive_resampling_low_recover_exit_margin_ms =
            Some(requested.adaptive.low_recover_exit_margin_ms);
        render.adaptive_resampling_low_recover_settle_margin_ms =
            Some(requested.adaptive.low_recover_settle_margin_ms);
        render.adaptive_resampling_low_recover_refill_delta_alpha =
            Some(requested.adaptive.low_recover_refill_delta_alpha);
        render.adaptive_resampling_control_smoothing_cutoff_hz =
            Some(requested.adaptive.control_smoothing_cutoff_hz as f32);
        render.adaptive_resampling_control_smoothing_order =
            Some(requested.adaptive.control_smoothing_order);
        render.adaptive_resampling_use_pre_bridge_clock =
            Some(requested.adaptive.use_pre_bridge_clock);
        render.adaptive_resampling_use_output_pacing = Some(requested.adaptive.use_output_pacing);
        render.adaptive_resampling_disable_backpressure =
            Some(requested.adaptive.disable_backpressure);

        // ── Live input ──
        let input = &self.input;
        let requested = input.requested_snapshot();
        render.input_mode = Some(match requested.mode {
            InputMode::Bridge => renderer::config::InputModeConfig::Bridge,
            InputMode::Live => renderer::config::InputModeConfig::Live,
            InputMode::PipewireBridge => renderer::config::InputModeConfig::PipewireBridge,
        });
        render.live_input = Some(renderer::config::LiveInputConfig {
            backend: requested.backend.map(|backend| match backend {
                InputBackend::Pipewire => renderer::config::InputBackendConfig::Pipewire,
                InputBackend::Asio => renderer::config::InputBackendConfig::Asio,
            }),
            node: requested.node_name,
            description: requested.node_description,
            layout: requested.layout_path,
            current_layout: requested.current_layout,
            clock_mode: Some(match requested.clock_mode {
                InputClockMode::Dac => renderer::config::InputClockModeConfig::Dac,
                InputClockMode::Pipewire => renderer::config::InputClockModeConfig::Pipewire,
                InputClockMode::Upstream => renderer::config::InputClockModeConfig::Upstream,
            }),
            channels: requested.channels,
            sample_rate: requested.sample_rate_hz,
            sample_format: requested.sample_format.map(|format| match format {
                InputSampleFormat::F32 => "f32".to_string(),
                InputSampleFormat::S16 => "s16".to_string(),
            }),
            map: Some(match requested.map_mode {
                InputMapMode::SevenOneFixed => renderer::config::InputMapModeConfig::SevenOneFixed,
            }),
            lfe_mode: Some(match requested.lfe_mode {
                InputLfeMode::Object => renderer::config::InputLfeModeConfig::Object,
                InputLfeMode::Direct => renderer::config::InputLfeModeConfig::Direct,
                InputLfeMode::Drop => renderer::config::InputLfeModeConfig::Drop,
            }),
        });
    }
}
