//! Host audio layer for the orender renderer.
//!
//! This crate is the OSC control surface for audio **output** and **input**
//! (`/omniphony/control/audio/*` and `/omniphony/control/input/*`): it applies
//! those messages to the shared [`audio_output::AudioControl`] and
//! [`audio_input::InputControl`] and publishes their state. The device I/O
//! itself (backends, the adaptive resampler, the pacer) lives in the
//! `audio_output` and `audio_input` crates and is driven by the host. It sits
//! *above* the engine: it depends on `runtime_control` (the audio-free core),
//! `audio_output` and `audio_input` — never the other way around.
//!
//! Hosts that own audio (the `orender` CLI/service) build a [`HostAudio`] and
//! register it with the engine's OSC server via
//! [`runtime_control::HostControlHandler`]. The embedded host (mpv via
//! `liborender`) registers nothing, so the core stays audio-free and
//! cross-compiles without cpal/pipewire/asio.

mod options;

use std::sync::Arc;

use audio_input::{
    InputBackend, InputClockMode, InputControl, InputLfeMode, InputMapMode, InputMode,
};
use audio_output::AudioControl;
use renderer::live_params::RendererControl;
use renderer::options::{HostBatchApplied, OptionKind, RawOptionValue};
use rosc::{OscMessage, OscPacket, OscType};
use runtime_control::HostControlHandler;
use runtime_control::live_control::WireValue;
use runtime_control::osc::{
    BroadcastUpdate, BroadcastValue, ControlEffects, Notify, parse_bool_arg,
    parse_input_layout_arg, parse_json_string_arg,
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
    /// Kept as a string so a rejected value (the retired `asio`) drops only
    /// this field, not the whole patch; see [`stage_live_input_backend`].
    backend: Option<Option<String>>,
    node: Option<Option<String>>,
    description: Option<Option<String>>,
    layout: Option<Option<String>>,
    clock_mode: Option<InputClockMode>,
    channels: Option<Option<u16>>,
    sample_rate: Option<Option<u32>>,
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
        InputMode::Pipewire => "pipewire",
    }
}

fn input_backend_name(backend: InputBackend) -> &'static str {
    match backend {
        InputBackend::Pipewire => "pipewire",
    }
}

/// Stage a live-input backend (`/control/input/live/backend`, or the
/// `liveInput.backend` field of `/control/config/input`). Only `pipewire`
/// exists. `asio` was reserved for a Windows capture path that was never
/// implemented and has been removed: it — like any other unknown value — is
/// rejected with a warning and leaves the staged backend unchanged. Returns
/// whether the value was staged.
pub(crate) fn stage_live_input_backend(input: &InputControl, value: &str) -> bool {
    match value.trim().to_ascii_lowercase().as_str() {
        "pipewire" => {
            input.set_requested_backend(Some(InputBackend::Pipewire));
            true
        }
        "asio" => {
            log::warn!(
                "OSC: live input backend 'asio' rejected: the ASIO live-input backend was never \
                 implemented and has been removed; only 'pipewire' is supported"
            );
            false
        }
        other => {
            log::warn!(
                "OSC: unknown live input backend '{other}' rejected; only 'pipewire' is supported"
            );
            false
        }
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

fn input_clock_mode_name(mode: InputClockMode) -> &'static str {
    match mode {
        InputClockMode::Dac => "dac",
        InputClockMode::Pipewire => "pipewire",
        InputClockMode::Upstream => "upstream",
    }
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

// ─── HostAudio: the OSC control handler for the host-owned audio I/O ──────────

pub struct HostAudio {
    pub renderer: Arc<RendererControl>,
    pub audio: Arc<AudioControl>,
    pub input: Arc<InputControl>,
    /// Whether the live input holds staged values not applied yet: set by a
    /// write that changes one, cleared by the apply. The `pending` flag of
    /// the `live_input` group in `/state/host_options`.
    input_staged: std::sync::atomic::AtomicBool,
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
            input_staged: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

/// The schema of this host's declared options, as a JSON array (what the
/// host appends to `/state/options_schema`).
pub fn host_options_schema_json() -> String {
    serde_json::Value::Array(renderer::options::host_schema_entries(
        options::HOST_OPTIONS,
    ))
    .to_string()
}

/// A patch field's value, owned until it is borrowed as a raw option value.
enum PatchValue {
    Null,
    Str(String),
    Number(f64),
    Bool(bool),
}

impl PatchValue {
    fn raw(&self) -> RawOptionValue<'_> {
        match self {
            Self::Null => RawOptionValue::Null,
            Self::Str(s) => RawOptionValue::Str(s),
            Self::Number(n) => RawOptionValue::Number(*n),
            Self::Bool(b) => RawOptionValue::Bool(*b),
        }
    }
}

/// `Option<Option<T>>` patch field → a pair: absent leaves the option out,
/// `null` unsets it, a value sets it.
fn push_nullable<T>(
    out: &mut Vec<(&'static str, PatchValue)>,
    key: &'static str,
    field: Option<Option<T>>,
    value: impl FnOnce(T) -> PatchValue,
) {
    match field {
        None => {}
        Some(None) => out.push((key, PatchValue::Null)),
        Some(Some(v)) => out.push((key, value(v))),
    }
}

fn push_some<T>(
    out: &mut Vec<(&'static str, PatchValue)>,
    key: &'static str,
    field: Option<T>,
    value: impl FnOnce(T) -> PatchValue,
) {
    if let Some(v) = field {
        out.push((key, value(v)));
    }
}

/// `/control/config/audio` as a batch of this host's options.
fn audio_patch_values(patch: AudioConfigPatch) -> Vec<(&'static str, PatchValue)> {
    let mut out = Vec::new();
    push_nullable(
        &mut out,
        "output_device",
        patch.output_device,
        PatchValue::Str,
    );
    push_nullable(
        &mut out,
        "output_backend",
        patch.output_backend,
        PatchValue::Str,
    );
    push_nullable(&mut out, "output_file", patch.output_file, PatchValue::Str);
    push_nullable(
        &mut out,
        "output_file_format",
        patch.output_file_format,
        PatchValue::Str,
    );
    push_nullable(&mut out, "output_sample_rate", patch.sample_rate, |v| {
        PatchValue::Number(v as f64)
    });
    push_nullable(&mut out, "latency_target", patch.latency_target_ms, |v| {
        PatchValue::Number(v as f64)
    });
    if let Some(a) = patch.adaptive_resampling {
        let b = PatchValue::Bool;
        let n = |v: f64| PatchValue::Number(v);
        push_some(&mut out, "enable_adaptive_resampling", a.enabled, b);
        push_some(
            &mut out,
            "adaptive_resampling_enable_far_mode",
            a.enable_far_mode,
            b,
        );
        push_some(
            &mut out,
            "adaptive_resampling_force_silence_in_far_mode",
            a.force_silence_in_far_mode,
            b,
        );
        push_some(
            &mut out,
            "adaptive_resampling_hard_recover_high_in_far_mode",
            a.hard_recover_high_in_far_mode,
            b,
        );
        push_some(
            &mut out,
            "adaptive_resampling_hard_recover_low_in_far_mode",
            a.hard_recover_low_in_far_mode,
            b,
        );
        push_some(
            &mut out,
            "adaptive_resampling_far_mode_return_fade_in_ms",
            a.far_mode_return_fade_in_ms,
            |v| n(v as f64),
        );
        push_some(&mut out, "adaptive_resampling_kp_near", a.kp_near, n);
        push_some(&mut out, "adaptive_resampling_ki", a.ki, n);
        push_some(
            &mut out,
            "adaptive_resampling_integral_discharge_ratio",
            a.integral_discharge_ratio,
            n,
        );
        push_some(&mut out, "adaptive_resampling_max_adjust", a.max_adjust, n);
        push_some(
            &mut out,
            "adaptive_resampling_high_recover_entry_margin_ms",
            a.high_recover_entry_margin_ms,
            |v| n(v as f64),
        );
        push_some(
            &mut out,
            "adaptive_resampling_update_interval_callbacks",
            a.update_interval_callbacks,
            |v| n(v as f64),
        );
        push_some(
            &mut out,
            "adaptive_resampling_low_recover_settle_stable_ms",
            a.low_recover_settle_stable_ms,
            |v| n(v as f64),
        );
        push_some(
            &mut out,
            "adaptive_resampling_low_recover_entry_margin_ms",
            a.low_recover_entry_margin_ms,
            |v| n(v as f64),
        );
        push_some(
            &mut out,
            "adaptive_resampling_low_recover_exit_margin_ms",
            a.low_recover_exit_margin_ms,
            |v| n(v as f64),
        );
        push_some(
            &mut out,
            "adaptive_resampling_low_recover_settle_margin_ms",
            a.low_recover_settle_margin_ms,
            |v| n(v as f64),
        );
        push_some(
            &mut out,
            "adaptive_resampling_low_recover_refill_delta_alpha",
            a.low_recover_refill_delta_alpha,
            |v| n(v as f64),
        );
        push_some(
            &mut out,
            "adaptive_resampling_control_smoothing_cutoff_hz",
            a.control_smoothing_cutoff_hz,
            |v| n(v as f64),
        );
        push_some(
            &mut out,
            "adaptive_resampling_control_smoothing_order",
            a.control_smoothing_order,
            |v| n(v as f64),
        );
        push_some(
            &mut out,
            "adaptive_resampling_use_pre_bridge_clock",
            a.use_pre_bridge_clock,
            b,
        );
        push_some(
            &mut out,
            "adaptive_resampling_use_output_pacing",
            a.use_output_pacing,
            b,
        );
        push_some(
            &mut out,
            "adaptive_resampling_disable_backpressure",
            a.disable_backpressure,
            b,
        );
    }
    out
}

/// `/control/config/input` as a batch of this host's options.
fn input_patch_values(patch: InputConfigPatch) -> Vec<(&'static str, PatchValue)> {
    let mut out = Vec::new();
    push_some(&mut out, "input_mode", patch.mode, |mode| {
        PatchValue::Str(input_mode_name(mode).to_string())
    });
    if let Some(live) = patch.live_input {
        push_nullable(
            &mut out,
            "live_input_backend",
            live.backend,
            PatchValue::Str,
        );
        push_nullable(&mut out, "live_input_node", live.node, PatchValue::Str);
        push_nullable(
            &mut out,
            "live_input_description",
            live.description,
            PatchValue::Str,
        );
        push_nullable(&mut out, "live_input_layout", live.layout, PatchValue::Str);
        push_some(&mut out, "live_input_clock_mode", live.clock_mode, |mode| {
            PatchValue::Str(input_clock_mode_name(mode).to_string())
        });
        push_nullable(&mut out, "live_input_channels", live.channels, |v| {
            PatchValue::Number(v as f64)
        });
        push_nullable(&mut out, "live_input_sample_rate", live.sample_rate, |v| {
            PatchValue::Number(v as f64)
        });
        push_some(&mut out, "live_input_map", live.map, |mode| {
            PatchValue::Str(input_map_mode_name(mode).to_string())
        });
        push_some(&mut out, "live_input_lfe_mode", live.lfe_mode, |mode| {
            PatchValue::Str(input_lfe_mode_name(mode).to_string())
        });
    }
    out
}

/// What a client learns from a write of this host's options: a changed
/// value is a config edit (Save lights up), an unchanged one is still
/// published (a clamp must reach the client that typed the value), a
/// rejected one changes nothing.
fn batch_effects(batch: &HostBatchApplied, log: String) -> ControlEffects {
    if batch.changed {
        let mut effects = ControlEffects::dirty(Notify::Snapshot);
        effects.log_message = Some(log);
        effects
    } else if batch.results.iter().any(Option::is_some) {
        ControlEffects::transient(Notify::Snapshot)
    } else {
        ControlEffects::default()
    }
}

impl HostAudio {
    fn apply_patch(&self, values: &[(&'static str, PatchValue)], log: &str) -> ControlEffects {
        let items: Vec<(&str, RawOptionValue)> = values
            .iter()
            .map(|(key, value)| (*key, value.raw()))
            .collect();
        let batch = self.apply_options(&items);
        batch_effects(&batch, log.to_string())
    }
}

impl HostControlHandler for HostAudio {
    fn handle(&self, addr: &str, msg: &OscMessage) -> Option<ControlEffects> {
        let audio = &self.audio;
        let input = &self.input;
        let mut effects = ControlEffects::default();

        // ── The JSON patches: aliases of a batch of this host's options ──
        if addr == osc_contract::CONTROL_CONFIG_AUDIO {
            let Some(patch) = parse_json_string_arg::<AudioConfigPatch>(msg.args.first()) else {
                return Some(effects);
            };
            // Not an option: a diagnostic hold, never saved.
            if let Some(paused) = patch
                .adaptive_resampling
                .as_ref()
                .and_then(|adaptive| adaptive.paused)
            {
                audio.set_requested_adaptive_resampling_paused(paused);
            }
            let values = audio_patch_values(patch);
            return Some(self.apply_patch(&values, "OSC: audio config staged"));
        }

        if addr == osc_contract::CONTROL_CONFIG_INPUT {
            let Some(patch) = parse_json_string_arg::<InputConfigPatch>(msg.args.first()) else {
                return Some(effects);
            };
            let values = input_patch_values(patch);
            return Some(self.apply_patch(&values, "OSC: input config staged"));
        }

        // ── The per-domain apply addresses: aliases of the group apply ──
        if addr == osc_contract::CONTROL_CONFIG_AUDIO_APPLY {
            return self.apply_option_group(options::AUDIO_OUTPUT.key);
        }
        if addr == osc_contract::CONTROL_CONFIG_INPUT_APPLY
            || addr == osc_contract::CONTROL_INPUT_APPLY
        {
            return self.apply_option_group(options::LIVE_INPUT.key);
        }

        // ── The dedicated per-option addresses: aliases of the rows ──
        let key = renderer::options::find_host_by_legacy_addr(options::HOST_OPTIONS, addr)
            .map(|spec| spec.key)
            .or_else(|| {
                options::EXTRA_ALIASES
                    .iter()
                    .find(|(alias, _)| *alias == addr)
                    .map(|(_, key)| *key)
            });
        if let Some(key) = key {
            let kind = self.option_kind(key).unwrap_or(OptionKind::Str);
            let Some(args) = msg.args.get(..kind.arity()) else {
                log::warn!("OSC option {key}: missing value");
                return Some(effects);
            };
            let value = WireValue::from_args(kind, args);
            let Some(raw) = value.raw() else {
                log::warn!("OSC option {key}: rejected value");
                return Some(effects);
            };
            if options::legacy_ignores(addr, &raw) {
                return Some(effects);
            }
            let batch = self.apply_options(&[(key, raw)]);
            return Some(batch_effects(&batch, format!("OSC: {key} staged")));
        }

        // ── Not options ──
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

        // A structured layout, kept out of the options.
        if addr == osc_contract::CONTROL_INPUT_LIVE_LAYOUT_IMPORT {
            let requested = parse_input_layout_arg(msg.args.first());
            input.set_requested_current_layout(requested);
            self.input_staged
                .store(true, std::sync::atomic::Ordering::Relaxed);
            effects.mark_dirty = true;
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_ADAPTIVE_RESAMPLING_PAUSE {
            if let Some(paused) = parse_bool_arg(msg.args.first()) {
                audio.set_requested_adaptive_resampling_paused(paused);
                // A diagnostic hold, never saved.
                effects = ControlEffects::transient(Notify::Snapshot);
            }
            return Some(effects);
        }

        if addr == osc_contract::CONTROL_ADAPTIVE_RESAMPLING_RESET_RATIO {
            // An action: nothing to save, nothing to publish.
            audio.request_ratio_reset();
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

    fn option_kind(&self, key: &str) -> Option<OptionKind> {
        renderer::options::find_host(options::HOST_OPTIONS, key).map(|spec| spec.kind)
    }

    fn apply_options(&self, items: &[(&str, RawOptionValue)]) -> HostBatchApplied {
        let batch = renderer::options::host_apply_batch(self, options::HOST_OPTIONS, items);
        let staged = items.iter().zip(&batch.results).any(|((key, _), result)| {
            result.as_ref().is_some_and(|applied| applied.changed)
                && renderer::options::find_host(options::HOST_OPTIONS, key)
                    .and_then(|spec| spec.group)
                    .is_some_and(|group| std::ptr::eq(group, &options::LIVE_INPUT))
        });
        if staged {
            self.input_staged
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        batch
    }

    fn apply_option_group(&self, group: &str) -> Option<ControlEffects> {
        let mut effects = ControlEffects::transient(Notify::Snapshot);
        if group == options::LIVE_INPUT.key {
            // Staged: every value requested since the last apply, at once.
            // An action, not an edit: the writes already lit Save.
            self.input.request_apply();
            self.input_staged
                .store(false, std::sync::atomic::Ordering::Relaxed);
            effects.log_message = Some("OSC: input config apply requested".to_string());
            return Some(effects);
        }
        // Live groups: applied as they were written; acknowledged.
        options::HOST_GROUPS
            .iter()
            .any(|declared| declared.key == group)
            .then_some(effects)
    }

    fn options_schema(&self) -> Vec<serde_json::Value> {
        renderer::options::host_schema_entries(options::HOST_OPTIONS)
    }

    fn options_json(&self) -> serde_json::Map<String, serde_json::Value> {
        renderer::options::host_options_json(self, options::HOST_OPTIONS)
    }

    fn options_applied_json(&self) -> serde_json::Map<String, serde_json::Value> {
        renderer::options::host_applied_json(self, options::HOST_OPTIONS)
    }

    fn option_groups_pending(&self) -> Vec<(&'static str, bool)> {
        vec![(
            options::LIVE_INPUT.key,
            self.input_staged.load(std::sync::atomic::Ordering::Relaxed),
        )]
    }

    fn amend_saved_config(&self, render: &mut renderer::config::RenderConfig) {
        // Every declared option (audio output, adaptive resampling, live
        // input), then the imported live-input layout, which is structured
        // data rather than an option.
        renderer::options::host_store_to_config(render, self, options::HOST_OPTIONS);
        render
            .live_input
            .get_or_insert_with(Default::default)
            .current_layout = self.input.requested_snapshot().current_layout;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retired_asio_live_input_backend_is_rejected() {
        let input = InputControl::default();
        input.set_requested_backend(Some(InputBackend::Pipewire));

        assert!(!stage_live_input_backend(&input, "asio"));
        assert!(!stage_live_input_backend(&input, " ASIO "));
        assert!(!stage_live_input_backend(&input, "coreaudio"));
        // The staged backend is left as it was.
        assert_eq!(
            input.requested_snapshot().backend,
            Some(InputBackend::Pipewire)
        );
    }

    #[test]
    fn pipewire_live_input_backend_is_staged() {
        let input = InputControl::default();
        assert_eq!(input.requested_snapshot().backend, None);
        assert!(stage_live_input_backend(&input, "PipeWire"));
        assert_eq!(
            input.requested_snapshot().backend,
            Some(InputBackend::Pipewire)
        );
    }

    /// A Save writes every live-input row. A row still at what an absent key
    /// stands for leaves the value a newer build wrote there; one the user
    /// changed replaces it.
    #[test]
    fn a_save_keeps_the_live_input_values_a_newer_build_wrote() {
        let host = host();
        let mut render: renderer::config::RenderConfig = serde_json::from_str(
            r#"{"live_input": {"clock_mode": "ptp", "lfe_mode": "bass_shaker", "node": "in"}}"#,
        )
        .expect("values a newer build wrote must not fail the section");
        host.amend_saved_config(&mut render);
        let saved = serde_json::to_value(&render).unwrap();
        assert_eq!(saved["live_input"]["clock_mode"], "ptp", "{saved}");
        assert_eq!(saved["live_input"]["lfe_mode"], "bass_shaker", "{saved}");

        host.input.set_requested_lfe_mode(InputLfeMode::Object);
        host.amend_saved_config(&mut render);
        let saved = serde_json::to_value(&render).unwrap();
        assert_eq!(saved["live_input"]["clock_mode"], "ptp", "{saved}");
        assert_eq!(saved["live_input"]["lfe_mode"], "object", "{saved}");
    }

    #[test]
    fn input_config_patch_with_the_retired_backend_still_parses() {
        // A client (an older Studio) that still sends `asio` must not lose the
        // rest of its patch: only the backend field is rejected.
        let json = r#"{"mode":"pipewire","liveInput":{"backend":"asio","node":"omniphony-in"}}"#;
        let patch: InputConfigPatch = serde_json::from_str(json).expect("patch parses");
        let live_input = patch.live_input.expect("liveInput");
        assert_eq!(live_input.backend, Some(Some("asio".to_string())));
        assert_eq!(live_input.node, Some(Some("omniphony-in".to_string())));
        assert_eq!(patch.mode, Some(InputMode::Pipewire));
    }

    use renderer::options::{GroupMode, LegacyAddr, OptionDefault};

    fn fixture_control() -> Arc<RendererControl> {
        use renderer::live_params::{LiveEvaluationMode, PreferredEvaluationMode};
        use renderer::spatial_renderer::{RendererSpec, SpatialRenderer};
        use renderer::spatial_vbap::{DistanceModel, VbapTableMode};
        let layout = renderer::speaker_layout::SpeakerLayout::preset("7.1.4").expect("preset");
        SpatialRenderer::new(RendererSpec {
            speaker_layout: layout,
            sample_rate: 48_000,
            az_res_deg: 1,
            el_res_deg: 1,
            spread_resolution: 0.25,
            distance_max: 2.0,
            table_mode: VbapTableMode::Cartesian {
                x_size: 5,
                y_size: 5,
                z_size: 3,
                z_neg_size: 3,
            },
            allow_negative_z: false,
            vbap_position_interpolation: true,
            distance_model: DistanceModel::None,
            spread_from_distance: false,
            spread_distance_range: 1.0,
            spread_distance_curve: 1.0,
            spread_min: 0.0,
            spread_max: 1.0,
            log_object_positions: false,
            room_ratio: [1.0, 2.0, 1.0],
            room_ratio_rear: 2.0,
            room_ratio_lower: 0.5,
            room_ratio_center_blend: 0.5,
            master_gain_db: 0.0,
            auto_gain: false,
            use_loudness: false,
            distance_diffuse: false,
            distance_diffuse_threshold: 1.0,
            distance_diffuse_curve: 1.0,
            preferred_evaluation_mode: PreferredEvaluationMode::PrecomputedCartesian,
            initial_evaluation_mode: LiveEvaluationMode::Auto,
            cartesian_default_x_size: 5,
            cartesian_default_y_size: 5,
            cartesian_default_z_size: 3,
            cartesian_default_z_neg_size: 3,
        })
        .expect("fixture renderer")
        .renderer_control()
    }

    fn host() -> HostAudio {
        HostAudio::new(
            fixture_control(),
            Arc::new(AudioControl::default()),
            Arc::new(InputControl::default()),
        )
    }

    fn msg(addr: &str, args: Vec<OscType>) -> OscMessage {
        OscMessage {
            addr: addr.to_string(),
            args,
        }
    }

    fn s(v: &str) -> OscType {
        OscType::String(v.into())
    }

    // The Save as it was written before the options were declared: the
    // reference the rows' stores must reproduce field for field.
    /// The output backend and the file sink's destination and encoding, as a Save
    /// writes them. They start unrequested — the CLI resolves them at launch and
    /// keeps them in its runtime — so only a live request replaces what the file
    /// already holds. Defaults stay out of the file, the way the CLI's own
    /// `--save-config` writes them.
    fn legacy_store_output_sink(
        render: &mut renderer::config::RenderConfig,
        requested: &audio_output::RequestedAudioOutputConfig,
    ) {
        if let Some(backend) = &requested.output_backend {
            render.output_backend = Some(backend.clone());
        }
        if let Some(file) = &requested.output_file {
            render.output_file = (file != "-").then(|| file.clone());
        }
        if let Some(format) = &requested.output_file_format {
            let raw = matches!(format.as_str(), "raw_f32" | "rawf32" | "raw" | "f32");
            render.output_file_format = (!raw).then(|| format.clone());
        }
    }

    fn legacy_amend(host: &HostAudio, render: &mut renderer::config::RenderConfig) {
        // ── Audio output ──
        let audio = &host.audio;
        let requested = audio.requested_snapshot();
        render.output_device = requested.output_device.clone();
        render.output_sample_rate = requested.output_sample_rate_hz;
        legacy_store_output_sink(render, &requested);
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
        let input = &host.input;
        let requested = input.requested_snapshot();
        render.input_mode = Some(match requested.mode {
            InputMode::Bridge => renderer::config::InputModeConfig::Bridge,
            InputMode::Pipewire => renderer::config::InputModeConfig::Pipewire,
        });
        render.live_input = Some(renderer::config::LiveInputConfig {
            backend: requested.backend.map(|backend| match backend {
                InputBackend::Pipewire => renderer::config::InputBackendConfig::Pipewire,
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
            map: Some(match requested.map_mode {
                InputMapMode::SevenOneFixed => renderer::config::InputMapModeConfig::SevenOneFixed,
            }),
            lfe_mode: Some(match requested.lfe_mode {
                InputLfeMode::Object => renderer::config::InputLfeModeConfig::Object,
                InputLfeMode::Direct => renderer::config::InputLfeModeConfig::Direct,
                InputLfeMode::Drop => renderer::config::InputLfeModeConfig::Drop,
            }),
            ..Default::default()
        });
    }

    /// A non-default value for every row, and what each shape takes.
    fn sample(key: &str) -> RawOptionValue<'static> {
        match options::HOST_OPTIONS
            .iter()
            .find(|spec| spec.key == key)
            .expect("declared")
            .kind
        {
            OptionKind::Bool => RawOptionValue::Bool(!matches!(
                options::HOST_OPTIONS
                    .iter()
                    .find(|s| s.key == key)
                    .unwrap()
                    .default,
                OptionDefault::Bool(true)
            )),
            _ => match key {
                "output_device" => RawOptionValue::Str("hw:1"),
                "output_backend" => RawOptionValue::Str("file"),
                "output_file" => RawOptionValue::Str("/tmp/out.caf"),
                "output_file_format" => RawOptionValue::Str("caf"),
                "output_sample_rate" | "live_input_sample_rate" => RawOptionValue::Number(96_000.0),
                "latency_target" => RawOptionValue::Number(120.0),
                "adaptive_resampling_far_mode_return_fade_in_ms" => RawOptionValue::Number(250.0),
                "adaptive_resampling_kp_near" | "adaptive_resampling_ki" => {
                    RawOptionValue::Number(2.5)
                }
                "adaptive_resampling_integral_discharge_ratio"
                | "adaptive_resampling_low_recover_refill_delta_alpha" => {
                    RawOptionValue::Number(0.75)
                }
                "adaptive_resampling_max_adjust" => RawOptionValue::Number(0.1),
                "adaptive_resampling_update_interval_callbacks" => RawOptionValue::Number(4.0),
                "adaptive_resampling_high_recover_entry_margin_ms" => RawOptionValue::Number(800.0),
                "adaptive_resampling_low_recover_settle_stable_ms"
                | "adaptive_resampling_low_recover_entry_margin_ms"
                | "adaptive_resampling_low_recover_exit_margin_ms"
                | "adaptive_resampling_low_recover_settle_margin_ms" => {
                    RawOptionValue::Number(42.0)
                }
                "adaptive_resampling_control_smoothing_cutoff_hz" => RawOptionValue::Number(2.0),
                "adaptive_resampling_control_smoothing_order" => RawOptionValue::Number(2.0),
                "input_mode" => RawOptionValue::Str("pipewire"),
                "live_input_backend" => RawOptionValue::Str("pipewire"),
                "live_input_node" => RawOptionValue::Str("omniphony-in"),
                "live_input_description" => RawOptionValue::Str("Omniphony input"),
                "live_input_layout" => RawOptionValue::Str("/layouts/in.yaml"),
                "live_input_clock_mode" => RawOptionValue::Str("upstream"),
                "live_input_channels" => RawOptionValue::Number(12.0),
                "live_input_map" => RawOptionValue::Str("7.1-fixed"),
                "live_input_lfe_mode" => RawOptionValue::Str("object"),
                other => panic!("no sample for host option {other}"),
            },
        }
    }

    #[test]
    fn every_row_is_declared_once_with_a_catalogued_alias() {
        let mut keys = std::collections::HashSet::new();
        for spec in options::HOST_OPTIONS {
            assert!(keys.insert(spec.key), "duplicate host option {}", spec.key);
            assert!(
                renderer::options::find(spec.key).is_none(),
                "{}: also a core option",
                spec.key
            );
            let group = spec.group.expect("every host option has a group");
            assert!(options::HOST_GROUPS.iter().any(|g| std::ptr::eq(*g, group)));
            match spec.legacy_control_addr {
                LegacyAddr::Exact(addr) => {
                    assert!(osc_contract::ALL_CONTROL.contains(&addr), "{}", spec.key)
                }
                LegacyAddr::None => {}
                LegacyAddr::Prefixed { .. } => panic!("{}: no prefix family here", spec.key),
            }
        }
        for (alias, key) in options::EXTRA_ALIASES {
            assert!(osc_contract::ALL_CONTROL.contains(alias));
            assert!(keys.contains(key));
        }
        assert_eq!(options::LIVE_INPUT.mode, GroupMode::Staged);
    }

    /// Every row: the sample changes the value, the same value again does
    /// not, and the Save through the rows writes exactly what the Save
    /// wrote before the options were declared.
    #[test]
    fn rows_set_and_save_like_the_hand_written_save() {
        let host = host();
        let items: Vec<(&str, RawOptionValue)> = options::HOST_OPTIONS
            .iter()
            .map(|spec| (spec.key, sample(spec.key)))
            .collect();
        let batch = host.apply_options(&items);
        for ((key, _), result) in items.iter().zip(&batch.results) {
            let result = result
                .as_ref()
                .unwrap_or_else(|| panic!("{key}: sample rejected"));
            // The input map has a single value today: nothing to change to.
            assert!(
                result.changed || *key == "live_input_map",
                "{key}: the sample is not a change"
            );
        }
        let again = host.apply_options(&items);
        assert!(!again.changed);

        let base = renderer::config::RenderConfig {
            output_backend: Some("pipewire".into()),
            ..Default::default()
        };
        let mut through_rows = base.clone();
        host.amend_saved_config(&mut through_rows);
        let mut reference = base;
        legacy_amend(&host, &mut reference);
        assert_eq!(
            serde_json::to_value(&through_rows).unwrap(),
            serde_json::to_value(&reference).unwrap()
        );

        // At the defaults too, where the file-sink rows leave the file alone.
        let host = self::host();
        let base = renderer::config::RenderConfig {
            output_backend: Some("pipewire".into()),
            output_file: Some("/srv/fifo".into()),
            ..Default::default()
        };
        let mut through_rows = base.clone();
        host.amend_saved_config(&mut through_rows);
        let mut reference = base;
        legacy_amend(&host, &mut reference);
        assert_eq!(
            serde_json::to_value(&through_rows).unwrap(),
            serde_json::to_value(&reference).unwrap()
        );
        assert_eq!(through_rows.output_backend.as_deref(), Some("pipewire"));
    }

    /// The JSON patch is an alias of a batch of the rows: the same values
    /// through `/control/config/audio` and through the rows land the same
    /// requested state.
    #[test]
    fn the_audio_patch_is_a_batch_of_the_rows() {
        let patch = r#"{"outputDevice":" hw:2 ","sampleRate":0,"latencyTargetMs":80,
            "adaptiveResampling":{"enabled":true,"kpNear":-1,"ki":0.5,
            "integralDischargeRatio":-2,"nearFarThresholdMs":700,"paused":true}}"#;
        let by_patch = host();
        let effects = by_patch
            .handle(
                osc_contract::CONTROL_CONFIG_AUDIO,
                &msg(osc_contract::CONTROL_CONFIG_AUDIO, vec![s(patch)]),
            )
            .expect("handled");
        assert!(effects.mark_dirty);
        let requested = by_patch.audio.requested_snapshot();
        assert_eq!(requested.output_device.as_deref(), Some("hw:2"));
        assert_eq!(requested.output_sample_rate_hz, None, "0 unsets the rate");
        assert_eq!(requested.latency_target_ms, Some(80));
        assert!(requested.adaptive_enabled);
        assert_eq!(
            requested.adaptive.kp_near, 1.0,
            "a non-positive kp is ignored"
        );
        assert_eq!(requested.adaptive.ki, 0.5);
        assert_eq!(requested.adaptive.integral_discharge_ratio, 0.0, "clamped");
        assert_eq!(
            requested.adaptive.high_recover_entry_margin_ms, 700,
            "old spelling"
        );
        assert!(
            requested.adaptive.paused,
            "the diagnostic hold still applies"
        );

        let by_rows = host();
        by_rows.apply_options(&[
            ("output_device", RawOptionValue::Str("hw:2")),
            ("output_sample_rate", RawOptionValue::Null),
            ("latency_target", RawOptionValue::Number(80.0)),
            ("enable_adaptive_resampling", RawOptionValue::Bool(true)),
            ("adaptive_resampling_ki", RawOptionValue::Number(0.5)),
            (
                "adaptive_resampling_integral_discharge_ratio",
                RawOptionValue::Number(0.0),
            ),
            (
                "adaptive_resampling_high_recover_entry_margin_ms",
                RawOptionValue::Number(700.0),
            ),
        ]);
        assert_eq!(
            renderer::options::host_options_json(&by_patch, options::HOST_OPTIONS),
            renderer::options::host_options_json(&by_rows, options::HOST_OPTIONS)
        );

        // The same patch again changes nothing: published, not dirty.
        let again = by_patch
            .handle(
                osc_contract::CONTROL_CONFIG_AUDIO,
                &msg(osc_contract::CONTROL_CONFIG_AUDIO, vec![s(patch)]),
            )
            .expect("handled");
        assert!(!again.mark_dirty && again.publish_only);
    }

    /// The live input is staged: a write (dedicated address, patch or row)
    /// changes the requested value only; the apply — any of its aliases —
    /// hands them over at once, as an action that lights no Save.
    #[test]
    fn the_live_input_is_staged_and_applied_on_command() {
        let host = host();
        let effects = host
            .handle(
                osc_contract::CONTROL_INPUT_LIVE_CHANNELS,
                &msg(
                    osc_contract::CONTROL_INPUT_LIVE_CHANNELS,
                    vec![OscType::Int(12)],
                ),
            )
            .expect("handled");
        assert!(effects.mark_dirty);
        let patch =
            r#"{"mode":"pipewire","liveInput":{"backend":"asio","node":" in ","sampleRate":null}}"#;
        host.handle(
            osc_contract::CONTROL_CONFIG_INPUT,
            &msg(osc_contract::CONTROL_CONFIG_INPUT, vec![s(patch)]),
        )
        .expect("handled");
        let requested = host.input.requested_snapshot();
        assert_eq!(requested.channels, Some(12));
        assert_eq!(requested.mode, InputMode::Pipewire);
        assert_eq!(
            requested.backend, None,
            "the retired backend is refused alone"
        );
        assert_eq!(requested.node_name.as_deref(), Some("in"));
        assert!(!host.input.is_apply_pending());
        // Staged values wait for the apply.
        assert_eq!(host.option_groups_pending(), vec![("live_input", true)]);
        // Requested and in force side by side.
        let applied = host.options_applied_json();
        assert_eq!(applied["input_mode"], "pipe_bridge");
        assert_eq!(host.options_json()["input_mode"], "pipewire");

        for addr in [
            osc_contract::CONTROL_INPUT_APPLY,
            osc_contract::CONTROL_CONFIG_INPUT_APPLY,
        ] {
            let effects = host.handle(addr, &msg(addr, vec![])).expect("handled");
            assert!(!effects.mark_dirty, "{addr}: an apply is an action");
            assert!(effects.publish_only);
            assert!(host.input.take_apply_pending());
            assert_eq!(host.option_groups_pending(), vec![("live_input", false)]);
        }
        // A write that changes nothing stages nothing.
        host.handle(
            osc_contract::CONTROL_INPUT_LIVE_CHANNELS,
            &msg(
                osc_contract::CONTROL_INPUT_LIVE_CHANNELS,
                vec![OscType::Int(12)],
            ),
        );
        assert_eq!(host.option_groups_pending(), vec![("live_input", false)]);
        // An audio-output write is not the input's.
        host.apply_options(&[("output_device", RawOptionValue::Str("hw:9"))]);
        assert_eq!(host.option_groups_pending(), vec![("live_input", false)]);
        assert!(host.apply_option_group("live_input").is_some());
        assert!(host.input.take_apply_pending());
        // A live group has nothing waiting: acknowledged, nothing staged.
        let effects = host.apply_option_group("audio_output").expect("ours");
        assert!(!effects.mark_dirty);
        assert!(host.apply_option_group("no_such_group").is_none());
    }

    /// The dedicated addresses keep what they ignored: a non-positive
    /// latency, input channel count or rate, a negative discharge ratio.
    #[test]
    fn the_dedicated_addresses_keep_their_old_rejections() {
        let host = host();
        host.audio.set_requested_latency_target_ms(Some(100));
        host.input.set_requested_channels(Some(8));
        for (addr, arg) in [
            (osc_contract::CONTROL_LATENCY_TARGET, OscType::Int(0)),
            (osc_contract::CONTROL_INPUT_LIVE_CHANNELS, OscType::Int(-1)),
            (
                osc_contract::CONTROL_ADAPTIVE_RESAMPLING_INTEGRAL_DISCHARGE_RATIO,
                OscType::Float(-0.5),
            ),
        ] {
            let effects = host.handle(addr, &msg(addr, vec![arg])).expect("handled");
            assert!(!effects.mark_dirty && !effects.publish_only, "{addr}");
        }
        assert_eq!(host.audio.requested_snapshot().latency_target_ms, Some(100));
        assert_eq!(host.input.requested_snapshot().channels, Some(8));
        // Through the rows, a non-positive value unsets, as the patch did.
        host.apply_options(&[("latency_target", RawOptionValue::Number(0.0))]);
        assert_eq!(host.audio.requested_snapshot().latency_target_ms, None);
        // An older spelling of an address still lands on its row.
        let effects = host
            .handle(
                osc_contract::CONTROL_ADAPTIVE_RESAMPLING_HARD_RECOVER_IN_FAR_MODE,
                &msg(
                    osc_contract::CONTROL_ADAPTIVE_RESAMPLING_HARD_RECOVER_IN_FAR_MODE,
                    vec![OscType::Int(0)],
                ),
            )
            .expect("handled");
        assert!(effects.mark_dirty);
        assert!(
            !host
                .audio
                .requested_snapshot()
                .adaptive
                .hard_recover_high_in_far_mode
        );
    }

    #[test]
    fn the_schema_carries_every_row_with_its_group() {
        let schema = host().options_schema();
        assert_eq!(schema.len(), options::HOST_OPTIONS.len());
        for (spec, entry) in options::HOST_OPTIONS.iter().zip(&schema) {
            assert_eq!(entry["key"], spec.key);
            assert_eq!(entry["group"]["key"], spec.group.unwrap().key);
        }
        let input = schema
            .iter()
            .find(|e| e["key"] == "live_input_channels")
            .unwrap();
        assert_eq!(input["group"]["mode"], "staged");
        assert_eq!(input["group"]["effect"], "restart_input");
        assert!(input["default"].is_null());
    }
}
