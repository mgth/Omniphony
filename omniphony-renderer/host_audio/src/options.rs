//! The options the standalone renderer's host declares: its audio output,
//! the output's adaptive resampler and its live input
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

use std::path::PathBuf;

use audio_input::{InputBackend, InputClockMode, InputLfeMode, InputMapMode, InputMode};
use renderer::config::RenderConfig;
use renderer::options::{
    ApplyEffect, GroupMode, HostOptionSpec, LegacyAddr, OptionDefault, OptionFlags, OptionGroup,
    OptionKind, RawOptionValue, raw_bool, raw_float_if, raw_int, raw_optional_int,
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

/// The output's adaptive resampler: its tuning is picked up by the running
/// resampler as it arrives.
pub static ADAPTIVE_RESAMPLING: OptionGroup = OptionGroup {
    key: "adaptive_resampling",
    mode: GroupMode::Live,
    effect: ApplyEffect::None,
    i18n_key: "adaptive.title",
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

/// This host's groups.
pub static HOST_GROUPS: &[&OptionGroup] = &[&AUDIO_OUTPUT, &ADAPTIVE_RESAMPLING, &LIVE_INPUT];

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

fn any_value(_: f32) -> bool {
    true
}

pub(crate) fn input_mode_from_str(value: &str) -> Option<InputMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "bridge" | "pipe_bridge" => Some(InputMode::Bridge),
        "pipewire" | "pipewire_bridge" | "live" => Some(InputMode::Pipewire),
        _ => None,
    }
}

pub(crate) fn clock_mode_from_str(value: &str) -> Option<InputClockMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "dac" => Some(InputClockMode::Dac),
        "pipewire" => Some(InputClockMode::Pipewire),
        "upstream" => Some(InputClockMode::Upstream),
        _ => None,
    }
}

pub(crate) fn lfe_mode_from_str(value: &str) -> Option<InputLfeMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "object" => Some(InputLfeMode::Object),
        "direct" => Some(InputLfeMode::Direct),
        "drop" => Some(InputLfeMode::Drop),
        _ => None,
    }
}

pub(crate) fn map_mode_from_str(value: &str) -> Option<InputMapMode> {
    match value.trim().to_ascii_lowercase().as_str() {
        "7.1-fixed" => Some(InputMapMode::SevenOneFixed),
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
const INPUT_CHANNELS_KIND: OptionKind = OptionKind::OptionalInt { min: 1, max: 64 };
const MS_KIND: OptionKind = OptionKind::Int {
    min: 0,
    max: u32::MAX as i64,
};
const POSITIVE_COUNT_KIND: OptionKind = OptionKind::Int {
    min: 1,
    max: u32::MAX as i64,
};
const SMOOTHING_ORDER_KIND: OptionKind = OptionKind::Int { min: 1, max: 2 };
const GAIN_KIND: OptionKind = OptionKind::Float {
    min: 0.0,
    max: 1_000_000.0,
    step: 0.01,
};
const UNIT_KIND: OptionKind = OptionKind::Float {
    min: 0.0,
    max: 1.0,
    step: 0.01,
};
const MARGIN_MS_KIND: OptionKind = OptionKind::Float {
    min: 0.0,
    max: 1_000_000.0,
    step: 0.1,
};
const CUTOFF_HZ_KIND: OptionKind = OptionKind::Float {
    min: 0.001,
    max: 1000.0,
    step: 0.001,
};

/// A boolean adaptive-resampling row.
macro_rules! adaptive_bool {
    ($key:ident, $field:ident, $setter:ident, $default:expr, $i18n:literal, $help:expr, $legacy:expr) => {
        HostOptionSpec {
            key: stringify!($key),
            kind: OptionKind::Bool,
            default: OptionDefault::Bool($default),
            flags: OptionFlags::NONE,
            group: Some(&ADAPTIVE_RESAMPLING),
            i18n_key: $i18n,
            help_i18n_key: $help,
            legacy_control_addr: $legacy,
            set: |host, raw| {
                let value = raw_bool(raw)?;
                host.audio.$setter(value);
                Some(if value { "1" } else { "0" }.to_string())
            },
            get_json: |host| host.audio.requested_snapshot().adaptive.$field.into(),
            applied_json: None,
            config_store: |render, host| {
                render.$key = Some(host.audio.requested_snapshot().adaptive.$field);
            },
        }
    };
}

/// A float adaptive-resampling row: accepted when `accept` holds, then
/// clamped to `kind` (the setter may bound it further).
macro_rules! adaptive_float {
    ($key:ident, $field:ident, $setter:ident, $kind:expr, $accept:expr, $default:expr, $i18n:literal, $help:expr, $legacy:expr) => {
        HostOptionSpec {
            key: stringify!($key),
            kind: $kind,
            default: OptionDefault::Float($default),
            flags: OptionFlags::NONE,
            group: Some(&ADAPTIVE_RESAMPLING),
            i18n_key: $i18n,
            help_i18n_key: $help,
            legacy_control_addr: $legacy,
            set: |host, raw| {
                let value = raw_float_if(raw, $kind, $accept)?;
                host.audio.$setter(value);
                Some(format!("{value}"))
            },
            get_json: |host| host.audio.requested_snapshot().adaptive.$field.into(),
            applied_json: None,
            config_store: |render, host| {
                render.$key = Some(host.audio.requested_snapshot().adaptive.$field as f32);
            },
        }
    };
}

/// An unsigned-integer adaptive-resampling row.
macro_rules! adaptive_u32 {
    ($key:ident, $field:ident, $setter:ident, $kind:expr, $default:expr, $i18n:literal, $help:expr, $legacy:expr) => {
        HostOptionSpec {
            key: stringify!($key),
            kind: $kind,
            default: OptionDefault::Int($default),
            flags: OptionFlags::NONE,
            group: Some(&ADAPTIVE_RESAMPLING),
            i18n_key: $i18n,
            help_i18n_key: $help,
            legacy_control_addr: $legacy,
            set: |host, raw| {
                let value = raw_int(raw, $kind)? as u32;
                host.audio.$setter(value);
                Some(value.to_string())
            },
            get_json: |host| host.audio.requested_snapshot().adaptive.$field.into(),
            applied_json: None,
            config_store: |render, host| {
                render.$key = Some(host.audio.requested_snapshot().adaptive.$field);
            },
        }
    };
}

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

fn input_backend_name(backend: Option<InputBackend>) -> &'static str {
    match backend {
        Some(InputBackend::Pipewire) => "pipewire",
        None => "",
    }
}

fn clock_mode_name(mode: InputClockMode) -> &'static str {
    match mode {
        InputClockMode::Dac => "dac",
        InputClockMode::Pipewire => "pipewire",
        InputClockMode::Upstream => "upstream",
    }
}

fn lfe_mode_name(mode: InputLfeMode) -> &'static str {
    match mode {
        InputLfeMode::Object => "object",
        InputLfeMode::Direct => "direct",
        InputLfeMode::Drop => "drop",
    }
}

fn map_mode_name(mode: InputMapMode) -> &'static str {
    match mode {
        InputMapMode::SevenOneFixed => "7.1-fixed",
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
    // ── Adaptive resampling ─────────────────────────────────────────────
    HostOptionSpec {
        key: "enable_adaptive_resampling",
        kind: OptionKind::Bool,
        default: OptionDefault::Bool(false),
        flags: OptionFlags::NONE,
        group: Some(&ADAPTIVE_RESAMPLING),
        i18n_key: "adaptive.title",
        help_i18n_key: Some("help.adaptive.title"),
        legacy_control_addr: LegacyAddr::Exact(osc_contract::CONTROL_ADAPTIVE_RESAMPLING),
        set: |host, raw| {
            let value = raw_bool(raw)?;
            host.audio.set_requested_adaptive_resampling(value);
            Some(if value { "1" } else { "0" }.to_string())
        },
        get_json: |host| host.audio.requested_snapshot().adaptive_enabled.into(),
        applied_json: None,
        config_store: |render, host| {
            renderer::config_fields::enable_adaptive_resampling::store(
                render,
                host.audio.requested_snapshot().adaptive_enabled,
            )
        },
    },
    adaptive_bool!(
        adaptive_resampling_enable_far_mode,
        enable_far_mode,
        set_requested_adaptive_resampling_enable_far_mode,
        true,
        "adaptive.enableFarMode",
        None,
        LegacyAddr::Exact(osc_contract::CONTROL_ADAPTIVE_RESAMPLING_ENABLE_FAR_MODE)
    ),
    adaptive_bool!(
        adaptive_resampling_force_silence_in_far_mode,
        force_silence_in_far_mode,
        set_requested_adaptive_resampling_force_silence_in_far_mode,
        true,
        "adaptive.silenceFar",
        Some("help.adaptive.silenceFar"),
        LegacyAddr::Exact(osc_contract::CONTROL_ADAPTIVE_RESAMPLING_FORCE_SILENCE_IN_FAR_MODE)
    ),
    adaptive_bool!(
        adaptive_resampling_hard_recover_high_in_far_mode,
        hard_recover_high_in_far_mode,
        set_requested_adaptive_resampling_hard_recover_high_in_far_mode,
        true,
        "adaptive.hardRecoverHigh",
        Some("help.adaptive.hardRecoverHigh"),
        LegacyAddr::Exact(osc_contract::CONTROL_ADAPTIVE_RESAMPLING_HARD_RECOVER_HIGH_IN_FAR_MODE)
    ),
    adaptive_bool!(
        adaptive_resampling_hard_recover_low_in_far_mode,
        hard_recover_low_in_far_mode,
        set_requested_adaptive_resampling_hard_recover_low_in_far_mode,
        false,
        "adaptive.hardRecoverLow",
        Some("help.adaptive.hardRecoverLow"),
        LegacyAddr::Exact(osc_contract::CONTROL_ADAPTIVE_RESAMPLING_HARD_RECOVER_LOW_IN_FAR_MODE)
    ),
    adaptive_u32!(
        adaptive_resampling_far_mode_return_fade_in_ms,
        far_mode_return_fade_in_ms,
        set_requested_adaptive_resampling_far_mode_return_fade_in_ms,
        MS_KIND,
        500,
        "adaptive.fadeNearReturn",
        Some("help.adaptive.fadeNearReturn"),
        LegacyAddr::Exact(osc_contract::CONTROL_ADAPTIVE_RESAMPLING_FAR_MODE_RETURN_FADE_IN_MS)
    ),
    adaptive_float!(
        adaptive_resampling_kp_near,
        kp_near,
        set_requested_adaptive_resampling_kp_near,
        GAIN_KIND,
        positive,
        1.0,
        "adaptive.kpNear",
        Some("help.adaptive.kpNear"),
        LegacyAddr::Exact(osc_contract::CONTROL_ADAPTIVE_RESAMPLING_KP_NEAR)
    ),
    adaptive_float!(
        adaptive_resampling_ki,
        ki,
        set_requested_adaptive_resampling_ki,
        GAIN_KIND,
        non_negative,
        1.0,
        "adaptive.ki",
        Some("help.adaptive.ki"),
        LegacyAddr::Exact(osc_contract::CONTROL_ADAPTIVE_RESAMPLING_KI)
    ),
    adaptive_float!(
        adaptive_resampling_integral_discharge_ratio,
        integral_discharge_ratio,
        set_requested_adaptive_resampling_integral_discharge_ratio,
        UNIT_KIND,
        any_value,
        0.25,
        "adaptive.integralDischarge",
        Some("help.adaptive.integralDischarge"),
        LegacyAddr::Exact(osc_contract::CONTROL_ADAPTIVE_RESAMPLING_INTEGRAL_DISCHARGE_RATIO)
    ),
    adaptive_float!(
        adaptive_resampling_max_adjust,
        max_adjust,
        set_requested_adaptive_resampling_max_adjust,
        GAIN_KIND,
        positive,
        0.01,
        "adaptive.max",
        Some("help.adaptive.max"),
        LegacyAddr::Exact(osc_contract::CONTROL_ADAPTIVE_RESAMPLING_MAX_ADJUST)
    ),
    adaptive_u32!(
        adaptive_resampling_update_interval_callbacks,
        update_interval_callbacks,
        set_requested_adaptive_resampling_update_interval_callbacks,
        POSITIVE_COUNT_KIND,
        1,
        "adaptive.updateInterval",
        Some("help.adaptive.updateInterval"),
        LegacyAddr::Exact(osc_contract::CONTROL_ADAPTIVE_RESAMPLING_UPDATE_INTERVAL_CALLBACKS)
    ),
    adaptive_u32!(
        adaptive_resampling_high_recover_entry_margin_ms,
        high_recover_entry_margin_ms,
        set_requested_adaptive_resampling_high_recover_entry_margin_ms,
        POSITIVE_COUNT_KIND,
        1000,
        "adaptive.threshold",
        Some("help.adaptive.threshold"),
        LegacyAddr::Exact(osc_contract::CONTROL_ADAPTIVE_RESAMPLING_HIGH_RECOVER_ENTRY_MARGIN_MS)
    ),
    adaptive_float!(
        adaptive_resampling_low_recover_settle_stable_ms,
        low_recover_settle_stable_ms,
        set_requested_adaptive_resampling_low_recover_settle_stable_ms,
        MARGIN_MS_KIND,
        non_negative,
        200.0,
        "adaptive.lowRecoverSettleStable",
        Some("help.adaptive.lowRecoverSettleStable"),
        LegacyAddr::None
    ),
    adaptive_float!(
        adaptive_resampling_low_recover_entry_margin_ms,
        low_recover_entry_margin_ms,
        set_requested_adaptive_resampling_low_recover_entry_margin_ms,
        MARGIN_MS_KIND,
        non_negative,
        18.0,
        "adaptive.lowRecoverEntryMargin",
        Some("help.adaptive.lowRecoverEntryMargin"),
        LegacyAddr::None
    ),
    adaptive_float!(
        adaptive_resampling_low_recover_exit_margin_ms,
        low_recover_exit_margin_ms,
        set_requested_adaptive_resampling_low_recover_exit_margin_ms,
        MARGIN_MS_KIND,
        non_negative,
        6.0,
        "adaptive.lowRecoverExitMargin",
        Some("help.adaptive.lowRecoverExitMargin"),
        LegacyAddr::None
    ),
    adaptive_float!(
        adaptive_resampling_low_recover_settle_margin_ms,
        low_recover_settle_margin_ms,
        set_requested_adaptive_resampling_low_recover_settle_margin_ms,
        MARGIN_MS_KIND,
        non_negative,
        6.0,
        "adaptive.lowRecoverSettleMargin",
        Some("help.adaptive.lowRecoverSettleMargin"),
        LegacyAddr::None
    ),
    adaptive_float!(
        adaptive_resampling_low_recover_refill_delta_alpha,
        low_recover_refill_delta_alpha,
        set_requested_adaptive_resampling_low_recover_refill_delta_alpha,
        UNIT_KIND,
        any_value,
        0.5,
        "adaptive.lowRecoverRefillDeltaAlpha",
        Some("help.adaptive.lowRecoverRefillDeltaAlpha"),
        LegacyAddr::None
    ),
    adaptive_float!(
        adaptive_resampling_control_smoothing_cutoff_hz,
        control_smoothing_cutoff_hz,
        set_requested_adaptive_resampling_control_smoothing_cutoff_hz,
        CUTOFF_HZ_KIND,
        any_value,
        0.5,
        "adaptive.controlSmoothingCutoffHz",
        Some("help.adaptive.controlSmoothingCutoffHz"),
        LegacyAddr::None
    ),
    adaptive_u32!(
        adaptive_resampling_control_smoothing_order,
        control_smoothing_order,
        set_requested_adaptive_resampling_control_smoothing_order,
        SMOOTHING_ORDER_KIND,
        1,
        "adaptive.controlSmoothingOrder",
        Some("help.adaptive.controlSmoothingOrder"),
        LegacyAddr::None
    ),
    adaptive_bool!(
        adaptive_resampling_use_pre_bridge_clock,
        use_pre_bridge_clock,
        set_requested_adaptive_resampling_use_pre_bridge_clock,
        false,
        "adaptive.usePreBridgeClock",
        Some("help.adaptive.usePreBridgeClock"),
        LegacyAddr::None
    ),
    adaptive_bool!(
        adaptive_resampling_use_output_pacing,
        use_output_pacing,
        set_requested_adaptive_resampling_use_output_pacing,
        false,
        "adaptive.useOutputPacing",
        Some("help.adaptive.useOutputPacing"),
        LegacyAddr::None
    ),
    adaptive_bool!(
        adaptive_resampling_disable_backpressure,
        disable_backpressure,
        set_requested_adaptive_resampling_disable_backpressure,
        false,
        "adaptive.disableBackpressure",
        Some("help.adaptive.disableBackpressure"),
        LegacyAddr::None
    ),
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
    HostOptionSpec {
        key: "live_input_backend",
        kind: OptionKind::Str,
        default: OptionDefault::Str(""),
        flags: OptionFlags::NONE,
        group: Some(&LIVE_INPUT),
        i18n_key: "input.backend",
        help_i18n_key: None,
        legacy_control_addr: LegacyAddr::Exact(osc_contract::CONTROL_INPUT_LIVE_BACKEND),
        // `pipewire`, or unset; anything else (the retired `asio`) refused.
        set: |host, raw| {
            match raw_opt_string(raw)? {
                None => host.input.set_requested_backend(None),
                Some(value) => {
                    if !crate::stage_live_input_backend(&host.input, &value) {
                        return None;
                    }
                }
            }
            Some(input_backend_name(host.input.requested_snapshot().backend).to_string())
        },
        get_json: |host| input_backend_name(host.input.requested_snapshot().backend).into(),
        applied_json: Some(|host| input_backend_name(host.input.applied_snapshot().backend).into()),
        config_store: |render, host| {
            live_input_cfg(render).backend =
                host.input
                    .requested_snapshot()
                    .backend
                    .map(|backend| match backend {
                        InputBackend::Pipewire => renderer::config::InputBackendConfig::Pipewire,
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
    HostOptionSpec {
        key: "live_input_layout",
        kind: OptionKind::Str,
        default: OptionDefault::Str(""),
        flags: OptionFlags::NONE,
        group: Some(&LIVE_INPUT),
        i18n_key: "input.layout",
        help_i18n_key: Some("help.input.layout"),
        legacy_control_addr: LegacyAddr::Exact(osc_contract::CONTROL_INPUT_LIVE_LAYOUT),
        // A layout file replaces an imported layout.
        set: |host, raw| {
            let value = raw_opt_string(raw)?;
            host.input
                .set_requested_layout_path(value.as_ref().map(PathBuf::from));
            host.input.set_requested_current_layout(None);
            Some(value.unwrap_or_default())
        },
        get_json: |host| {
            host.input
                .requested_snapshot()
                .layout_path
                .map(|path| path.display().to_string())
                .unwrap_or_default()
                .into()
        },
        applied_json: None,
        config_store: |render, host| {
            live_input_cfg(render).layout = host.input.requested_snapshot().layout_path;
        },
    },
    HostOptionSpec {
        key: "live_input_clock_mode",
        kind: OptionKind::Enum(&["dac", "pipewire", "upstream"]),
        default: OptionDefault::Str("dac"),
        flags: OptionFlags::NONE,
        group: Some(&LIVE_INPUT),
        i18n_key: "input.clock",
        help_i18n_key: None,
        legacy_control_addr: LegacyAddr::Exact(osc_contract::CONTROL_INPUT_LIVE_CLOCK_MODE),
        set: |host, raw| {
            let RawOptionValue::Str(value) = raw else {
                return None;
            };
            let mode = clock_mode_from_str(value)?;
            host.input.set_requested_clock_mode(mode);
            Some(clock_mode_name(mode).to_string())
        },
        get_json: |host| clock_mode_name(host.input.requested_snapshot().clock_mode).into(),
        applied_json: None,
        config_store: |render, host| {
            live_input_cfg(render).clock_mode =
                Some(match host.input.requested_snapshot().clock_mode {
                    InputClockMode::Dac => renderer::config::InputClockModeConfig::Dac,
                    InputClockMode::Pipewire => renderer::config::InputClockModeConfig::Pipewire,
                    InputClockMode::Upstream => renderer::config::InputClockModeConfig::Upstream,
                });
        },
    },
    HostOptionSpec {
        key: "live_input_channels",
        kind: INPUT_CHANNELS_KIND,
        default: OptionDefault::Unset,
        flags: OptionFlags::NONE,
        group: Some(&LIVE_INPUT),
        i18n_key: "input.channels",
        help_i18n_key: Some("help.input.channels"),
        legacy_control_addr: LegacyAddr::Exact(osc_contract::CONTROL_INPUT_LIVE_CHANNELS),
        set: |host, raw| {
            let value = raw_optional_int(raw, INPUT_CHANNELS_KIND)?.map(|n| n as u16);
            host.input.set_requested_channels(value);
            Some(value.map_or_else(String::new, |n| n.to_string()))
        },
        get_json: |host| opt_json(host.input.requested_snapshot().channels),
        applied_json: Some(|host| opt_json(host.input.applied_snapshot().channels)),
        config_store: |render, host| {
            live_input_cfg(render).channels = host.input.requested_snapshot().channels;
        },
    },
    HostOptionSpec {
        key: "live_input_sample_rate",
        kind: SAMPLE_RATE_KIND,
        default: OptionDefault::Unset,
        flags: OptionFlags::NONE,
        group: Some(&LIVE_INPUT),
        i18n_key: "input.sampleRate",
        help_i18n_key: Some("help.input.sampleRate"),
        legacy_control_addr: LegacyAddr::Exact(osc_contract::CONTROL_INPUT_LIVE_SAMPLE_RATE),
        set: |host, raw| {
            let value = raw_optional_int(raw, SAMPLE_RATE_KIND)?.map(|hz| hz as u32);
            host.input.set_requested_sample_rate_hz(value);
            Some(value.map_or_else(String::new, |hz| hz.to_string()))
        },
        get_json: |host| opt_json(host.input.requested_snapshot().sample_rate_hz),
        applied_json: Some(|host| opt_json(host.input.applied_snapshot().sample_rate_hz)),
        config_store: |render, host| {
            live_input_cfg(render).sample_rate = host.input.requested_snapshot().sample_rate_hz;
        },
    },
    HostOptionSpec {
        key: "live_input_map",
        // A single mapping today; a string so the schema's enum rule (two
        // values or more) does not force a fake second one.
        kind: OptionKind::Str,
        default: OptionDefault::Str("7.1-fixed"),
        flags: OptionFlags::NONE,
        group: Some(&LIVE_INPUT),
        i18n_key: "input.map",
        help_i18n_key: Some("help.input.map"),
        legacy_control_addr: LegacyAddr::Exact(osc_contract::CONTROL_INPUT_LIVE_MAP),
        set: |host, raw| {
            let RawOptionValue::Str(value) = raw else {
                return None;
            };
            let mode = map_mode_from_str(value)?;
            host.input.set_requested_map_mode(mode);
            Some(map_mode_name(mode).to_string())
        },
        get_json: |host| map_mode_name(host.input.requested_snapshot().map_mode).into(),
        applied_json: None,
        config_store: |render, host| {
            live_input_cfg(render).map = Some(match host.input.requested_snapshot().map_mode {
                InputMapMode::SevenOneFixed => renderer::config::InputMapModeConfig::SevenOneFixed,
            });
        },
    },
    HostOptionSpec {
        key: "live_input_lfe_mode",
        kind: OptionKind::Enum(&["object", "direct", "drop"]),
        default: OptionDefault::Str("direct"),
        flags: OptionFlags::NONE,
        group: Some(&LIVE_INPUT),
        i18n_key: "input.lfe",
        help_i18n_key: None,
        legacy_control_addr: LegacyAddr::Exact(osc_contract::CONTROL_INPUT_LIVE_LFE_MODE),
        set: |host, raw| {
            let RawOptionValue::Str(value) = raw else {
                return None;
            };
            let mode = lfe_mode_from_str(value)?;
            host.input.set_requested_lfe_mode(mode);
            Some(lfe_mode_name(mode).to_string())
        },
        get_json: |host| lfe_mode_name(host.input.requested_snapshot().lfe_mode).into(),
        applied_json: None,
        config_store: |render, host| {
            live_input_cfg(render).lfe_mode =
                Some(match host.input.requested_snapshot().lfe_mode {
                    InputLfeMode::Object => renderer::config::InputLfeModeConfig::Object,
                    InputLfeMode::Direct => renderer::config::InputLfeModeConfig::Direct,
                    InputLfeMode::Drop => renderer::config::InputLfeModeConfig::Drop,
                });
        },
    },
];

/// Pre-registry addresses that are further aliases of a row (the row names
/// one address; these older spellings map to the same key).
pub(crate) static EXTRA_ALIASES: &[(&str, &str)] = &[
    (
        osc_contract::CONTROL_ADAPTIVE_RESAMPLING_HARD_RECOVER_IN_FAR_MODE,
        "adaptive_resampling_hard_recover_high_in_far_mode",
    ),
    (
        osc_contract::CONTROL_ADAPTIVE_RESAMPLING_NEAR_FAR_THRESHOLD_MS,
        "adaptive_resampling_high_recover_entry_margin_ms",
    ),
];

/// Values the pre-registry address ignored where its row takes them: a
/// non-positive latency target, input channel count or input rate (the row
/// reads them as "unset", as the JSON patch always did) and a negative
/// integral discharge ratio (the row clamps it to 0, as the patch did).
pub(crate) fn legacy_ignores(addr: &str, raw: &RawOptionValue) -> bool {
    let number = match raw {
        RawOptionValue::Number(n) => *n,
        _ => return false,
    };
    match addr {
        osc_contract::CONTROL_LATENCY_TARGET
        | osc_contract::CONTROL_INPUT_LIVE_CHANNELS
        | osc_contract::CONTROL_INPUT_LIVE_SAMPLE_RATE => number <= 0.0,
        osc_contract::CONTROL_ADAPTIVE_RESAMPLING_INTEGRAL_DISCHARGE_RATIO => number < 0.0,
        _ => false,
    }
}
