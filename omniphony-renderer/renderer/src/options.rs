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

use crate::config::RenderConfig;
use crate::live_params::{LiveParams, OutputChannelMapping, PhantomExtractMode, SurroundPlacement};

/// What kind of value an option takes. Drives wire validation, the published
/// schema, and (later) which Studio control the binder renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionKind {
    /// Boolean toggle; accepts int/float (0 = false) and bool on the wire.
    Bool,
    /// Closed set of canonical lowercase spellings. The option's setter may
    /// accept extra legacy aliases (e.g. `direct`/`virtual` → `spatial`), but
    /// it always reports back a canonical value.
    Enum(&'static [&'static str]),
    /// Free-form string (e.g. a registry id).
    Str,
}

/// Behaviour flags, interpreted generically by the plumbing layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionFlags(u8);

impl OptionFlags {
    pub const NONE: Self = Self(0);
    /// Commit to config.yaml immediately when set over OSC (targeted,
    /// sidecar-clearing write). Rationale: a host fallback can route the next
    /// toggle to a *different* renderer instance, so the value must reach the
    /// file — not just the memory of whichever instance received it.
    pub const PERSIST: Self = Self(1 << 0);
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
}

impl OptionDefault {
    fn to_json(self) -> serde_json::Value {
        match self {
            Self::Bool(b) => b.into(),
            Self::Str(s) => s.into(),
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
}

/// One live option, declared once.
pub struct OptionSpec {
    /// The single canonical name (see the module docs).
    pub key: &'static str,
    pub kind: OptionKind,
    /// Canonical default; the config key is omitted at this value.
    pub default: OptionDefault,
    pub flags: OptionFlags,
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

/// Every declared live option. Iterated by the OSC dispatcher, persistence,
/// seeding, the snapshot, the schema dump, and the conformance net.
pub static LIVE_OPTIONS: &[OptionSpec] = &[
    OptionSpec {
        key: "surround_placement",
        kind: OptionKind::Enum(&["side", "back"]),
        default: OptionDefault::Str("side"),
        flags: OptionFlags::PERSIST.or(OptionFlags::REPLAN),
        i18n_key: "twoDSources.surroundLabel",
        help_i18n_key: None,
        legacy_control_addr: "/omniphony/control/surround_placement",
        set: |live, raw| match raw {
            RawOptionValue::Str(s) => {
                let placement = SurroundPlacement::from_str(s)?;
                live.surround_placement = placement;
                Some(placement.as_str().to_string())
            }
            _ => None,
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
        flags: OptionFlags::PERSIST.or(OptionFlags::REPLAN),
        i18n_key: "twoDSources.syntheticObjectsLabel",
        help_i18n_key: Some("help.syntheticObjects"),
        legacy_control_addr: "/omniphony/control/synthetic_objects",
        set: |live, raw| {
            let enabled = match raw {
                RawOptionValue::Number(n) => *n != 0.0,
                RawOptionValue::Bool(b) => *b,
                RawOptionValue::Str(_) => return None,
            };
            live.synthetic_objects_enabled = enabled;
            Some(if enabled { "1" } else { "0" }.to_string())
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
        key: "output_channel_mapping",
        kind: OptionKind::Enum(&["by_index", "by_name"]),
        default: OptionDefault::Str("by_index"),
        flags: OptionFlags::PERSIST,
        i18n_key: "audio.channelMapping",
        help_i18n_key: None,
        legacy_control_addr: "/omniphony/control/output_channel_mapping",
        set: |live, raw| match raw {
            RawOptionValue::Str(s) => {
                let mapping = OutputChannelMapping::from_str(s)?;
                live.output_channel_mapping = mapping;
                Some(mapping.as_str().to_string())
            }
            _ => None,
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
        flags: OptionFlags::PERSIST.or(OptionFlags::REPLAN),
        i18n_key: "twoDSources.objectGeneratorLabel",
        help_i18n_key: Some("help.objectGenerator"),
        legacy_control_addr: "/omniphony/control/object_generator",
        set: |live, raw| match raw {
            RawOptionValue::Str(s) => {
                if live.object_generator_id != *s {
                    // New generator: drop the previous one's param overrides so
                    // the new generator starts at its declared defaults.
                    live.object_generator_params.clear();
                }
                live.object_generator_id = s.to_string();
                Some(s.to_string())
            }
            _ => None,
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
        flags: OptionFlags::PERSIST.or(OptionFlags::REPLAN),
        i18n_key: "twoDSources.phantomLabel",
        help_i18n_key: Some("help.phantomExtract"),
        legacy_control_addr: "/omniphony/control/phantom_extract",
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
];

/// Look an option up by its canonical key (the `/control/option` key argument).
pub fn find(key: &str) -> Option<&'static OptionSpec> {
    LIVE_OPTIONS.iter().find(|spec| spec.key == key)
}

/// Apply a client value to a control's live params through `spec`: validate +
/// set, mark the config dirty, and bump the replan epoch when a
/// `REPLAN`-flagged option **actually changed value** — a redundant re-send
/// (Studio reconnecting, a client echoing state back) must not force a
/// re-plan, which can carry an audible re-prime transient. Returns the
/// canonical value applied, or `None` when the value was rejected.
///
/// Persistence and client notification stay with the transport layer (the OSC
/// dispatcher), which alone knows the config path and the subscriber list.
pub fn apply_to_control(
    control: &crate::live_params::RendererControl,
    spec: &OptionSpec,
    raw: &RawOptionValue,
) -> Option<String> {
    let (canonical, changed) = {
        let mut live = control.live.write();
        let before = (spec.get_json)(&live);
        let canonical = (spec.set)(&mut live, raw)?;
        let changed = (spec.get_json)(&live) != before;
        (canonical, changed)
    };
    control.mark_dirty();
    if changed && spec.flags.contains(OptionFlags::REPLAN) {
        control.bump_options_epoch();
    }
    Some(canonical)
}

/// Look an option up by its pre-registry dedicated control address.
pub fn find_by_legacy_addr(addr: &str) -> Option<&'static OptionSpec> {
    LIVE_OPTIONS
        .iter()
        .find(|spec| spec.legacy_control_addr == addr)
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
    // Virtual bed: absent = the built-in canonical poses (LFE direct).
    if let Some(bed) = render.virtual_bed.clone() {
        live.virtual_bed = Some(bed);
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
    // Virtual bed: persist verbatim (round the radius for stable diffs);
    // `None` keeps the key out so the built-in canonical poses stay in effect.
    render.virtual_bed = live.virtual_bed.clone().map(|mut bed| {
        bed.radius_m = (bed.radius_m as f64 * 1e6).round() as f32 / 1e6;
        bed
    });
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
            };
            let mut flags = Vec::new();
            if spec.flags.contains(OptionFlags::PERSIST) {
                flags.push("persist");
            }
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
            // Every declared option persists today; a non-persisted option
            // would need a dedicated review of the save/seed paths.
            assert!(spec.flags.contains(OptionFlags::PERSIST), "{}", spec.key);
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
        }
    }
}
