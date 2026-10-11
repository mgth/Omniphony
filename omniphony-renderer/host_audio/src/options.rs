//! The options the standalone renderer's host declares: its audio output,
//! the loop that holds the output's latency and its live input
//! (`renderer::options::HostOptionSpec` rows over [`HostIo`], the output and input of [`crate::HostAudio`]).
//!
//! The engine publishes, sets and applies them like the core's options —
//! `/control/option(s)`, `/control/options/apply`, the schema,
//! `/state/host_options`, Save — through the option methods of
//! [`runtime_control::HostControlHandler`], each a one-line call to the
//! `renderer::options::host_*` helpers over [`HOST_OPTIONS`]. The dedicated
//! addresses and the `/control/config/{audio,input}` JSON patches are aliases
//! of these rows (see `lib.rs`). The embedded engine registers no host, so it
//! publishes none of them.
//!
//! The host seeds these values itself, at its bootstrap (the CLI's argument
//! resolution), so a row has no config seed; its store is the Save's.

use audio_input::InputMode;
use renderer::config::RenderConfig;
use renderer::options::{
    ApplyEffect, GroupMode, HostOptionSpec, LegacyAddr, OptionDefault, OptionFlags, OptionGroup,
    OptionKind, RawOptionValue, raw_float_if, raw_optional_int,
};
use runtime_control::osc_contract;
use serde_json::Value;

use crate::HostIo;

/// The audio output: device, backend, file sink, rate, latency target. The
/// host compares what is requested with what runs on every poll and
/// restarts the output as soon as they differ, so a write takes effect as
/// it arrives.
pub static AUDIO_OUTPUT: OptionGroup = OptionGroup {
    key: "audio_output",
    mode: GroupMode::Live,
    effect: ApplyEffect::RestartOutput,
    i18n_key: "section.audioOutput",
};

/// The live input: a write stages a requested value; the input restarts
/// with every staged value at once when the group is applied
/// (`/control/options/apply live_input`, or `/control/input/apply`).
pub static LIVE_INPUT: OptionGroup = OptionGroup {
    key: "live_input",
    mode: GroupMode::Staged,
    effect: ApplyEffect::RestartInput,
    i18n_key: "section.audioInput",
};

/// The latency loop (`docs/resampling-rework-plan.md`): the output takes a
/// new value at once, with a realign.
pub static SYNC: OptionGroup = OptionGroup {
    key: "sync",
    mode: GroupMode::Live,
    effect: ApplyEffect::RestartOutput,
    i18n_key: "sync.title",
};

/// This host's groups.
pub static HOST_GROUPS: &[&OptionGroup] = &[&AUDIO_OUTPUT, &SYNC, &LIVE_INPUT];

// ── Value helpers ───────────────────────────────────────────────────────

/// An optional string: `None` for a shape it does not take, `Some(None)` to
/// unset it (null, or blank), else the trimmed text.
fn raw_opt_string(raw: &RawOptionValue) -> Option<Option<String>> {
    match raw {
        RawOptionValue::Null => Some(None),
        RawOptionValue::Str(s) => {
            let trimmed = s.trim();
            Some((!trimmed.is_empty()).then(|| trimmed.to_string()))
        }
        _ => None,
    }
}

/// An optional string as the snapshot carries it: `""` when unset.
fn opt_string_json(value: &Option<String>) -> Value {
    value.as_deref().unwrap_or("").into()
}

/// An optional number as the snapshot carries it: `null` when unset.
fn opt_json<T: Into<Value> + Copy>(value: Option<T>) -> Value {
    value.map_or(Value::Null, Into::into)
}

fn positive(v: f32) -> bool {
    v > 0.0
}

fn non_negative(v: f32) -> bool {
    v >= 0.0
}

pub(crate) fn input_mode_from_str(value: &str) -> Option<InputMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "bridge" | "pipe_bridge" => Some(InputMode::Bridge),
        "pipewire" | "pipewire_bridge" | "live" => Some(InputMode::Pipewire),
        _ => None,
    }
}

const SAMPLE_RATE_KIND: OptionKind = OptionKind::OptionalInt {
    min: 1,
    max: 768_000,
};
const LATENCY_TARGET_KIND: OptionKind = OptionKind::OptionalInt {
    min: 1,
    max: 10_000,
};
const LOOP_BANDWIDTH_KIND: OptionKind = OptionKind::Float {
    min: 0.0,
    max: 1.0,
    step: 0.001,
};
const REALIGN_THRESHOLD_KIND: OptionKind = OptionKind::Float {
    min: 1.0,
    max: 1000.0,
    step: 1.0,
};

/// An optional-string live-input row (unset when blank).
macro_rules! input_string {
    ($key:literal, $field:ident, $cfg:ident, $setter:ident, $i18n:literal, $help:expr, $legacy:expr, $applied:expr) => {
        HostOptionSpec {
            key: $key,
            kind: OptionKind::Str,
            default: OptionDefault::Str(""),
            flags: OptionFlags::NONE,
            group: Some(&LIVE_INPUT),
            i18n_key: $i18n,
            help_i18n_key: $help,
            legacy_control_addr: $legacy,
            set: |host, raw| {
                let value = raw_opt_string(raw)?;
                host.input.$setter(value.clone());
                Some(value.unwrap_or_default())
            },
            get_json: |host| opt_string_json(&host.input.requested_snapshot().$field),
            applied_json: $applied,
            config_store: |render, host| {
                live_input_cfg(render).$cfg = host.input.requested_snapshot().$field;
            },
        }
    };
}

fn live_input_cfg(render: &mut RenderConfig) -> &mut renderer::config::LiveInputConfig {
    render.live_input.get_or_insert_with(Default::default)
}

fn input_mode_name(mode: InputMode) -> &'static str {
    match mode {
        InputMode::Bridge => "pipe_bridge",
        InputMode::Pipewire => "pipewire",
    }
}

/// Every option this host declares.
pub static HOST_OPTIONS: &[HostOptionSpec<HostIo>] = &[
    // ── Audio output ────────────────────────────────────────────────────
    HostOptionSpec {
        key: "output_device",
        kind: OptionKind::Str,
        default: OptionDefault::Str(""),
        flags: OptionFlags::NONE,
        group: Some(&AUDIO_OUTPUT),
        i18n_key: "audio.outputDevice",
        help_i18n_key: Some("help.audio.outputDevice"),
        legacy_control_addr: LegacyAddr::Exact(osc_contract::CONTROL_AUDIO_OUTPUT_DEVICE),
        // Blank or null: the backend's default device.
        set: |host, raw| {
            let value = raw_opt_string(raw)?;
            host.audio.set_requested_output_device(value.clone());
            Some(value.unwrap_or_default())
        },
        get_json: |host| opt_string_json(&host.audio.requested_snapshot().output_device),
        applied_json: None,
        config_store: |render, host| {
            render.output_device = host.audio.requested_snapshot().output_device;
        },
    },
    HostOptionSpec {
        key: "output_backend",
        kind: OptionKind::Str,
        default: OptionDefault::Str(""),
        flags: OptionFlags::NONE,
        group: Some(&AUDIO_OUTPUT),
        i18n_key: "audio.outputBackend",
        help_i18n_key: Some("help.audio.outputBackend"),
        legacy_control_addr: LegacyAddr::Exact(osc_contract::CONTROL_AUDIO_OUTPUT_BACKEND),
        // Blank or null: keep the active backend.
        set: |host, raw| {
            let value = raw_opt_string(raw)?;
            host.audio.set_requested_output_backend(value.clone());
            Some(value.unwrap_or_default())
        },
        get_json: |host| opt_string_json(&host.audio.requested_snapshot().output_backend),
        applied_json: None,
        // Only a live request replaces what the file holds (the CLI resolves
        // the backend at launch and keeps it in its runtime).
        config_store: |render, host| {
            if let Some(backend) = host.audio.requested_snapshot().output_backend {
                render.output_backend = Some(backend);
            }
        },
    },
    HostOptionSpec {
        key: "output_file",
        kind: OptionKind::Str,
        default: OptionDefault::Str(""),
        flags: OptionFlags::NONE,
        group: Some(&AUDIO_OUTPUT),
        i18n_key: "audio.outputFile",
        help_i18n_key: Some("help.audio.outputFile"),
        legacy_control_addr: LegacyAddr::Exact(osc_contract::CONTROL_AUDIO_OUTPUT_FILE),
        set: |host, raw| {
            let value = raw_opt_string(raw)?;
            host.audio.set_requested_output_file(value.clone());
            Some(value.unwrap_or_default())
        },
        get_json: |host| opt_string_json(&host.audio.requested_snapshot().output_file),
        applied_json: None,
        // `-` (stdout) is the default and leaves the file.
        config_store: |render, host| {
            if let Some(file) = host.audio.requested_snapshot().output_file {
                render.output_file = (file != "-").then_some(file);
            }
        },
    },
    HostOptionSpec {
        key: "output_file_format",
        kind: OptionKind::Str,
        default: OptionDefault::Str(""),
        flags: OptionFlags::NONE,
        group: Some(&AUDIO_OUTPUT),
        i18n_key: "audio.outputFileFormat",
        help_i18n_key: Some("help.audio.outputFileFormat"),
        legacy_control_addr: LegacyAddr::Exact(osc_contract::CONTROL_AUDIO_OUTPUT_FILE_FORMAT),
        set: |host, raw| {
            let value = raw_opt_string(raw)?;
            host.audio.set_requested_output_file_format(value.clone());
            Some(value.unwrap_or_default())
        },
        get_json: |host| opt_string_json(&host.audio.requested_snapshot().output_file_format),
        applied_json: None,
        // Raw samples are the default and leave the file.
        config_store: |render, host| {
            if let Some(format) = host.audio.requested_snapshot().output_file_format {
                let raw = matches!(format.as_str(), "raw_f32" | "rawf32" | "raw" | "f32");
                render.output_file_format = (!raw).then_some(format);
            }
        },
    },
    HostOptionSpec {
        key: "output_sample_rate",
        kind: SAMPLE_RATE_KIND,
        default: OptionDefault::Unset,
        flags: OptionFlags::NONE,
        group: Some(&AUDIO_OUTPUT),
        i18n_key: "audio.sampleRate",
        help_i18n_key: Some("help.audio.sampleRate"),
        legacy_control_addr: LegacyAddr::Exact(osc_contract::CONTROL_AUDIO_SAMPLE_RATE),
        // Unset (null, or 0): the device's rate.
        set: |host, raw| {
            let value = raw_optional_int(raw, SAMPLE_RATE_KIND)?.map(|hz| hz as u32);
            host.audio.set_requested_output_sample_rate(value);
            Some(value.map_or_else(String::new, |hz| hz.to_string()))
        },
        get_json: |host| opt_json(host.audio.requested_snapshot().output_sample_rate_hz),
        applied_json: None,
        config_store: |render, host| {
            render.output_sample_rate = host.audio.requested_snapshot().output_sample_rate_hz;
        },
    },
    HostOptionSpec {
        key: "latency_target",
        kind: LATENCY_TARGET_KIND,
        default: OptionDefault::Unset,
        flags: OptionFlags::NONE,
        group: Some(&AUDIO_OUTPUT),
        i18n_key: "audio.targetLatency",
        help_i18n_key: Some("help.audio.targetLatency"),
        legacy_control_addr: LegacyAddr::Exact(osc_contract::CONTROL_LATENCY_TARGET),
        set: |host, raw| {
            let value = raw_optional_int(raw, LATENCY_TARGET_KIND)?.map(|ms| ms as u32);
            host.audio.set_requested_latency_target_ms(value);
            Some(value.map_or_else(String::new, |ms| ms.to_string()))
        },
        get_json: |host| opt_json(host.audio.requested_snapshot().latency_target_ms),
        applied_json: None,
        config_store: |render, host| {
            render.latency_target = host.audio.requested_snapshot().latency_target_ms;
        },
    },
    // ── Sync engine ─────────────────────────────────────────────────────
    HostOptionSpec {
        key: "loop_bandwidth_hz",
        kind: LOOP_BANDWIDTH_KIND,
        default: OptionDefault::Float(0.0),
        flags: OptionFlags::NONE,
        group: Some(&SYNC),
        i18n_key: "sync.loopBandwidth",
        help_i18n_key: Some("help.sync.loopBandwidth"),
        legacy_control_addr: LegacyAddr::None,
        // 0: the source's own default.
        set: |host, raw| {
            let value = raw_float_if(raw, LOOP_BANDWIDTH_KIND, non_negative)?;
            host.audio.set_requested_loop_bandwidth_hz(value);
            Some(format!("{value}"))
        },
        get_json: |host| host.audio.requested_snapshot().loop_bandwidth_hz.into(),
        applied_json: None,
        config_store: |render, host| {
            let value = host.audio.requested_snapshot().loop_bandwidth_hz;
            render.loop_bandwidth_hz = (value > 0.0).then_some(value);
        },
    },
    HostOptionSpec {
        key: "source_clock",
        kind: OptionKind::Enum(&audio_output::control::SourceClock::NAMES),
        default: OptionDefault::Str("auto"),
        flags: OptionFlags::NONE,
        group: Some(&SYNC),
        i18n_key: "sync.sourceClock",
        help_i18n_key: Some("help.sync.sourceClock"),
        legacy_control_addr: LegacyAddr::None,
        set: |host, raw| {
            let RawOptionValue::Str(value) = raw else {
                return None;
            };
            let clock = audio_output::control::SourceClock::from_name(value)?;
            host.audio.set_requested_source_clock(clock);
            Some(clock.as_str().to_string())
        },
        get_json: |host| host.audio.requested_snapshot().source_clock.as_str().into(),
        applied_json: None,
        // The default leaves the file.
        config_store: |render, host| {
            let clock = host.audio.requested_snapshot().source_clock;
            render.source_clock = (clock != audio_output::control::SourceClock::Auto)
                .then(|| clock.as_str().to_string());
        },
    },
    HostOptionSpec {
        key: "realign_threshold_ms",
        kind: REALIGN_THRESHOLD_KIND,
        default: OptionDefault::Float(audio_output::control::DEFAULT_REALIGN_THRESHOLD_MS),
        flags: OptionFlags::NONE,
        group: Some(&SYNC),
        i18n_key: "sync.realignThreshold",
        help_i18n_key: Some("help.sync.realignThreshold"),
        legacy_control_addr: LegacyAddr::None,
        set: |host, raw| {
            let value = raw_float_if(raw, REALIGN_THRESHOLD_KIND, positive)?;
            host.audio.set_requested_realign_threshold_ms(value);
            Some(format!("{value}"))
        },
        get_json: |host| host.audio.requested_snapshot().realign_threshold_ms.into(),
        applied_json: None,
        // The default leaves the file.
        config_store: |render, host| {
            let value = host.audio.requested_snapshot().realign_threshold_ms;
            render.realign_threshold_ms =
                (value != audio_output::control::DEFAULT_REALIGN_THRESHOLD_MS).then_some(value);
        },
    },
    // ── Live input (staged) ─────────────────────────────────────────────
    HostOptionSpec {
        key: "input_mode",
        kind: OptionKind::Enum(&["pipe_bridge", "pipewire"]),
        default: OptionDefault::Str("pipe_bridge"),
        flags: OptionFlags::NONE,
        group: Some(&LIVE_INPUT),
        i18n_key: "input.mode",
        help_i18n_key: Some("help.input.mode"),
        legacy_control_addr: LegacyAddr::Exact(osc_contract::CONTROL_INPUT_MODE),
        set: |host, raw| {
            let RawOptionValue::Str(value) = raw else {
                return None;
            };
            let mode = input_mode_from_str(value)?;
            host.input.set_requested_mode(mode);
            Some(input_mode_name(mode).to_string())
        },
        get_json: |host| input_mode_name(host.input.requested_snapshot().mode).into(),
        applied_json: Some(|host| {
            input_mode_name(host.input.applied_snapshot().active_mode).into()
        }),
        config_store: |render, host| {
            render.input_mode = Some(match host.input.requested_snapshot().mode {
                InputMode::Bridge => renderer::config::InputModeConfig::Bridge,
                InputMode::Pipewire => renderer::config::InputModeConfig::Pipewire,
            });
        },
    },
    input_string!(
        "live_input_node",
        node_name,
        node,
        set_requested_node_name,
        "input.node",
        Some("help.input.node"),
        LegacyAddr::Exact(osc_contract::CONTROL_INPUT_LIVE_NODE),
        Some(|host| opt_string_json(&host.input.applied_snapshot().node_name))
    ),
    input_string!(
        "live_input_description",
        node_description,
        description,
        set_requested_node_description,
        "input.description",
        Some("help.input.description"),
        LegacyAddr::Exact(osc_contract::CONTROL_INPUT_LIVE_DESCRIPTION),
        Some(|host| opt_string_json(&host.input.applied_snapshot().node_description))
    ),
];

/// Pre-registry addresses that are further aliases of a row (the row names
/// one address; these older spellings map to the same key). None today.
pub(crate) static EXTRA_ALIASES: &[(&str, &str)] = &[];

/// Values the pre-registry address ignored where its row takes them: a
/// non-positive latency target (the row reads it as "unset", as the JSON
/// patch always did).
pub(crate) fn legacy_ignores(addr: &str, raw: &RawOptionValue) -> bool {
    matches!(raw, RawOptionValue::Number(n) if *n <= 0.0)
        && addr == osc_contract::CONTROL_LATENCY_TARGET
}
