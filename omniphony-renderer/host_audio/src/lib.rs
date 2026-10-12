//! Host audio layer for the orender renderer.
//!
//! This crate is the OSC control surface for audio **output** and **input**
//! (`/omniphony/control/audio/*` and `/omniphony/control/input/*`): it applies
//! those messages to the shared [`audio_output::AudioControl`] and
//! [`audio_input::InputControl`] and publishes their state. The device I/O
//! itself (the backends, the loop that holds the latency) lives in the
//! `audio_output` and `audio_sync` crates and is driven by the host. It sits
//! *above* the engine: it depends on `runtime_control` (the audio-free core),
//! `audio_output` and `audio_input` — never the other way around.
//!
//! Hosts that own audio (the `orender` CLI/service) build a [`HostAudio`] and
//! register it with the engine's OSC server via
//! [`runtime_control::HostControlHandler`]. The embedded host (mpv via
//! `liborender`) registers nothing, so the core stays audio-free and
//! cross-compiles without cpal/pipewire/asio.

#[cfg(test)]
mod doc_tables;
pub mod options;

use std::sync::Arc;

use audio_input::{InputControl, InputMode};
use audio_output::AudioControl;
use renderer::live_params::RendererControl;
use renderer::options::{HostBatchApplied, OptionKind, RawOptionValue};
use rosc::{OscMessage, OscPacket, OscType};
use runtime_control::HostControlHandler;
use runtime_control::command_table::{self, Command};
use runtime_control::live_control::WireValue;
use runtime_control::osc::{
    BroadcastUpdate, BroadcastValue, ControlEffects, Notify, parse_json_string_arg,
};
use runtime_control::osc_contract;
use serde::Deserialize;
use serde_json::json;

// ─── Patch structs (deserialised from /control/config/{audio,input} JSON) ──────

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct AudioConfigPatch {
    output_device: Option<Option<String>>,
    output_backend: Option<Option<String>>,
    output_file: Option<Option<String>>,
    output_file_format: Option<Option<String>>,
    sample_rate: Option<Option<u32>>,
    latency_target_ms: Option<Option<u32>>,
}

/// The live input's part of `/control/config/input`. A field of an older
/// client that this host has no option for is ignored.
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LiveInputPatch {
    node: Option<Option<String>>,
    description: Option<Option<String>>,
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

// ─── JSON / broadcast helpers (raw-enum format used by the live-edit broadcasts)

fn build_audio_state_json(audio: &AudioControl) -> String {
    let requested = audio.requested_snapshot();
    let (_, sample_format) = audio.audio_state();
    json!({
        "outputDevices": audio.available_output_devices(),
        "outputDevice": requested.output_device,
        "outputDeviceEffective": audio.effective_output_device(),
        // The host the open stream plays through, e.g. "WASAPI (fallback:
        // no ASIO driver)"; "" when none is open or the backend names none.
        "outputHost": audio.effective_output_host().unwrap_or_default(),
        "outputBackend": requested.output_backend,
        "outputFile": requested.output_file,
        "outputFileFormat": requested.output_file_format,
        "sampleRate": requested.output_sample_rate_hz,
        "sampleFormat": sample_format,
        "error": audio.audio_error(),
        "latencyTargetMs": requested.latency_target_ms,
        // The latency held instead, when the stream needs more than the
        // target (the sync engine raises it); null while the target holds.
        "latencyTargetHeldMs": audio.applied_snapshot().latency_held_ms
    })
    .to_string()
}

// ─── HostAudio: the OSC control handler for the host-owned audio I/O ──────────

pub struct HostAudio {
    pub renderer: Arc<RendererControl>,
    /// The output and input the option rows reach (`HostAudio` derefs to
    /// it, so `host.audio` / `host.input` read through).
    io: HostIo,
    /// Bumped by the host when its output state changed on its own (a
    /// stream opened, a device failed): part of
    /// [`state_generation`](HostControlHandler::state_generation).
    output_generation: std::sync::atomic::AtomicU64,
    /// Whether the live input holds staged values not applied yet: set by a
    /// write that changes one, cleared by the apply. The `pending` flag of
    /// the `live_input` group in `/state/host_options`.
    input_staged: std::sync::atomic::AtomicBool,
}

/// The audio output and live input a host drives: all its option rows
/// (`options::HOST_OPTIONS`) reach. Apart from the renderer, so that the
/// command line can fold option values into a config before any renderer
/// exists ([`store_host_values`]).
pub struct HostIo {
    pub audio: Arc<AudioControl>,
    pub input: Arc<InputControl>,
}

impl Default for HostIo {
    fn default() -> Self {
        Self {
            audio: Arc::new(AudioControl::default()),
            input: Arc::new(InputControl::default()),
        }
    }
}

impl std::ops::Deref for HostAudio {
    type Target = HostIo;
    fn deref(&self) -> &HostIo {
        &self.io
    }
}

impl HostAudio {
    /// Bring every declared option of this host within its kind (see
    /// [`renderer::options::bound_host_options`]): for a host whose audio
    /// output and live input were built straight from config.yaml, before
    /// anything plays. Returns the keys it changed.
    pub fn bound_options_to_their_kinds(&self) -> Vec<&'static str> {
        renderer::options::bound_host_options(&self.io, self.rows())
    }

    pub fn new(
        renderer: Arc<RendererControl>,
        audio: Arc<AudioControl>,
        input: Arc<InputControl>,
    ) -> Self {
        Self {
            renderer,
            io: HostIo { audio, input },
            output_generation: std::sync::atomic::AtomicU64::new(0),
            input_staged: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// The output's applied state changed without a client asking (a stream
    /// opened, a device failed): the snapshot goes out again.
    pub fn mark_output_changed(&self) {
        self.output_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }

    /// The rows this host declares.
    fn rows(&self) -> &'static [renderer::options::HostOptionSpec<HostIo>] {
        options::HOST_OPTIONS
    }
}

/// Write client values for this host's options (the command line's) into a
/// config as a save would: each through its row's `set`, on a blank
/// [`HostIo`], then its row's `config_store`. Returns the keys that are
/// unknown or whose value was refused.
pub fn store_host_values(
    render: &mut renderer::config::RenderConfig,
    values: &[(&str, RawOptionValue)],
) -> Vec<String> {
    let io = HostIo::default();
    let mut refused = Vec::new();
    let mut applied = Vec::new();
    for (key, raw) in values {
        match renderer::options::find_host(options::HOST_OPTIONS, key) {
            Some(spec) if (spec.set)(&io, raw).is_some() => applied.push(spec),
            _ => refused.push((*key).to_string()),
        }
    }
    for spec in applied {
        (spec.config_store)(render, &io);
    }
    refused
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
}

impl PatchValue {
    fn raw(&self) -> RawOptionValue<'_> {
        match self {
            Self::Null => RawOptionValue::Null,
            Self::Str(s) => RawOptionValue::Str(s),
            Self::Number(n) => RawOptionValue::Number(*n),
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
    out
}

/// `/control/config/input` as a batch of this host's options.
fn input_patch_values(patch: InputConfigPatch) -> Vec<(&'static str, PatchValue)> {
    let mut out = Vec::new();
    push_some(&mut out, "input_mode", patch.mode, |mode| {
        PatchValue::Str(input_mode_name(mode).to_string())
    });
    if let Some(live) = patch.live_input {
        push_nullable(&mut out, "live_input_node", live.node, PatchValue::Str);
        push_nullable(
            &mut out,
            "live_input_description",
            live.description,
            PatchValue::Str,
        );
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

/// A handler of [`HOST_COMMANDS`]; `None` passes the message on.
type HostHandler = fn(&HostAudio, &OscMessage) -> Option<ControlEffects>;

/// This host's control addresses that are not one option's alias (see
/// `runtime_control::command_table`): the JSON patches and the group
/// applies (aliases of option batches), the device refresh, the structured
/// input layout, the resampling hold and reset. The per-option aliases are
/// looked up after it.
static HOST_COMMANDS: &[Command<HostHandler>] = &[
    Command::exact(osc_contract::CONTROL_CONFIG_AUDIO, config_audio),
    Command::exact(osc_contract::CONTROL_CONFIG_INPUT, config_input),
    Command::exact(osc_contract::CONTROL_CONFIG_AUDIO_APPLY, config_audio_apply),
    Command::exact(
        osc_contract::CONTROL_AUDIO_OUTPUT_DEVICES_REFRESH,
        audio_output_devices_refresh,
    ),
    Command::any(
        &[
            osc_contract::CONTROL_CONFIG_INPUT_APPLY,
            osc_contract::CONTROL_INPUT_APPLY,
        ],
        |host, _| host.apply_option_group(options::LIVE_INPUT.key),
    ),
];

impl HostControlHandler for HostAudio {
    fn capabilities(&self) -> runtime_control::HostCapabilities {
        runtime_control::HostCapabilities { input: true }
    }

    fn handle(&self, addr: &str, msg: &OscMessage) -> Option<ControlEffects> {
        if let Some(run) = command_table::find(HOST_COMMANDS, addr) {
            return run(self, msg);
        }

        // ── The dedicated per-option addresses: aliases of the rows ──
        let key = renderer::options::find_host_by_legacy_addr(self.rows(), addr)
            .map(|spec| spec.key)
            .or_else(|| {
                options::EXTRA_ALIASES
                    .iter()
                    .find(|(alias, _)| *alias == addr)
                    .map(|(_, key)| *key)
            })?;
        let effects = ControlEffects::default();
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
        Some(batch_effects(&batch, format!("OSC: {key} staged")))
    }

    fn extend_snapshot(&self) -> Vec<OscPacket> {
        let audio = &self.audio;
        let input = &self.input;
        let mut messages = Vec::with_capacity(2);

        // /state/audio: the output device, its format and the latency target.
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_AUDIO.to_string(),
            args: vec![OscType::String(build_audio_state_json(audio))],
        }));

        // /state/input: live-input device state. DRC fields (drcMode/drcWeight/
        // supportedDrcModes) are owned by the core (decode-stage) and emitted by
        // the core's build_live_state_bundle in a separate /state/input message;
        // Studio's InputDomainState parser (omniphony-studio-core, osc/apply.rs)
        // merges partial payloads, so two /state/input messages in one bundle
        // compose cleanly.
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
                        "node": requested.node_name.clone(),
                        "description": requested.node_description.clone()
                    },
                    "applied": {
                        "backend": applied.backend.map(|_| "pipewire"),
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
            + self
                .output_generation
                .load(std::sync::atomic::Ordering::Relaxed)
    }

    fn option_kind(&self, key: &str) -> Option<OptionKind> {
        renderer::options::find_host(self.rows(), key).map(|spec| spec.kind)
    }

    fn apply_options(&self, items: &[(&str, RawOptionValue)]) -> HostBatchApplied {
        let batch = renderer::options::host_apply_batch(&self.io, self.rows(), items);
        let staged = items.iter().zip(&batch.results).any(|((key, _), result)| {
            result.as_ref().is_some_and(|applied| applied.changed)
                && renderer::options::find_host(self.rows(), key)
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
        renderer::options::host_schema_entries(self.rows())
    }

    fn options_json(&self) -> serde_json::Map<String, serde_json::Value> {
        renderer::options::host_options_json(&self.io, self.rows())
    }

    fn options_applied_json(&self) -> serde_json::Map<String, serde_json::Value> {
        renderer::options::host_applied_json(&self.io, self.rows())
    }

    fn option_groups_pending(&self) -> Vec<(&'static str, bool)> {
        vec![(
            options::LIVE_INPUT.key,
            self.input_staged.load(std::sync::atomic::Ordering::Relaxed),
        )]
    }

    fn amend_saved_config(&self, render: &mut renderer::config::RenderConfig) {
        // Every declared option (audio output, latency loop, live input).
        renderer::options::host_store_to_config(render, &self.io, self.rows());
    }
}

/// ── The JSON patches: aliases of a batch of this host's options ──
fn config_audio(host: &HostAudio, msg: &OscMessage) -> Option<ControlEffects> {
    let effects = ControlEffects::default();
    let Some(patch) = parse_json_string_arg::<AudioConfigPatch>(msg.args.first()) else {
        return Some(effects);
    };
    let values = audio_patch_values(patch);
    Some(host.apply_patch(&values, "OSC: audio config staged"))
}

fn config_input(host: &HostAudio, msg: &OscMessage) -> Option<ControlEffects> {
    let effects = ControlEffects::default();
    let Some(patch) = parse_json_string_arg::<InputConfigPatch>(msg.args.first()) else {
        return Some(effects);
    };
    let values = input_patch_values(patch);
    Some(host.apply_patch(&values, "OSC: input config staged"))
}

/// ── The per-domain apply addresses: aliases of the group apply ──
fn config_audio_apply(host: &HostAudio, _msg: &OscMessage) -> Option<ControlEffects> {
    host.apply_option_group(options::AUDIO_OUTPUT.key)
}

/// ── Not options ──
fn audio_output_devices_refresh(host: &HostAudio, _msg: &OscMessage) -> Option<ControlEffects> {
    let audio = &host.audio;
    let mut effects = ControlEffects::default();
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
    Some(effects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer::options::{GroupMode, LegacyAddr};

    fn fixture_control() -> Arc<RendererControl> {
        use renderer::live_params::LiveEvaluationMode;
        use renderer::spatial_renderer::{RendererSpec, SpatialRenderer};
        use renderer::spatial_vbap::{DistanceModel, VbapTableMode};
        let layout = renderer::speaker_layout::SpeakerLayout::preset("7.1.4").expect("preset");
        SpatialRenderer::new(RendererSpec {
            spread_resolution: 0.25,
            table_mode: VbapTableMode::Cartesian {
                x_size: 5,
                y_size: 5,
                z_size: 3,
                z_neg_size: 3,
            },
            distance_model: DistanceModel::None,
            room_ratio: [1.0, 2.0, 1.0],
            room_ratio_center_blend: 0.5,
            initial_evaluation_mode: LiveEvaluationMode::Auto,
            cartesian_default_x_size: 5,
            cartesian_default_y_size: 5,
            cartesian_default_z_size: 3,
            cartesian_default_z_neg_size: 3,
            ..renderer::test_support::spec(layout)
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

    #[test]
    fn the_host_table_is_declared_in_the_contract_and_claimed_once() {
        let core = runtime_control::command_table::core_addresses;
        let aliases = || -> Vec<&'static str> {
            options::HOST_OPTIONS
                .iter()
                .filter_map(|spec| match spec.legacy_control_addr {
                    renderer::options::LegacyAddr::Exact(addr) => Some(addr),
                    _ => None,
                })
                .chain(options::EXTRA_ALIASES.iter().map(|(alias, _)| *alias))
                .collect()
        };
        let found = runtime_control::command_table::problems(HOST_COMMANDS, &[&core, &aliases]);
        assert!(found.is_empty(), "{found:#?}");
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

    /// The rows are the render host's: the output, the latency loop, and the
    /// part of the input it reconfigures. What the engine before it tuned
    /// (the adaptive resampler, the live input's clock and shape) has no row:
    /// it is not in the schema and a write of it is refused.
    #[test]
    fn the_rows_are_the_render_hosts() {
        let keys: Vec<&str> = options::HOST_OPTIONS.iter().map(|spec| spec.key).collect();
        assert_eq!(
            keys,
            [
                "output_device",
                "output_backend",
                "output_file",
                "output_file_format",
                "output_sample_rate",
                "latency_target",
                "loop_bandwidth_hz",
                "source_clock",
                "realign_threshold_ms",
                "input_mode",
                "live_input_node",
                "live_input_description"
            ]
        );
        let host = host();
        assert_eq!(host.options_schema().len(), keys.len());
        assert!(host.capabilities().input);
        for gone in [
            "adaptive_resampling_kp_near",
            "enable_adaptive_resampling",
            "live_input_clock_mode",
            "live_input_lfe_mode",
        ] {
            assert!(host.option_kind(gone).is_none(), "{gone}");
            let refused = host.apply_options(&[(gone, RawOptionValue::Number(1.0))]);
            assert!(refused.results.iter().all(Option::is_none), "{gone}");
        }
    }

    /// `/control/config/audio` is a batch of the rows; what an older client
    /// still sends of the adaptive resampler is ignored, not an error.
    #[test]
    fn the_audio_patch_is_a_batch_of_the_rows() {
        let host = host();
        let patch = r#"{"outputDevice":" hw:2 ","sampleRate":0,"latencyTargetMs":80,
            "adaptiveResampling":{"enabled":true,"kpNear":2.0}}"#;
        let effects = host
            .handle(osc_contract::CONTROL_CONFIG_AUDIO, &msg("", vec![s(patch)]))
            .expect("a host address");
        assert!(effects.mark_dirty, "a changed value lights Save");
        let requested = host.audio.requested_snapshot();
        assert_eq!(requested.output_device.as_deref(), Some("hw:2"));
        assert_eq!(requested.output_sample_rate_hz, None, "0 unsets the rate");
        assert_eq!(requested.latency_target_ms, Some(80));
    }

    /// The live input is staged: a patch (with the fields of an older client
    /// left out) changes what is asked for, and the apply is a command.
    #[test]
    fn the_live_input_is_staged_and_applied_on_command() {
        let host = host();
        let pending = |host: &HostAudio| {
            host.option_groups_pending()
                .into_iter()
                .any(|(group, pending)| group == options::LIVE_INPUT.key && pending)
        };
        let patch = r#"{"mode":"pipewire","liveInput":{"backend":"asio","node":" in ",
            "clockMode":"dac","channels":2,"sampleRate":null,"lfeMode":"object"}}"#;
        host.handle(osc_contract::CONTROL_CONFIG_INPUT, &msg("", vec![s(patch)]))
            .expect("a host address");
        let requested = host.input.requested_snapshot();
        assert_eq!(requested.mode, InputMode::Pipewire);
        assert_eq!(requested.node_name.as_deref(), Some("in"));
        assert!(pending(&host), "staged values wait for their apply");
        assert!(!host.input.is_apply_pending());

        host.handle(osc_contract::CONTROL_INPUT_APPLY, &msg("", vec![]))
            .expect("the apply");
        assert!(host.input.is_apply_pending());
        assert!(!pending(&host));
    }

    /// The dedicated addresses are aliases of the rows, with the rejections
    /// they always had; an address whose row is gone is not this host's.
    #[test]
    fn the_dedicated_addresses_are_aliases_of_the_rows() {
        let host = host();
        host.handle(
            osc_contract::CONTROL_LATENCY_TARGET,
            &msg("", vec![OscType::Int(120)]),
        )
        .expect("a host address");
        assert_eq!(host.audio.requested_snapshot().latency_target_ms, Some(120));
        // A non-positive target was ignored by the address, and still is.
        host.handle(
            osc_contract::CONTROL_LATENCY_TARGET,
            &msg("", vec![OscType::Int(0)]),
        )
        .expect("a host address");
        assert_eq!(host.audio.requested_snapshot().latency_target_ms, Some(120));

        host.handle(
            osc_contract::CONTROL_INPUT_LIVE_NODE,
            &msg("", vec![s(" den ")]),
        )
        .expect("a host address");
        assert_eq!(
            host.input.requested_snapshot().node_name.as_deref(),
            Some("den")
        );
        // An address of the regulation this host replaced is not one of its.
        let retired = osc_contract::CONTROL_LATENCY_TARGET
            .replace("latency_target", "adaptive_resampling/kp_near");
        assert!(
            host.handle(&retired, &msg("", vec![OscType::Float(2.0)]))
                .is_none()
        );
    }

    /// Save writes the rows and leaves the rest of the file alone: the keys
    /// of the engine before this one are kept as they were read.
    #[test]
    fn a_save_writes_the_rows_and_keeps_the_rest() {
        let host = host();
        host.apply_options(&[
            ("latency_target", RawOptionValue::Number(120.0)),
            ("realign_threshold_ms", RawOptionValue::Number(35.0)),
            ("input_mode", RawOptionValue::Str("pipewire")),
            ("live_input_node", RawOptionValue::Str("den")),
        ]);
        let mut render = renderer::config::RenderConfig {
            adaptive_resampling_kp_near: Some(3.5),
            live_input: Some(renderer::config::LiveInputConfig {
                clock_mode: Some(renderer::config::InputClockModeConfig::Dac),
                lfe_mode: Some(renderer::config::InputLfeModeConfig::Drop),
                ..Default::default()
            }),
            ..Default::default()
        };
        host.amend_saved_config(&mut render);
        assert_eq!(render.latency_target, Some(120));
        assert_eq!(render.realign_threshold_ms, Some(35.0));
        assert_eq!(
            render.input_mode,
            Some(renderer::config::InputModeConfig::Pipewire)
        );
        assert_eq!(render.adaptive_resampling_kp_near, Some(3.5));
        let live = render.live_input.expect("the live input");
        assert_eq!(live.node.as_deref(), Some("den"));
        assert!(live.clock_mode.is_some());
        assert_eq!(
            live.lfe_mode,
            Some(renderer::config::InputLfeModeConfig::Drop)
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
        let node = schema
            .iter()
            .find(|e| e["key"] == "live_input_node")
            .unwrap();
        assert_eq!(node["group"]["mode"], "staged");
    }
}
