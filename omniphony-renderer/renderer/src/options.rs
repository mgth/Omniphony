//! The declared live-options registry (RFC: `docs/live-options-registry.md`).
//!
//! One [`OptionSpec`] row per live option; every other layer iterates the
//! registry instead of naming options one by one:
//!
//! * the generic OSC handler (`/omniphony/control/option` — the legacy
//!   per-option addresses stay as aliases),
//! * config persistence (the targeted persist-on-change and the full
//!   live-state save both call [`store_live_to_config`]),
//! * config→live seeding ([`seed_live_from_config`], shared by the CLI
//!   bootstrap and `Engine::from_paths` so FFI/CLI parity holds by
//!   construction),
//! * the `/state/renderer` snapshot (`options` block, [`options_json`]) and
//!   the published schema ([`schema_json`], consumed by the Studio contract
//!   check and, later, the `data-option` binder).
//!
//! `key` is the single canonical name: the `render.*` config key, the key
//! argument of `/omniphony/control/option`, the key inside the snapshot
//! `options` block, and the Studio binding id. Inside the `options` namespace
//! the key travels verbatim (snake_case) — no per-layer renaming.
//!
//! The audio hot path does NOT read options through the registry: options
//! remain typed fields on [`LiveParams`], read directly per frame. The
//! registry is the declaration + plumbing layer, not the storage.
//!
//! Adding a live option = one row here (+ the `LiveParams`/`RenderConfig`
//! fields it points at, + Studio i18n keys). The conformance net in
//! `runtime_control/tests/live_options_conformance.rs` fails when a row is
//! missing a layer.
//!
//! Options that only make sense together belong to an [`OptionGroup`], which
//! declares what applying a change costs (an [`ApplyEffect`]: a topology
//! rebuild, an evaluation-only rebuild, …). Several keys written in one go
//! (`/omniphony/control/options`, [`apply_batch`]) are applied under one lock
//! and cost one rebuild and one notification, never one per key — so a
//! rebuild never starts on a half-written group.

use crate::config::RenderConfig;
use crate::live_params::{
    CrossoverType, HrirUpdateLattice, LiveParams, OutputChannelMapping, PhantomExtractMode,
    RampMode, SurroundPlacement,
};
use omniphony_osc_contract as osc_contract;

/// What kind of value an option takes. Drives wire validation, the published
/// schema, and (later) which Studio control the binder renders.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OptionKind {
    /// Boolean toggle; accepts int/float (0 = false) and bool on the wire.
    Bool,
    /// Closed set of canonical lowercase spellings. The option's setter may
    /// accept extra legacy aliases (e.g. `direct`/`virtual` → `spatial`), but
    /// it always reports back a canonical value.
    Enum(&'static [&'static str]),
    /// Free-form string (e.g. a registry id).
    Str,
    /// Bounded numeric value; accepts float/int (and a parseable string) on
    /// the wire, clamped to `[min, max]` by the setter. `step` is a UI hint
    /// for the Studio control, not a validation grid.
    Float { min: f32, max: f32, step: f32 },
    /// `len` bounded numbers set together (e.g. the room's width, length and
    /// height). On the wire: `len` numeric arguments; each is clamped to
    /// `[min, max]` like a `Float`.
    FloatArray {
        len: usize,
        min: f32,
        max: f32,
        step: f32,
    },
}

impl OptionKind {
    /// How many wire arguments a value of this kind takes — what lets
    /// `/omniphony/control/options` walk a list of key/value pairs.
    pub const fn arity(self) -> usize {
        match self {
            Self::FloatArray { len, .. } => len,
            _ => 1,
        }
    }
}

/// When a group's writes take effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupMode {
    /// Applied as it arrives; a multi-key write is applied as one.
    Live,
}

impl GroupMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Live => "live",
        }
    }
}

/// What applying a changed option costs beyond storing it. The effects of a
/// batch merge: one rebuild of the widest kind any changed key asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyEffect {
    /// Nothing: the value is read where it is used (per frame, or compared
    /// against what a stage built, which rebuilds itself).
    None,
    /// Re-plan the synthesized-object stages (`RendererControl::options_epoch`),
    /// like the `REPLAN` flag of an ungrouped option.
    Replan,
    /// Rebuild the speaker topology: backend geometry and evaluation.
    Topology,
    /// Rebuild the evaluation layer only, reusing the backend's gain models.
    Evaluation,
}

impl ApplyEffect {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Replan => "replan",
            Self::Topology => "topology",
            Self::Evaluation => "evaluation",
        }
    }
}

/// Options applied together (see the module docs). Declared once; every
/// member row points at it.
#[derive(Debug)]
pub struct OptionGroup {
    /// Canonical group name, published in the schema.
    pub key: &'static str,
    pub mode: GroupMode,
    pub effect: ApplyEffect,
    /// Studio i18n key for the group's title.
    pub i18n_key: &'static str,
}

/// Room proportions: they scale every position before panning, so a change
/// rebuilds the topology.
pub static ROOM: OptionGroup = OptionGroup {
    key: "room",
    mode: GroupMode::Live,
    effect: ApplyEffect::Topology,
    i18n_key: "room.title",
};

/// Every declared group.
pub static OPTION_GROUPS: &[&OptionGroup] = &[&ROOM];

/// The rebuild a set of changes asks the engine for, widest first merged in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Rebuild {
    #[default]
    None,
    /// Evaluation layer only ([`ApplyEffect::Evaluation`]).
    Evaluation,
    /// Full topology ([`ApplyEffect::Topology`]).
    Topology,
}

/// Behaviour flags, interpreted generically by the plumbing layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionFlags(u8);

impl OptionFlags {
    pub const NONE: Self = Self(0);
    // Bit 0 was `PERSIST`, a write to config.yaml on every OSC set. Options
    // change what is heard, so they reach the file through the Save button
    // only (docs/persistence-policy.md); an unsaved value that must follow a
    // handoff to another renderer instance rides the live-handoff sidecar.
    /// A change re-plans synthesized-object stages: setting the option bumps
    /// `RendererControl::options_epoch`, which plan signatures compare instead
    /// of enumerating options field by field.
    pub const REPLAN: Self = Self(1 << 1);

    pub const fn or(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

/// An option's canonical default, as it appears on the wire.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OptionDefault {
    Bool(bool),
    Str(&'static str),
    Float(f32),
    FloatArray(&'static [f32]),
}

impl OptionDefault {
    pub fn to_json(self) -> serde_json::Value {
        match self {
            Self::Bool(b) => b.into(),
            Self::Str(s) => s.into(),
            Self::Float(f) => f.into(),
            Self::FloatArray(values) => values.into(),
        }
    }
}

/// A raw, unvalidated option value as supplied by a client. The OSC layer
/// maps `OscType` into this; other transports (FFI, CLI) can too.
#[derive(Debug, Clone, Copy)]
pub enum RawOptionValue<'a> {
    Str(&'a str),
    Number(f64),
    Bool(bool),
    /// The values of a `FloatArray` option, in order.
    Numbers(&'a [f64]),
}

/// One live option, declared once.
pub struct OptionSpec {
    /// The single canonical name (see the module docs).
    pub key: &'static str,
    pub kind: OptionKind,
    /// Canonical default; the config key is omitted at this value.
    pub default: OptionDefault,
    pub flags: OptionFlags,
    /// The group the option is applied with, if any.
    pub group: Option<&'static OptionGroup>,
    /// Studio i18n key for the control label.
    pub i18n_key: &'static str,
    /// Studio i18n key for the help text (`None` = no help entry yet).
    pub help_i18n_key: Option<&'static str>,
    /// The pre-registry dedicated control address, kept as an alias of
    /// `/omniphony/control/option` so existing clients keep working.
    pub legacy_control_addr: &'static str,
    /// Validate and apply a client value. Returns the canonical value applied
    /// (for logging/echo), or `None` when the value is invalid — the engine
    /// drops bad input rather than erroring, per the OSC contract.
    pub set: fn(&mut LiveParams, &RawOptionValue) -> Option<String>,
    /// Current value, for the snapshot `options` block.
    pub get_json: fn(&LiveParams) -> serde_json::Value,
    /// Write the live value into the config (skip-if-default descriptors keep
    /// the key out of the file at the default).
    pub config_store: fn(&mut RenderConfig, &LiveParams),
    /// Seed the live value from a loaded config; an absent key is a no-op
    /// (the constructed default stays).
    pub config_seed: fn(&mut LiveParams, &RenderConfig),
}

/// A boolean from the raw shapes a `Bool` option accepts: a bool, or a number
/// (`0` = false). Strings are rejected.
fn raw_bool(raw: &RawOptionValue) -> Option<bool> {
    match raw {
        RawOptionValue::Number(n) => Some(*n != 0.0),
        RawOptionValue::Bool(b) => Some(*b),
        RawOptionValue::Str(_) | RawOptionValue::Numbers(_) => None,
    }
}

/// Canonical wire spelling of a boolean option value.
fn bool_canonical(value: bool) -> String {
    if value { "1" } else { "0" }.to_string()
}

/// The string of a string-shaped value (`Enum` / `Str` options); other shapes
/// are rejected.
fn raw_str<'a>(raw: &RawOptionValue<'a>) -> Option<&'a str> {
    match raw {
        RawOptionValue::Str(s) => Some(s),
        _ => None,
    }
}

/// A `Float` option value: a number or a parseable string, finite, clamped to
/// the bounds declared by `kind` — so a row states its range once, in its
/// `kind`, and the setter, the seed and the schema all read it from there.
fn raw_float(raw: &RawOptionValue, kind: OptionKind) -> Option<f32> {
    let value = match raw {
        RawOptionValue::Number(n) => *n as f32,
        RawOptionValue::Str(s) => s.trim().parse::<f32>().ok()?,
        RawOptionValue::Bool(_) | RawOptionValue::Numbers(_) => return None,
    };
    value.is_finite().then(|| clamp_to(kind, value))
}

/// A `FloatArray` option value: exactly `N` finite numbers, each clamped to
/// the bounds declared by `kind`.
fn raw_floats<const N: usize>(raw: &RawOptionValue, kind: OptionKind) -> Option<[f32; N]> {
    let RawOptionValue::Numbers(values) = raw else {
        return None;
    };
    if values.len() != N {
        return None;
    }
    let mut out = [0.0; N];
    for (slot, value) in out.iter_mut().zip(values.iter()) {
        let value = *value as f32;
        if !value.is_finite() {
            return None;
        }
        *slot = clamp_to(kind, value);
    }
    Some(out)
}

/// Clamp `value` to the bounds of a `Float` / `FloatArray` kind (identity for
/// other kinds).
fn clamp_to(kind: OptionKind, value: f32) -> f32 {
    match kind {
        OptionKind::Float { min, max, .. } | OptionKind::FloatArray { min, max, .. } => {
            value.clamp(min, max)
        }
        _ => value,
    }
}

const CROSSOVER_FIR_TRANSITION_RATIO_KIND: OptionKind = OptionKind::Float {
    min: 0.05,
    max: 2.0,
    step: 0.05,
};
/// dBFS target of the anti-clip auto-gain: at or below 0 dBFS.
const AUTO_GAIN_CEILING_DB_KIND: OptionKind = OptionKind::Float {
    min: -12.0,
    max: 0.0,
    step: 0.1,
};
const DRC_WEIGHT_KIND: OptionKind = OptionKind::Float {
    min: 0.0,
    max: 1.0,
    step: 0.01,
};

/// Room ratios: floored like the geometry floors them, bounded far above any
/// real room so a typo cannot blow the scene up.
const ROOM_RATIO_KIND: OptionKind = OptionKind::FloatArray {
    len: 3,
    min: crate::config_fields::room::MIN_RATIO,
    max: 100.0,
    step: 0.01,
};
const ROOM_EXTENT_KIND: OptionKind = OptionKind::Float {
    min: crate::config_fields::room::MIN_RATIO,
    max: 100.0,
    step: 0.01,
};
const ROOM_CENTER_BLEND_KIND: OptionKind = OptionKind::Float {
    min: 0.0,
    max: 1.0,
    step: 0.01,
};

/// The room a config describes, for the room rows' seeds. `None` for a
/// malformed `room_ratio`, which leaves the live room alone: the renderer
/// build and the profile switch reject such a config before any seed runs.
fn configured_room(render: &RenderConfig) -> Option<crate::config_fields::room::Room> {
    crate::config_fields::room::resolve(render).ok()
}

#[inline]
fn round6(v: f32) -> f32 {
    (v * 1_000_000.0).round() / 1_000_000.0
}

/// Every declared live option. Iterated by the OSC dispatcher, persistence,
/// seeding, the snapshot, the schema dump, and the conformance net.
pub static LIVE_OPTIONS: &[OptionSpec] = &[
    OptionSpec {
        key: "surround_placement",
        kind: OptionKind::Enum(&["side", "back"]),
        default: OptionDefault::Str("side"),
        flags: OptionFlags::REPLAN,
        group: None,
        i18n_key: "twoDSources.surroundLabel",
        help_i18n_key: None,
        legacy_control_addr: osc_contract::CONTROL_SURROUND_PLACEMENT,
        set: |live, raw| {
            let placement = SurroundPlacement::from_str(raw_str(raw)?)?;
            live.surround_placement = placement;
            Some(placement.as_str().to_string())
        },
        get_json: |live| live.surround_placement.as_str().into(),
        config_store: |render, live| {
            crate::config_fields::surround_placement::store(render, live.surround_placement)
        },
        config_seed: |live, render| {
            if let Some(placement) = crate::config_fields::surround_placement::get(render) {
                live.surround_placement = placement;
            }
        },
    },
    OptionSpec {
        key: "synthetic_objects_enabled",
        kind: OptionKind::Bool,
        default: OptionDefault::Bool(false),
        flags: OptionFlags::REPLAN,
        group: None,
        i18n_key: "twoDSources.syntheticObjectsLabel",
        help_i18n_key: Some("help.syntheticObjects"),
        legacy_control_addr: osc_contract::CONTROL_SYNTHETIC_OBJECTS,
        set: |live, raw| {
            let enabled = raw_bool(raw)?;
            live.synthetic_objects_enabled = enabled;
            Some(bool_canonical(enabled))
        },
        get_json: |live| live.synthetic_objects_enabled.into(),
        // Always persist this master, including false: an explicit false must
        // continue to suppress remembered non-off child selections after reload.
        config_store: |render, live| {
            render.synthetic_objects_enabled = Some(live.synthetic_objects_enabled);
        },
        config_seed: |live, render| {
            if let Some(enabled) = render.synthetic_objects_enabled {
                live.synthetic_objects_enabled = enabled;
            }
        },
    },
    OptionSpec {
        key: "decode_thread",
        kind: OptionKind::Bool,
        default: OptionDefault::Bool(false),
        // No REPLAN: nothing synthesized depends on where decoding runs.
        flags: OptionFlags::NONE,
        group: None,
        i18n_key: "renderer.decodeThreadLabel",
        help_i18n_key: Some("help.decodeThread"),
        legacy_control_addr: osc_contract::CONTROL_DECODE_THREAD,
        set: |live, raw| {
            let enabled = raw_bool(raw)?;
            live.decode_thread = enabled;
            Some(bool_canonical(enabled))
        },
        get_json: |live| live.decode_thread.into(),
        config_store: |render, live| {
            crate::config_fields::decode_thread::store(render, live.decode_thread)
        },
        config_seed: |live, render| {
            if let Some(enabled) = crate::config_fields::decode_thread::get(render) {
                live.decode_thread = enabled;
            }
        },
    },
    OptionSpec {
        key: "output_channel_mapping",
        kind: OptionKind::Enum(&["by_index", "by_name"]),
        default: OptionDefault::Str("by_index"),
        flags: OptionFlags::NONE,
        group: None,
        i18n_key: "audio.channelMapping",
        help_i18n_key: None,
        legacy_control_addr: osc_contract::CONTROL_OUTPUT_CHANNEL_MAPPING,
        set: |live, raw| {
            let mapping = OutputChannelMapping::from_str(raw_str(raw)?)?;
            live.output_channel_mapping = mapping;
            Some(mapping.as_str().to_string())
        },
        get_json: |live| live.output_channel_mapping.as_str().into(),
        config_store: |render, live| {
            crate::config_fields::output_channel_mapping::store(render, live.output_channel_mapping)
        },
        config_seed: |live, render| {
            if let Some(mapping) = crate::config_fields::output_channel_mapping::get(render) {
                live.output_channel_mapping = mapping;
            }
        },
    },
    OptionSpec {
        key: "object_generator_id",
        kind: OptionKind::Str,
        default: OptionDefault::Str(""),
        flags: OptionFlags::REPLAN,
        group: None,
        i18n_key: "twoDSources.objectGeneratorLabel",
        help_i18n_key: Some("help.objectGenerator"),
        legacy_control_addr: osc_contract::CONTROL_OBJECT_GENERATOR,
        set: |live, raw| {
            let id = raw_str(raw)?;
            if live.object_generator_id != id {
                // New generator: drop the previous one's param overrides so
                // the new generator starts at its declared defaults.
                live.object_generator_params.clear();
            }
            live.object_generator_id = id.to_string();
            Some(id.to_string())
        },
        get_json: |live| live.object_generator_id.as_str().into(),
        config_store: |render, live| {
            crate::config_fields::object_generator_id::store(render, &live.object_generator_id)
        },
        config_seed: |live, render| {
            if let Some(id) = crate::config_fields::object_generator_id::get(render) {
                live.object_generator_id = id;
            }
        },
    },
    OptionSpec {
        key: "phantom_extract_mode",
        kind: OptionKind::Enum(&["off", "broadband", "spectral"]),
        default: OptionDefault::Str("off"),
        flags: OptionFlags::REPLAN,
        group: None,
        i18n_key: "twoDSources.phantomLabel",
        help_i18n_key: Some("help.phantomExtract"),
        legacy_control_addr: osc_contract::CONTROL_PHANTOM_EXTRACT,
        set: |live, raw| {
            let mode = match raw {
                RawOptionValue::Str(s) => PhantomExtractMode::from_str(s)?,
                // Backward compatibility for the old boolean legacy OSC
                // address: enabling selects the historical broadband default.
                RawOptionValue::Number(n) => {
                    if *n == 0.0 {
                        PhantomExtractMode::Off
                    } else {
                        PhantomExtractMode::Broadband
                    }
                }
                RawOptionValue::Bool(false) => PhantomExtractMode::Off,
                RawOptionValue::Bool(true) => PhantomExtractMode::Broadband,
                RawOptionValue::Numbers(_) => return None,
            };
            live.phantom_extract_mode = mode;
            Some(mode.as_str().to_string())
        },
        get_json: |live| live.phantom_extract_mode.as_str().into(),
        config_store: |render, live| {
            crate::config_fields::phantom_extract_mode::store(render, live.phantom_extract_mode);
            render.phantom_enabled = None;
            if let Some(params) = render.phantom_params.as_mut() {
                params.remove("method");
                if params.is_empty() {
                    render.phantom_params = None;
                }
            }
        },
        config_seed: |live, render| {
            if let Some(mode) = crate::config_fields::phantom_extract_mode::get(render) {
                live.phantom_extract_mode = mode;
            }
        },
    },
    OptionSpec {
        key: "crossover_type",
        kind: OptionKind::Enum(&["lr4", "fir"]),
        default: OptionDefault::Str("lr4"),
        // No REPLAN: the speaker stage compares the live value against the
        // bank it built every frame and rebuilds the filter bank itself; no
        // synthesized-object topology depends on it.
        flags: OptionFlags::NONE,
        group: None,
        i18n_key: "renderer.crossoverTypeLabel",
        help_i18n_key: Some("help.crossoverType"),
        legacy_control_addr: osc_contract::CONTROL_CROSSOVER_TYPE,
        set: |live, raw| {
            let crossover_type = CrossoverType::from_str(raw_str(raw)?)?;
            live.crossover_type = crossover_type;
            Some(crossover_type.as_str().to_string())
        },
        get_json: |live| live.crossover_type.as_str().into(),
        config_store: |render, live| {
            crate::config_fields::crossover_type::store(render, live.crossover_type)
        },
        config_seed: |live, render| {
            if let Some(crossover_type) = crate::config_fields::crossover_type::get(render) {
                live.crossover_type = crossover_type;
            }
        },
    },
    OptionSpec {
        key: "crossover_fir_transition_ratio",
        kind: CROSSOVER_FIR_TRANSITION_RATIO_KIND,
        default: OptionDefault::Float(0.5),
        // No REPLAN, same as crossover_type: the speaker stage compares the
        // live value against the bank it built every frame and rebuilds the
        // FIR bank itself when it moves.
        flags: OptionFlags::NONE,
        group: None,
        i18n_key: "renderer.crossoverTransitionLabel",
        help_i18n_key: Some("help.crossoverFirTransition"),
        legacy_control_addr: osc_contract::CONTROL_CROSSOVER_FIR_TRANSITION_RATIO,
        set: |live, raw| {
            let v = raw_float(raw, CROSSOVER_FIR_TRANSITION_RATIO_KIND)?;
            live.crossover_fir_transition_ratio = v;
            Some(format!("{v}"))
        },
        get_json: |live| live.crossover_fir_transition_ratio.into(),
        config_store: |render, live| {
            crate::config_fields::crossover_fir_transition_ratio::store(
                render,
                live.crossover_fir_transition_ratio,
            )
        },
        config_seed: |live, render| {
            if let Some(ratio) = crate::config_fields::crossover_fir_transition_ratio::get(render) {
                live.crossover_fir_transition_ratio =
                    clamp_to(CROSSOVER_FIR_TRANSITION_RATIO_KIND, ratio);
            }
        },
    },
    OptionSpec {
        key: "hrir_update_lattice",
        kind: OptionKind::Enum(&["exact", "fine", "balanced", "coarse"]),
        default: OptionDefault::Str("exact"),
        // No REPLAN: the lattice only gates a per-block cache in the binaural
        // stage, it does not change any synthesized-object topology.
        flags: OptionFlags::NONE,
        group: None,
        i18n_key: "binaural.hrirUpdateLatticeLabel",
        help_i18n_key: Some("help.hrirUpdateLattice"),
        legacy_control_addr: osc_contract::CONTROL_BINAURAL_HRIR_UPDATE_LATTICE,
        set: |live, raw| {
            let lattice = HrirUpdateLattice::from_str(raw_str(raw)?)?;
            live.binaural.hrir_update_lattice = lattice;
            Some(lattice.as_str().to_string())
        },
        get_json: |live| live.binaural.hrir_update_lattice.as_str().into(),
        config_store: |render, live| {
            crate::config_fields::hrir_update_lattice::store(
                render,
                live.binaural.hrir_update_lattice,
            )
        },
        config_seed: |live, render| {
            if let Some(lattice) = crate::config_fields::hrir_update_lattice::get(render) {
                live.binaural.hrir_update_lattice = lattice;
            }
        },
    },
    // ── Gain stage, loudness, transitions, DRC ──────────────────────────
    //
    // Migrated from dedicated handlers; the dedicated addresses stay as
    // aliases and the flat snapshot keys (`autoGain`, `autoGainCeilingDb`,
    // `rampMode`, `/state/loudness` `enabled`, `/state/input` `drcMode` /
    // `drcWeight`) are still emitted. Like every option they reach
    // `config.yaml` on an explicit Save. None re-plans anything: they
    // are read per frame (gain stage, ramps) or pushed to the decoder.
    OptionSpec {
        key: "auto_gain",
        kind: OptionKind::Bool,
        default: OptionDefault::Bool(crate::config_fields::auto_gain::DEFAULT),
        flags: OptionFlags::NONE,
        group: None,
        i18n_key: "autoGain.title",
        help_i18n_key: Some("help.master.autoGain"),
        legacy_control_addr: osc_contract::CONTROL_AUTO_GAIN,
        set: |live, raw| {
            let enabled = raw_bool(raw)?;
            live.auto_gain = enabled;
            Some(bool_canonical(enabled))
        },
        get_json: |live| live.auto_gain.into(),
        config_store: |render, live| crate::config_fields::auto_gain::store(render, live.auto_gain),
        config_seed: |live, render| {
            if let Some(enabled) = crate::config_fields::auto_gain::get(render) {
                live.auto_gain = enabled;
            }
        },
    },
    OptionSpec {
        key: "auto_gain_ceiling_db",
        kind: AUTO_GAIN_CEILING_DB_KIND,
        default: OptionDefault::Float(crate::config_fields::auto_gain_ceiling_db::DEFAULT),
        flags: OptionFlags::NONE,
        group: None,
        i18n_key: "autoGain.ceiling",
        help_i18n_key: Some("help.master.ceiling"),
        legacy_control_addr: osc_contract::CONTROL_AUTO_GAIN_CEILING,
        set: |live, raw| {
            let db = raw_float(raw, AUTO_GAIN_CEILING_DB_KIND)?;
            live.auto_gain_ceiling_db = db;
            Some(format!("{db}"))
        },
        get_json: |live| live.auto_gain_ceiling_db.into(),
        config_store: |render, live| {
            crate::config_fields::auto_gain_ceiling_db::store(render, live.auto_gain_ceiling_db)
        },
        // Seeded as configured (not clamped), exactly as before the
        // migration; only a client write is bounded.
        config_seed: |live, render| {
            if let Some(db) = crate::config_fields::auto_gain_ceiling_db::get(render) {
                live.auto_gain_ceiling_db = db;
            }
        },
    },
    OptionSpec {
        key: "use_loudness",
        kind: OptionKind::Bool,
        default: OptionDefault::Bool(crate::config_fields::use_loudness::DEFAULT),
        flags: OptionFlags::NONE,
        group: None,
        i18n_key: "section.loudness",
        help_i18n_key: Some("help.drc.loudness"),
        legacy_control_addr: osc_contract::CONTROL_LOUDNESS,
        set: |live, raw| {
            let enabled = raw_bool(raw)?;
            live.use_loudness = enabled;
            Some(bool_canonical(enabled))
        },
        get_json: |live| live.use_loudness.into(),
        config_store: |render, live| {
            crate::config_fields::use_loudness::store(render, live.use_loudness)
        },
        config_seed: |live, render| {
            if let Some(enabled) = crate::config_fields::use_loudness::get(render) {
                live.use_loudness = enabled;
            }
        },
    },
    OptionSpec {
        key: "ramp_mode",
        kind: OptionKind::Enum(&["off", "frame", "interp", "sample"]),
        default: OptionDefault::Str(crate::config_fields::ramp_mode::DEFAULT),
        flags: OptionFlags::NONE,
        group: None,
        i18n_key: "audio.rampMode",
        help_i18n_key: None,
        legacy_control_addr: osc_contract::CONTROL_RAMP_MODE,
        set: |live, raw| {
            let mode = RampMode::from_str(raw_str(raw)?)?;
            live.ramp_mode = mode;
            Some(mode.as_str().to_string())
        },
        get_json: |live| live.ramp_mode.as_str().into(),
        config_store: |render, live| {
            crate::config_fields::ramp_mode::store(render, live.ramp_mode.as_str())
        },
        config_seed: |live, render| {
            if let Some(mode) = crate::config_fields::ramp_mode::get(render)
                .as_deref()
                .and_then(RampMode::from_str)
            {
                live.ramp_mode = mode;
            }
        },
    },
    OptionSpec {
        key: "drc_mode",
        // Free-form: the modes are the bridge's (`supportedDrcModes` on
        // `/state/input`), not a closed set the renderer knows.
        kind: OptionKind::Str,
        default: OptionDefault::Str("Off"),
        flags: OptionFlags::NONE,
        group: None,
        i18n_key: "input.drc",
        help_i18n_key: Some("help.drc.mode"),
        legacy_control_addr: osc_contract::CONTROL_INPUT_DRC_MODE,
        set: |live, raw| {
            let mode = raw_str(raw)?;
            if live.drc_mode != mode {
                live.drc_mode = mode.to_string();
            }
            Some(mode.to_string())
        },
        get_json: |live| live.drc_mode.as_str().into(),
        config_store: |render, live| {
            render.drc_mode = (live.drc_mode != "Off").then(|| live.drc_mode.clone());
        },
        config_seed: |live, render| {
            if let Some(mode) = render.drc_mode.as_ref() {
                live.drc_mode = mode.clone();
            }
        },
    },
    OptionSpec {
        key: "drc_weight",
        kind: DRC_WEIGHT_KIND,
        default: OptionDefault::Float(1.0),
        flags: OptionFlags::NONE,
        group: None,
        i18n_key: "input.drc_weight",
        help_i18n_key: Some("help.drc.weight"),
        legacy_control_addr: osc_contract::CONTROL_INPUT_DRC_WEIGHT,
        set: |live, raw| {
            let weight = raw_float(raw, DRC_WEIGHT_KIND)?;
            live.drc_weight = weight;
            Some(format!("{weight}"))
        },
        get_json: |live| live.drc_weight.into(),
        config_store: |render, live| {
            render.drc_weight =
                ((live.drc_weight - 1.0).abs() > 1e-4).then(|| round6(live.drc_weight));
        },
        config_seed: |live, render| {
            if let Some(weight) = render.drc_weight {
                live.drc_weight = clamp_to(DRC_WEIGHT_KIND, weight);
            }
        },
    },
    // ── Room ────────────────────────────────────────────────────────────
    //
    // The room group: the proportions every position is scaled by before
    // panning. The file stores them in metres (`config_fields::room`); the
    // dedicated addresses stay as aliases and the snapshot's `roomRatio`
    // block is still emitted.
    OptionSpec {
        key: "room_ratio",
        kind: ROOM_RATIO_KIND,
        default: OptionDefault::FloatArray(&[1.0, 2.0, 1.0]),
        flags: OptionFlags::NONE,
        group: Some(&ROOM),
        i18n_key: "room.summary.ratio",
        help_i18n_key: None,
        legacy_control_addr: osc_contract::CONTROL_ROOM_RATIO,
        set: |live, raw| {
            let ratio = raw_floats::<3>(raw, ROOM_RATIO_KIND)?;
            live.room_ratio = ratio;
            Some(format!("{},{},{}", ratio[0], ratio[1], ratio[2]))
        },
        get_json: |live| live.room_ratio.as_slice().into(),
        config_store: |render, live| {
            crate::config_fields::room::store_ratio(render, live.room_ratio)
        },
        // Seeded as configured (not clamped), exactly as the renderer build
        // reads it; only a client write is bounded. The same holds for the
        // other room rows.
        config_seed: |live, render| {
            if let Some(room) = configured_room(render) {
                live.room_ratio = room.ratio;
            }
        },
    },
    OptionSpec {
        key: "room_ratio_rear",
        kind: ROOM_EXTENT_KIND,
        default: OptionDefault::Float(2.0),
        flags: OptionFlags::NONE,
        group: Some(&ROOM),
        i18n_key: "room.axis.rear",
        help_i18n_key: Some("help.room.rear"),
        legacy_control_addr: osc_contract::CONTROL_ROOM_RATIO_REAR,
        set: |live, raw| {
            let v = raw_float(raw, ROOM_EXTENT_KIND)?;
            live.room_ratio_rear = v;
            Some(format!("{v}"))
        },
        get_json: |live| live.room_ratio_rear.into(),
        config_store: |render, live| {
            crate::config_fields::room::store_rear(render, live.room_ratio_rear)
        },
        // An absent rear follows the configured length (`room::parse`).
        config_seed: |live, render| {
            if let Some(room) = configured_room(render) {
                live.room_ratio_rear = room.rear;
            }
        },
    },
    OptionSpec {
        key: "room_ratio_lower",
        kind: ROOM_EXTENT_KIND,
        default: OptionDefault::Float(crate::config_fields::room::DEFAULT_LOWER),
        flags: OptionFlags::NONE,
        group: Some(&ROOM),
        i18n_key: "room.axis.lower",
        help_i18n_key: Some("help.room.lower"),
        legacy_control_addr: osc_contract::CONTROL_ROOM_RATIO_LOWER,
        set: |live, raw| {
            let v = raw_float(raw, ROOM_EXTENT_KIND)?;
            live.room_ratio_lower = v;
            Some(format!("{v}"))
        },
        get_json: |live| live.room_ratio_lower.into(),
        config_store: |render, live| {
            crate::config_fields::room::store_lower(render, live.room_ratio_lower)
        },
        config_seed: |live, render| {
            if let Some(room) = configured_room(render) {
                live.room_ratio_lower = room.lower;
            }
        },
    },
    OptionSpec {
        key: "room_ratio_center_blend",
        kind: ROOM_CENTER_BLEND_KIND,
        default: OptionDefault::Float(crate::config_fields::room::DEFAULT_CENTER_BLEND),
        flags: OptionFlags::NONE,
        group: Some(&ROOM),
        i18n_key: "room.centerBlend",
        help_i18n_key: Some("help.room.centerBlend"),
        legacy_control_addr: osc_contract::CONTROL_ROOM_RATIO_CENTER_BLEND,
        set: |live, raw| {
            let v = raw_float(raw, ROOM_CENTER_BLEND_KIND)?;
            live.room_ratio_center_blend = v;
            Some(format!("{v}"))
        },
        get_json: |live| live.room_ratio_center_blend.into(),
        config_store: |render, live| {
            crate::config_fields::room::store_center_blend(render, live.room_ratio_center_blend)
        },
        config_seed: |live, render| {
            if let Some(room) = configured_room(render) {
                live.room_ratio_center_blend = room.center_blend;
            }
        },
    },
];

/// The longest `FloatArray` a row declares (checked by a test), so a default
/// can be widened on the stack.
const MAX_ARRAY_LEN: usize = 4;

/// Look an option up by its canonical key (the `/control/option` key argument).
pub fn find(key: &str) -> Option<&'static OptionSpec> {
    LIVE_OPTIONS.iter().find(|spec| spec.key == key)
}

/// What [`apply_to_control`] made of a client value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    /// The canonical value now in force.
    pub canonical: String,
    /// Whether it differs from the value before.
    pub changed: bool,
}

/// What [`apply_batch`] made of a list of client values.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BatchApplied {
    /// One entry per input, in order: `None` for a rejected value.
    pub results: Vec<Option<Applied>>,
    /// Whether any value changed.
    pub changed: bool,
    /// The one rebuild the changed options ask the engine for, merged over
    /// their groups' effects. `Rebuild::None` when nothing changed.
    pub rebuild: Rebuild,
}

/// The rebuild a changed option asks for, from its group.
fn rebuild_for(spec: &OptionSpec) -> Rebuild {
    match spec.group.map(|group| group.effect) {
        Some(ApplyEffect::Topology) => Rebuild::Topology,
        Some(ApplyEffect::Evaluation) => Rebuild::Evaluation,
        Some(ApplyEffect::None | ApplyEffect::Replan) | None => Rebuild::None,
    }
}

/// Whether a changed option re-plans the synthesized-object stages.
fn replans(spec: &OptionSpec) -> bool {
    spec.flags.contains(OptionFlags::REPLAN)
        || spec
            .group
            .is_some_and(|group| group.effect == ApplyEffect::Replan)
}

/// Apply a client value to a control's live params through `spec`: validate +
/// set, and — only when the option **actually changed value** — mark the
/// config dirty and bump the replan epoch of a re-planning option. A
/// redundant re-send (Studio reconnecting, a client echoing state back) must
/// neither light the Save button nor force a re-plan, which can carry an
/// audible re-prime transient. Returns `None` when the value was rejected.
///
/// A rebuild the option's group asks for is the caller's to trigger; use
/// [`apply_batch`] to learn it. Client notification stays with the transport
/// layer (the OSC dispatcher), which alone knows the subscriber list.
pub fn apply_to_control(
    control: &crate::live_params::RendererControl,
    spec: &OptionSpec,
    raw: &RawOptionValue,
) -> Option<Applied> {
    apply_batch(control, &[(spec, *raw)])
        .results
        .pop()
        .flatten()
}

/// Apply several client values at once: all of them under one write lock, so
/// neither the audio thread nor a rebuild ever sees half of the batch; then,
/// if anything changed, one dirty mark, at most one replan-epoch bump, and one
/// merged [`Rebuild`] for the caller to trigger. A rejected value is skipped
/// (and reported as `None`); the others still apply. A later entry for the
/// same key wins.
pub fn apply_batch(
    control: &crate::live_params::RendererControl,
    items: &[(&OptionSpec, RawOptionValue)],
) -> BatchApplied {
    let mut batch = BatchApplied {
        results: Vec::with_capacity(items.len()),
        ..BatchApplied::default()
    };
    let mut replan = false;
    {
        let mut live = control.live.write();
        for (spec, raw) in items {
            let before = (spec.get_json)(&live);
            let Some(canonical) = (spec.set)(&mut live, raw) else {
                batch.results.push(None);
                continue;
            };
            let changed = (spec.get_json)(&live) != before;
            if changed {
                batch.changed = true;
                batch.rebuild = batch.rebuild.max(rebuild_for(spec));
                replan |= replans(spec);
            }
            batch.results.push(Some(Applied { canonical, changed }));
        }
    }
    if batch.changed {
        control.mark_dirty();
        if replan {
            control.bump_options_epoch();
        }
    }
    batch
}

/// Look an option up by its pre-registry dedicated control address.
pub fn find_by_legacy_addr(addr: &str) -> Option<&'static OptionSpec> {
    LIVE_OPTIONS
        .iter()
        .find(|spec| spec.legacy_control_addr == addr)
}

/// Reset every declared option — plus the param bags and the virtual bed —
/// to its declared default. The live profile switch runs this before
/// [`seed_live_from_config`]: the per-option `config_seed` closures only
/// assign when the config pins a value, which is correct at construction
/// (live starts at defaults) but on a running control would silently keep
/// the previous profile's value for any field the incoming profile stores
/// as absent (the skip-if-default persist convention).
pub fn reset_live_to_defaults(live: &mut LiveParams) {
    for spec in LIVE_OPTIONS {
        let mut numbers = [0.0f64; MAX_ARRAY_LEN];
        let raw = match spec.default {
            OptionDefault::Bool(b) => RawOptionValue::Bool(b),
            OptionDefault::Str(s) => RawOptionValue::Str(s),
            OptionDefault::Float(f) => RawOptionValue::Number(f as f64),
            OptionDefault::FloatArray(values) => {
                let len = values.len().min(MAX_ARRAY_LEN);
                for (slot, value) in numbers.iter_mut().zip(values) {
                    *slot = *value as f64;
                }
                RawOptionValue::Numbers(&numbers[..len])
            }
        };
        if (spec.set)(live, &raw).is_none() {
            // A spec whose default fails its own validation is a registry bug.
            log::warn!("live option '{}' rejected its declared default", spec.key);
        }
    }
    live.object_generator_params.clear();
    live.phantom_params.clear();
    live.placement = crate::placement::PlacementState::default();
}

/// Seed every declared live option — plus the document-valued companions the
/// registry doesn't model (the two param bags and the virtual bed) — from a
/// loaded config. Shared by the CLI bootstrap and `Engine::from_paths` so the
/// two boot paths cannot drift (the FFI/CLI parity bug class).
pub fn seed_live_from_config(live: &mut LiveParams, render: &RenderConfig) {
    for spec in LIVE_OPTIONS {
        (spec.config_seed)(live, render);
    }
    // Param bags: absent = the stage's declared defaults.
    if let Some(params) = render.object_generator_params.clone() {
        live.object_generator_params = params;
    }
    if let Some(params) = render.phantom_params.clone() {
        live.phantom_params = params;
    }
    // Migrate the old phantom boolean + `phantom_params.method` split into the
    // explicit three-position mode. A remembered method remains available even
    // if the old enable switch was off.
    if render.phantom_extract_mode.is_none() {
        let legacy_method = live.phantom_params.get("method").copied();
        live.phantom_extract_mode = match (render.phantom_enabled, legacy_method) {
            (Some(true), Some(v)) if v >= 0.5 => PhantomExtractMode::Spectral,
            (Some(true), _) => PhantomExtractMode::Broadband,
            (_, Some(v)) if v >= 0.5 => PhantomExtractMode::Spectral,
            (_, Some(_)) => PhantomExtractMode::Broadband,
            _ => PhantomExtractMode::Off,
        };
    }
    live.phantom_params.remove("method");

    // Old configs had no global master. Infer it once from an active child so
    // upgrading preserves audible behaviour; new configs always persist the
    // master explicitly, including false.
    if render.synthetic_objects_enabled.is_none() {
        let generator_active = !live.object_generator_id.trim().is_empty()
            && !live.object_generator_id.eq_ignore_ascii_case("none");
        live.synthetic_objects_enabled = render.phantom_enabled.unwrap_or(false)
            || generator_active
            || render
                .phantom_extract_mode
                .is_some_and(|m| m != PhantomExtractMode::Off);
    }
    // Placement: absent = every family at its built-in defaults. A config
    // from before placement existed carries the single `virtual_bed` that
    // applied to every stream: that is the generic family in manual mode.
    if let Some(placement) = render.placement.as_ref() {
        live.placement = crate::placement::PlacementState::from_config(placement);
    } else if let Some(bed) = render.virtual_bed.clone() {
        live.placement = crate::placement::PlacementState::from_legacy_virtual_bed(bed);
    }
}

/// Write every declared live option — plus the param bags and the virtual
/// bed — into a config. Used by the full live-state save; the OSC targeted
/// persist stores single options through `OptionSpec::config_store`.
pub fn store_live_to_config(render: &mut RenderConfig, live: &LiveParams) {
    for spec in LIVE_OPTIONS {
        (spec.config_store)(render, live);
    }
    // Param bags: `None` keeps the key out of the file so each stage falls
    // back to its declared defaults.
    render.object_generator_params = if live.object_generator_params.is_empty() {
        None
    } else {
        Some(live.object_generator_params.clone())
    };
    let mut phantom_params = live.phantom_params.clone();
    phantom_params.remove("method");
    render.phantom_params = if phantom_params.is_empty() {
        None
    } else {
        Some(phantom_params)
    };
    // Legacy global-host and phantom boolean keys are read-only migrations.
    render.channel_render_mode = None;
    render.phantom_enabled = None;
    // Placement: `None` keeps the key out so every family stays at its
    // built-in defaults. The legacy `virtual_bed` was migrated into it at
    // seed time and is dropped here.
    render.placement = live.placement.to_config();
    render.virtual_bed = None;
}

/// The current value of every declared option, keyed by canonical name — the
/// `options` block of the `/state/renderer` snapshot. Emitted alongside the
/// legacy flat keys during the migration.
pub fn options_json(live: &LiveParams) -> serde_json::Value {
    let mut map = serde_json::Map::with_capacity(LIVE_OPTIONS.len());
    for spec in LIVE_OPTIONS {
        map.insert(spec.key.to_string(), (spec.get_json)(live));
    }
    map.into()
}

/// The machine-readable schema of every declared option, for the Studio
/// contract check (CI) and the `data-option` binder. Shape mirrors the
/// object-generator/phantom param schemas: an array of specs with i18n keys.
pub fn schema_json() -> String {
    let specs: Vec<serde_json::Value> = LIVE_OPTIONS
        .iter()
        .map(|spec| {
            let (kind, values) = match spec.kind {
                OptionKind::Bool => ("bool", None),
                OptionKind::Enum(values) => ("enum", Some(values)),
                OptionKind::Str => ("string", None),
                OptionKind::Float { .. } => ("float", None),
                OptionKind::FloatArray { .. } => ("float_array", None),
            };
            let mut flags = Vec::new();
            if spec.flags.contains(OptionFlags::REPLAN) {
                flags.push("replan");
            }
            let mut obj = serde_json::json!({
                "key": spec.key,
                "kind": kind,
                "default": spec.default.to_json(),
                "flags": flags,
                "i18nKey": spec.i18n_key,
            });
            if let Some(values) = values {
                obj["values"] = values.into();
            }
            match spec.kind {
                OptionKind::Float { min, max, step } => {
                    obj["min"] = min.into();
                    obj["max"] = max.into();
                    obj["step"] = step.into();
                }
                OptionKind::FloatArray {
                    len,
                    min,
                    max,
                    step,
                } => {
                    obj["len"] = len.into();
                    obj["min"] = min.into();
                    obj["max"] = max.into();
                    obj["step"] = step.into();
                }
                _ => {}
            }
            if let Some(group) = spec.group {
                obj["group"] = serde_json::json!({
                    "key": group.key,
                    "mode": group.mode.as_str(),
                    "effect": group.effect.as_str(),
                    "i18nKey": group.i18n_key,
                });
            }
            if let Some(help) = spec.help_i18n_key {
                obj["helpI18nKey"] = help.into();
            }
            obj
        })
        .collect();
    serde_json::Value::Array(specs).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique_snake_case_and_flags_sane() {
        let mut seen = std::collections::HashSet::new();
        for spec in LIVE_OPTIONS {
            assert!(seen.insert(spec.key), "duplicate option key {}", spec.key);
            assert!(
                spec.key.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "option key {} is not snake_case",
                spec.key
            );
            assert!(
                spec.legacy_control_addr.starts_with("/omniphony/control/"),
                "{}: bad legacy address",
                spec.key
            );
            // Every option reaches the file through the full save
            // (`store_live_into_config` stores every row) and is seeded from
            // it at boot.
        }
    }

    #[test]
    fn enum_defaults_are_listed_in_their_values() {
        for spec in LIVE_OPTIONS {
            if let (OptionKind::Enum(values), OptionDefault::Str(default)) =
                (spec.kind, spec.default)
            {
                assert!(
                    values.contains(&default),
                    "{}: default '{}' missing from values",
                    spec.key,
                    default
                );
            }
        }
    }

    #[test]
    fn float_values_are_bounded_by_their_kind() {
        let kind = DRC_WEIGHT_KIND;
        assert_eq!(raw_float(&RawOptionValue::Number(2.0), kind), Some(1.0));
        assert_eq!(raw_float(&RawOptionValue::Str(" -1 "), kind), Some(0.0));
        assert_eq!(raw_float(&RawOptionValue::Number(f64::NAN), kind), None);
        assert_eq!(raw_float(&RawOptionValue::Bool(true), kind), None);
        // Every Float row's default sits inside its own bounds.
        for spec in LIVE_OPTIONS {
            if let (OptionKind::Float { min, max, .. }, OptionDefault::Float(d)) =
                (spec.kind, spec.default)
            {
                assert!(
                    (min..=max).contains(&d),
                    "{}: default out of bounds",
                    spec.key
                );
            }
        }
    }

    #[test]
    fn float_arrays_take_exactly_their_length_and_are_bounded() {
        let kind = ROOM_RATIO_KIND;
        assert_eq!(
            raw_floats::<3>(&RawOptionValue::Numbers(&[1.0, 500.0, 0.0]), kind),
            Some([1.0, 100.0, crate::config_fields::room::MIN_RATIO])
        );
        assert_eq!(
            raw_floats::<3>(&RawOptionValue::Numbers(&[1.0, 2.0]), kind),
            None
        );
        assert_eq!(
            raw_floats::<3>(&RawOptionValue::Numbers(&[1.0, f64::NAN, 1.0]), kind),
            None
        );
        assert_eq!(raw_floats::<3>(&RawOptionValue::Number(1.0), kind), None);
        // No scalar kind takes an array.
        assert_eq!(raw_float(&RawOptionValue::Numbers(&[1.0]), kind), None);
        assert_eq!(raw_bool(&RawOptionValue::Numbers(&[1.0])), None);
    }

    #[test]
    fn array_rows_fit_the_reset_buffer_and_their_defaults_match_their_kind() {
        for spec in LIVE_OPTIONS {
            let OptionKind::FloatArray { len, min, max, .. } = spec.kind else {
                continue;
            };
            assert!(len <= MAX_ARRAY_LEN, "{}: raise MAX_ARRAY_LEN", spec.key);
            assert_eq!(spec.kind.arity(), len);
            let OptionDefault::FloatArray(values) = spec.default else {
                panic!("{}: an array option needs an array default", spec.key);
            };
            assert_eq!(values.len(), len, "{}: default length", spec.key);
            assert!(
                values.iter().all(|v| (min..=max).contains(v)),
                "{}: default out of bounds",
                spec.key
            );
        }
    }

    #[test]
    fn every_group_is_listed_and_every_listed_group_has_members() {
        let listed = |group: &OptionGroup| OPTION_GROUPS.iter().any(|g| std::ptr::eq(*g, group));
        for spec in LIVE_OPTIONS {
            if let Some(group) = spec.group {
                assert!(
                    listed(group),
                    "{}: group {} not listed",
                    spec.key,
                    group.key
                );
            }
        }
        let mut keys = std::collections::HashSet::new();
        for group in OPTION_GROUPS {
            assert!(keys.insert(group.key), "duplicate group {}", group.key);
            assert!(
                LIVE_OPTIONS
                    .iter()
                    .any(|spec| spec.group.is_some_and(|g| std::ptr::eq(g, *group))),
                "group {} has no member",
                group.key
            );
        }
    }

    #[test]
    fn a_batch_merges_the_widest_rebuild_of_what_changed() {
        assert_eq!(Rebuild::None.max(Rebuild::Evaluation), Rebuild::Evaluation);
        assert_eq!(
            Rebuild::Topology.max(Rebuild::Evaluation),
            Rebuild::Topology
        );
        let room = find("room_ratio_rear").expect("registered");
        assert_eq!(rebuild_for(room), Rebuild::Topology);
        assert_eq!(
            rebuild_for(find("ramp_mode").expect("registered")),
            Rebuild::None
        );
        assert!(replans(find("surround_placement").expect("registered")));
        assert!(!replans(room));
    }

    #[test]
    fn schema_json_parses_and_covers_every_spec() {
        let schema: serde_json::Value =
            serde_json::from_str(&schema_json()).expect("schema is valid JSON");
        let specs = schema.as_array().expect("schema is an array");
        assert_eq!(specs.len(), LIVE_OPTIONS.len());
        for (spec, entry) in LIVE_OPTIONS.iter().zip(specs) {
            assert_eq!(entry["key"], spec.key);
            assert!(entry["kind"].is_string());
            assert!(entry["i18nKey"].is_string());
            assert!(entry["flags"].is_array());
            if matches!(spec.kind, OptionKind::Enum(_)) {
                assert!(entry["values"].is_array(), "{}: missing values", spec.key);
            }
            if let OptionKind::FloatArray { len, .. } = spec.kind {
                assert_eq!(entry["len"], len, "{}", spec.key);
            }
            match spec.group {
                Some(group) => {
                    assert_eq!(entry["group"]["key"], group.key, "{}", spec.key);
                    assert_eq!(entry["group"]["mode"], group.mode.as_str());
                    assert_eq!(entry["group"]["effect"], group.effect.as_str());
                }
                None => assert!(entry.get("group").is_none(), "{}", spec.key),
            }
        }
    }
}
