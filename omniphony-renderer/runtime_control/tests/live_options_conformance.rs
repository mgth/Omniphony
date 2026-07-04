//! Conformance net over the hand-wired live options (the "2D-sources family").
//!
//! Phase 0 of `docs/live-options-registry.md`: until the declared options
//! registry exists, this table is the single list of live options and every
//! test below iterates it. Adding a live option means adding ONE row here; the
//! row then proves the option is visible on every layer it claims to be on:
//!
//! * the OSC control catalogue (`osc_contract::ALL_CONTROL`),
//! * the `/omniphony/state/renderer` snapshot (key present, value tracked),
//! * config persistence (saved when non-default, omitted when default).
//!
//! When the Phase-1 registry lands, these rows migrate into the registry and
//! the tests iterate it instead.
//!
//! Known gaps this net does NOT cover yet (Phase-1 targets, see the RFC and
//! `docs/option-surface-parity.fr.md`):
//! * CLI-vs-FFI seed parity: only `channel_render_mode` and
//!   `surround_placement` are seeded by the CLI bootstrap, while
//!   `Engine::from_paths` (FFI) seeds the whole family — exercising both boot
//!   paths needs an engine fixture that doesn't exist yet.
//! * OSC dispatcher acceptance: `handle_control_message` is crate-private to
//!   `orender_engine` and needs a live socket; the generic Phase-1 handler
//!   will be testable without one.

use std::sync::Arc;

use renderer::config::{Config, RenderConfig};
use renderer::live_params::{
    ChannelRenderMode, LiveEvaluationMode, LiveParams, OutputChannelMapping,
    PreferredEvaluationMode, RendererControl, SurroundPlacement,
};
use renderer::spatial_renderer::SpatialRenderer;
use renderer::spatial_vbap::{DistanceModel, VbapTableMode};
use renderer::speaker_layout::SpeakerLayout;
use runtime_control::osc_contract;
use runtime_control::persist::save_live_config_to_path;
use runtime_control::snapshot::build_renderer_state_json;

/// One live option, declared once. Every conformance test iterates this table.
struct LiveOptionRow {
    /// Canonical option name (`render.*` config key and failure-message id).
    key: &'static str,
    /// Client → engine control address; must be in `ALL_CONTROL`.
    control_addr: &'static str,
    /// Key in the `/omniphony/state/renderer` JSON snapshot (camelCase).
    snapshot_key: &'static str,
    /// Flip the option to a non-default value.
    set_non_default: fn(&mut LiveParams),
    /// Does the snapshot value reflect `set_non_default`?
    snapshot_reflects: fn(&serde_json::Value) -> bool,
    /// Does the reloaded config reflect `set_non_default`?
    config_reflects: fn(&RenderConfig) -> bool,
}

const LIVE_OPTIONS: &[LiveOptionRow] = &[
    LiveOptionRow {
        key: "channel_render_mode",
        control_addr: osc_contract::CONTROL_CHANNEL_RENDER_MODE,
        snapshot_key: "channelRenderMode",
        set_non_default: |live| live.channel_render_mode = ChannelRenderMode::Host,
        snapshot_reflects: |v| v == "host",
        config_reflects: |r| r.channel_render_mode == Some(ChannelRenderMode::Host),
    },
    LiveOptionRow {
        key: "surround_placement",
        control_addr: osc_contract::CONTROL_SURROUND_PLACEMENT,
        snapshot_key: "surroundPlacement",
        set_non_default: |live| live.surround_placement = SurroundPlacement::Back,
        snapshot_reflects: |v| v == "back",
        config_reflects: |r| r.surround_placement == Some(SurroundPlacement::Back),
    },
    LiveOptionRow {
        key: "output_channel_mapping",
        control_addr: osc_contract::CONTROL_OUTPUT_CHANNEL_MAPPING,
        snapshot_key: "outputChannelMapping",
        set_non_default: |live| live.output_channel_mapping = OutputChannelMapping::ByName,
        snapshot_reflects: |v| v == "by_name",
        config_reflects: |r| r.output_channel_mapping == Some(OutputChannelMapping::ByName),
    },
    LiveOptionRow {
        key: "object_generator_id",
        control_addr: osc_contract::CONTROL_OBJECT_GENERATOR,
        snapshot_key: "objectGeneratorId",
        set_non_default: |live| live.object_generator_id = "copy_up".to_string(),
        snapshot_reflects: |v| v == "copy_up",
        config_reflects: |r| r.object_generator_id.as_deref() == Some("copy_up"),
    },
    LiveOptionRow {
        key: "object_generator_params",
        control_addr: osc_contract::CONTROL_OBJECT_GENERATOR_PARAM,
        snapshot_key: "objectGeneratorParams",
        set_non_default: |live| {
            live.object_generator_params
                .insert("strength".to_string(), 0.5);
        },
        snapshot_reflects: |v| v["strength"] == 0.5,
        config_reflects: |r| {
            r.object_generator_params
                .as_ref()
                .is_some_and(|m| m.get("strength") == Some(&0.5))
        },
    },
    LiveOptionRow {
        key: "phantom_enabled",
        control_addr: osc_contract::CONTROL_PHANTOM_EXTRACT,
        snapshot_key: "phantomEnabled",
        set_non_default: |live| live.phantom_enabled = true,
        snapshot_reflects: |v| v == true,
        config_reflects: |r| r.phantom_enabled == Some(true),
    },
    LiveOptionRow {
        key: "phantom_params",
        control_addr: osc_contract::CONTROL_PHANTOM_EXTRACT_PARAM,
        snapshot_key: "phantomParams",
        set_non_default: |live| {
            live.phantom_params.insert("strength".to_string(), 0.75);
        },
        snapshot_reflects: |v| v["strength"] == 0.75,
        config_reflects: |r| {
            r.phantom_params
                .as_ref()
                .is_some_and(|m| m.get("strength") == Some(&0.75))
        },
    },
    LiveOptionRow {
        key: "virtual_bed",
        control_addr: osc_contract::CONTROL_VIRTUAL_BED,
        snapshot_key: "virtualBed",
        set_non_default: |live| {
            live.virtual_bed = Some(SpeakerLayout::preset("5.1").expect("5.1 preset"));
        },
        snapshot_reflects: |v| v.is_object(),
        config_reflects: |r| r.virtual_bed.is_some(),
    },
];

/// A real `RendererControl`, the only way live options exist at runtime.
/// Small cartesian grid so the table build stays trivial.
fn fixture_control() -> Arc<RendererControl> {
    let layout = SpeakerLayout::preset("7.1.4").expect("7.1.4 preset");
    let renderer = SpatialRenderer::new(
        layout,
        48_000,
        1,
        1,
        0.0,
        2.0,
        VbapTableMode::Cartesian {
            x_size: 5,
            y_size: 5,
            z_size: 3,
            z_neg_size: 3,
        },
        false,
        true,
        DistanceModel::Linear,
        false,
        1.0,
        1.0,
        0.0,
        1.0,
        false,
        [1.0, 1.0, 1.0],
        1.0,
        1.0,
        0.0,
        0.0,
        false,
        false,
        false,
        1.0,
        1.0,
        PreferredEvaluationMode::PrecomputedCartesian,
        LiveEvaluationMode::PrecomputedCartesian,
        5,
        5,
        3,
        3,
    )
    .expect("fixture renderer");
    renderer.renderer_control()
}

fn snapshot_json(control: &Arc<RendererControl>) -> serde_json::Value {
    let live = control.live.read();
    let json = build_renderer_state_json(
        &live,
        &control.active_topology(),
        1.0,
        control.available_backends(),
        control.all_backend_params(),
        &[],
    );
    serde_json::from_str(&json).expect("snapshot is valid JSON")
}

fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "orender-live-options-conformance-{}-{name}.yaml",
        std::process::id()
    ))
}

/// Every live option's control address is in the machine-readable catalogue.
/// (`surround_placement` was missing from `ALL_CONTROL` until this net landed —
/// exactly the silent-omission class the RFC describes.)
#[test]
fn every_live_option_is_in_the_control_catalogue() {
    for row in LIVE_OPTIONS {
        assert!(
            osc_contract::ALL_CONTROL.contains(&row.control_addr),
            "{}: control address {} is not listed in osc_contract::ALL_CONTROL",
            row.key,
            row.control_addr
        );
    }
}

/// The renderer state snapshot carries every live option — key present at
/// defaults, value tracking a live change. A key silently dropped here never
/// reaches Studio (incident gap #1).
#[test]
fn snapshot_carries_every_live_option() {
    let control = fixture_control();

    let at_defaults = snapshot_json(&control);
    for row in LIVE_OPTIONS {
        assert!(
            at_defaults.get(row.snapshot_key).is_some(),
            "{}: snapshot key {} missing from /state/renderer at defaults",
            row.key,
            row.snapshot_key
        );
    }

    {
        let mut live = control.live.write();
        for row in LIVE_OPTIONS {
            (row.set_non_default)(&mut live);
        }
    }
    let changed = snapshot_json(&control);
    for row in LIVE_OPTIONS {
        assert!(
            (row.snapshot_reflects)(&changed[row.snapshot_key]),
            "{}: snapshot key {} does not reflect the live change (got {})",
            row.key,
            row.snapshot_key,
            changed[row.snapshot_key]
        );
    }
}

/// Registry-driven nets (phase 1): the declared `renderer::options` rows must
/// agree with the snapshot, the config round-trip, the schema, and the OSC
/// catalogue — one non-default sample value per row exercises all of it.
mod registry {
    use super::*;
    use renderer::options::{self, RawOptionValue};

    /// A non-default sample per declared option, keyed by canonical name.
    /// Extending the registry without extending this list fails loudly below.
    fn non_default_samples() -> Vec<(&'static str, RawOptionValue<'static>)> {
        vec![
            ("channel_render_mode", RawOptionValue::Str("host")),
            ("surround_placement", RawOptionValue::Str("back")),
            ("output_channel_mapping", RawOptionValue::Str("by_name")),
            ("object_generator_id", RawOptionValue::Str("copy_up")),
            ("phantom_enabled", RawOptionValue::Number(1.0)),
        ]
    }

    fn sample_for(key: &str) -> RawOptionValue<'static> {
        non_default_samples()
            .into_iter()
            .find(|(k, _)| *k == key)
            .unwrap_or_else(|| panic!("no non-default sample for registry option {key}"))
            .1
    }

    #[test]
    fn legacy_addresses_are_catalogued_and_resolvable() {
        for spec in options::LIVE_OPTIONS {
            assert!(
                osc_contract::ALL_CONTROL.contains(&spec.legacy_control_addr),
                "{}: legacy address missing from ALL_CONTROL",
                spec.key
            );
            assert!(options::find(spec.key).is_some(), "{}", spec.key);
            assert!(
                options::find_by_legacy_addr(spec.legacy_control_addr).is_some(),
                "{}",
                spec.key
            );
        }
        assert!(
            osc_contract::ALL_CONTROL.contains(&osc_contract::CONTROL_OPTION),
            "generic /control/option missing from ALL_CONTROL"
        );
    }

    /// The snapshot `options` block carries every registry row, and its
    /// defaults agree with the published schema defaults — a divergence means
    /// the constructed `LiveParams` default and the declared default drifted.
    #[test]
    fn snapshot_options_block_matches_registry_and_schema_defaults() {
        let control = fixture_control();
        let snapshot = snapshot_json(&control);
        let block = snapshot
            .get("options")
            .expect("snapshot has an options block");
        let schema: serde_json::Value =
            serde_json::from_str(&options::schema_json()).expect("valid schema");
        for (spec, schema_entry) in options::LIVE_OPTIONS.iter().zip(schema.as_array().unwrap()) {
            let value = block
                .get(spec.key)
                .unwrap_or_else(|| panic!("{}: missing from the options block", spec.key));
            assert_eq!(
                value, &schema_entry["default"],
                "{}: snapshot default != declared schema default",
                spec.key
            );
        }
    }

    /// set → store → seed round-trip: a value applied through the registry
    /// setter survives a config save and re-seeds a fresh boot identically —
    /// the CLI and FFI boot paths call the exact seed used here.
    #[test]
    fn set_store_seed_round_trips_every_option() {
        let control = fixture_control();
        {
            let mut live = control.live.write();
            for spec in options::LIVE_OPTIONS {
                let raw = sample_for(spec.key);
                assert!(
                    (spec.set)(&mut live, &raw).is_some(),
                    "{}: sample value rejected",
                    spec.key
                );
            }
        }
        let mut render = RenderConfig::default();
        {
            let live = control.live.read();
            options::store_live_to_config(&mut render, &live);
        }

        let fresh = fixture_control();
        {
            let mut live = fresh.live.write();
            options::seed_live_from_config(&mut live, &render);
        }
        let changed = control.live.read();
        let seeded = fresh.live.read();
        for spec in options::LIVE_OPTIONS {
            assert_eq!(
                (spec.get_json)(&changed),
                (spec.get_json)(&seeded),
                "{}: value lost in the store→seed round-trip",
                spec.key
            );
        }
    }

    /// `apply_to_control` bumps the replan epoch exactly when a REPLAN-flagged
    /// option **changes value**: a redundant re-send (a client echoing state
    /// back) must not force a re-plan, and non-REPLAN options never bump.
    #[test]
    fn apply_bumps_epoch_only_on_real_replan_change() {
        let control = fixture_control();
        let placement = options::find("surround_placement").expect("registered");
        let mode = options::find("channel_render_mode").expect("registered");

        let epoch = control.options_epoch();
        assert_eq!(
            options::apply_to_control(&control, placement, &RawOptionValue::Str("back")).as_deref(),
            Some("back")
        );
        assert_eq!(control.options_epoch(), epoch + 1, "real change must bump");

        assert!(
            options::apply_to_control(&control, placement, &RawOptionValue::Str("back")).is_some()
        );
        assert_eq!(
            control.options_epoch(),
            epoch + 1,
            "redundant re-send must not bump (it would re-prime the stages)"
        );

        assert!(
            options::apply_to_control(&control, placement, &RawOptionValue::Str("bogus")).is_none()
        );
        assert_eq!(
            control.options_epoch(),
            epoch + 1,
            "rejected value: no bump"
        );

        assert!(options::apply_to_control(&control, mode, &RawOptionValue::Str("host")).is_some());
        assert_eq!(
            control.options_epoch(),
            epoch + 1,
            "non-REPLAN option must not bump"
        );
        assert!(
            control
                .config_dirty
                .load(std::sync::atomic::Ordering::Relaxed),
            "apply must mark the config dirty"
        );
    }

    /// Every declared option accepts its sample through the setter and reports
    /// a canonical value; the setter rejects a shape no option accepts.
    #[test]
    fn setters_validate_and_report_canonical_values() {
        let control = fixture_control();
        let mut live = control.live.write();
        for spec in options::LIVE_OPTIONS {
            let raw = sample_for(spec.key);
            let canonical = (spec.set)(&mut live, &raw);
            assert!(canonical.is_some(), "{}: sample rejected", spec.key);
            if let options::OptionKind::Enum(values) = spec.kind {
                let canonical = canonical.unwrap();
                assert!(
                    values.contains(&canonical.as_str()),
                    "{}: canonical '{}' not in declared values",
                    spec.key,
                    canonical
                );
                assert!(
                    (spec.set)(&mut live, &RawOptionValue::Str("no_such_value_xyz")).is_none(),
                    "{}: junk value accepted",
                    spec.key
                );
            }
        }
    }
}

/// Config save persists every live option when non-default and omits its key
/// when default (the value==default → key-absent contract). An option dropped
/// from `persist.rs` silently loses user settings on save.
#[test]
fn config_save_covers_every_live_option_and_omits_defaults() {
    let control = fixture_control();
    let base = temp_path("base-missing");
    let out = temp_path("out");

    save_live_config_to_path(&control, None, &base, &out).expect("save at defaults");
    let yaml = std::fs::read_to_string(&out).expect("saved config readable");
    for row in LIVE_OPTIONS {
        assert!(
            !yaml.contains(&format!("{}:", row.key)),
            "{}: default value should keep the key out of the saved config",
            row.key
        );
    }

    {
        let mut live = control.live.write();
        for row in LIVE_OPTIONS {
            (row.set_non_default)(&mut live);
        }
    }
    save_live_config_to_path(&control, None, &base, &out).expect("save non-defaults");
    let config = Config::load_or_default(&out);
    let render = config.render.expect("saved config has a render section");
    for row in LIVE_OPTIONS {
        assert!(
            (row.config_reflects)(&render),
            "{}: non-default value did not round-trip through the saved config",
            row.key
        );
    }

    let _ = std::fs::remove_file(&out);
}
