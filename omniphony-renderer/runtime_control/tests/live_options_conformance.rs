//! Conformance net over the live options.
//!
//! Two nets live here. The declared `renderer::options` registry (see
//! `docs/live-options-registry.md`) is covered by the `registry` module below,
//! which iterates the registry itself. The hand-wired options that predate it,
//! or are not declared in it (the per-family placement, …), are covered by
//! [`HAND_WIRED_OPTIONS`]: one row per option, and every test iterates the
//! table, proving the option is visible on every layer it claims to be on:
//!
//! * the OSC control catalogue (`osc_contract::ALL_CONTROL`),
//! * the `/omniphony/state/renderer` snapshot (key present, value tracked),
//! * config persistence (saved when non-default, omitted when default).
//!
//! A row that migrates into the registry leaves the table; its registry row
//! is then covered by the `registry` nets.
//!
//! Known gaps this net does NOT cover yet (see the RFC and
//! `docs/option-surface-parity.md`):
//! * CLI-vs-FFI seed parity for the remaining CLI-specific options, while
//!   `Engine::from_paths` (FFI) seeds the whole family — exercising both boot
//!   paths needs an engine fixture that doesn't exist yet.
//! * OSC dispatcher acceptance end to end (socket, notification, persistence)
//!   is covered by `orender_engine::osc::dispatch`'s own tests, not here.

use std::sync::Arc;

use renderer::config::{Config, RenderConfig};
use renderer::live_params::{
    LiveEvaluationMode, LiveParams, OutputChannelMapping, PhantomExtractMode, RendererControl,
    SurroundPlacement,
};
use renderer::placement::{PlacementMode, SourceFamily};
use renderer::spatial_renderer::{RendererSpec, SpatialRenderer};
use renderer::spatial_vbap::{DistanceModel, VbapTableMode};
use renderer::speaker_layout::SpeakerLayout;
use runtime_control::osc_contract;
use runtime_control::persist::save_live_config_to_path;
use runtime_control::snapshot::build_renderer_state_json;

/// One hand-wired live option, declared once. Every conformance test iterates
/// [`HAND_WIRED_OPTIONS`].
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

const HAND_WIRED_OPTIONS: &[LiveOptionRow] = &[
    LiveOptionRow {
        key: "synthetic_objects_enabled",
        control_addr: osc_contract::CONTROL_SYNTHETIC_OBJECTS,
        snapshot_key: "syntheticObjectsEnabled",
        set_non_default: |live| live.options.synthetic_objects_enabled = true,
        snapshot_reflects: |v| v == true,
        config_reflects: |r| r.options.synthetic_objects_enabled == Some(true),
    },
    LiveOptionRow {
        key: "surround_placement",
        control_addr: osc_contract::CONTROL_SURROUND_PLACEMENT,
        snapshot_key: "surroundPlacement",
        set_non_default: |live| live.options.surround_placement = SurroundPlacement::Back,
        snapshot_reflects: |v| v == "back",
        config_reflects: |r| r.options.surround_placement == Some(SurroundPlacement::Back),
    },
    LiveOptionRow {
        key: "output_channel_mapping",
        control_addr: osc_contract::CONTROL_OUTPUT_CHANNEL_MAPPING,
        snapshot_key: "outputChannelMapping",
        set_non_default: |live| live.options.output_channel_mapping = OutputChannelMapping::ByName,
        snapshot_reflects: |v| v == "by_name",
        config_reflects: |r| r.options.output_channel_mapping == Some(OutputChannelMapping::ByName),
    },
    LiveOptionRow {
        key: "object_generator_id",
        control_addr: osc_contract::CONTROL_OBJECT_GENERATOR,
        snapshot_key: "objectGeneratorId",
        set_non_default: |live| live.options.object_generator_id = "copy_up".to_string(),
        snapshot_reflects: |v| v == "copy_up",
        config_reflects: |r| r.options.object_generator_id.as_deref() == Some("copy_up"),
    },
    LiveOptionRow {
        key: "phantom_extract_mode",
        control_addr: osc_contract::CONTROL_PHANTOM_EXTRACT,
        snapshot_key: "phantomExtractMode",
        set_non_default: |live| live.options.phantom_extract_mode = PhantomExtractMode::Spectral,
        snapshot_reflects: |v| v == "spectral",
        config_reflects: |r| r.options.phantom_extract_mode == Some(PhantomExtractMode::Spectral),
    },
    LiveOptionRow {
        key: "placement.generic.layout",
        control_addr: osc_contract::CONTROL_PLACEMENT_LAYOUT,
        snapshot_key: "placement",
        set_non_default: |live| {
            live.placement.family_mut(SourceFamily::GENERIC).layout =
                Some(SpeakerLayout::preset("5.1").expect("5.1 preset"));
        },
        snapshot_reflects: |v| v["generic"]["layout"].is_object(),
        config_reflects: |r| {
            r.placement
                .as_ref()
                .and_then(|p| p.get("generic"))
                .is_some_and(|g| g.layout.is_some())
        },
    },
    LiveOptionRow {
        key: "placement.auro.mode",
        control_addr: osc_contract::CONTROL_PLACEMENT_MODE,
        snapshot_key: "placement",
        set_non_default: |live| {
            // A family the loaded bridge declares, a sphere by default.
            live.placement
                .declare("auro", "Auro-3D", PlacementMode::Sphere);
            let auro = live.placement.find("auro").expect("declared");
            live.placement.family_mut(auro).mode = Some(PlacementMode::Room);
        },
        snapshot_reflects: |v| v["auro"]["mode"] == "room" && v["auro"]["effectiveMode"] == "room",
        config_reflects: |r| {
            r.placement
                .as_ref()
                .and_then(|p| p.get("auro"))
                .is_some_and(|a| a.mode == Some(PlacementMode::Room))
        },
    },
    // The legacy generic-bed address stays in the catalogue for clients that
    // predate `placement`; its mirror key tracks the generic entries.
    LiveOptionRow {
        key: "virtual_bed (legacy mirror)",
        control_addr: osc_contract::CONTROL_VIRTUAL_BED,
        snapshot_key: "virtualBed",
        set_non_default: |live| {
            live.placement.family_mut(SourceFamily::GENERIC).layout =
                Some(SpeakerLayout::preset("5.1").expect("5.1 preset"));
        },
        snapshot_reflects: |v| v.is_object(),
        config_reflects: |r| {
            r.placement
                .as_ref()
                .and_then(|p| p.get("generic"))
                .is_some_and(|g| g.layout.is_some())
        },
    },
];

/// A real `RendererControl`, the only way live options exist at runtime.
/// Small cartesian grid so the table build stays trivial.
fn fixture_control() -> Arc<RendererControl> {
    let layout = SpeakerLayout::preset("7.1.4").expect("7.1.4 preset");
    let renderer = SpatialRenderer::new(RendererSpec {
        // 8 distance cells over 2 units: the declared polar defaults.
        spread_resolution: 0.25,
        table_mode: VbapTableMode::Cartesian {
            x_size: 5,
            y_size: 5,
            z_size: 3,
            z_neg_size: 3,
        },
        // The declared default (`config_fields::vbap_distance_model`).
        distance_model: DistanceModel::None,
        // The declared room defaults (`renderer::config_fields::room`), so
        // the snapshot-vs-schema default net holds for the room rows.
        room_ratio: [1.0, 2.0, 1.0],
        room_ratio_center_blend: 0.5,
        // The declared default mode.
        initial_evaluation_mode: LiveEvaluationMode::Auto,
        cartesian_default_x_size: 5,
        cartesian_default_y_size: 5,
        cartesian_default_z_size: 3,
        cartesian_default_z_neg_size: 3,
        ..renderer::test_support::spec(layout)
    })
    .expect("fixture renderer");
    renderer.renderer_control()
}

/// The option environment of a fixture control.
fn env(control: &Arc<RendererControl>) -> renderer::options::OptionEnv<'_> {
    renderer::options::OptionEnv::of(control)
}

/// A legacy alias is in the control catalogue: a whole address in
/// `ALL_CONTROL`, or a tail under one of the contract's prefix families.
fn legacy_addr_is_catalogued(addr: renderer::options::LegacyAddr) -> bool {
    use renderer::options::LegacyAddr;
    match addr {
        // Nothing to catalogue: the generic setters only.
        LegacyAddr::None => true,
        LegacyAddr::Exact(addr) => osc_contract::ALL_CONTROL.contains(&addr),
        LegacyAddr::Prefixed { prefix, .. } => [
            osc_contract::CONTROL_DISTANCE_DIFFUSE_PREFIX,
            osc_contract::CONTROL_HYBRID_PREFIX,
            osc_contract::CONTROL_RENDER_EVALUATION_CARTESIAN_PREFIX,
            osc_contract::CONTROL_RENDER_EVALUATION_POLAR_PREFIX,
        ]
        .contains(&prefix),
    }
}

/// The address a client sends for a legacy alias, if the option has one.
fn legacy_addr_example(addr: renderer::options::LegacyAddr) -> Option<String> {
    use renderer::options::LegacyAddr;
    match addr {
        LegacyAddr::None => None,
        LegacyAddr::Exact(addr) => Some(addr.to_string()),
        LegacyAddr::Prefixed { prefix, tail } => Some(format!("{prefix}{tail}")),
    }
}

fn snapshot_json(control: &Arc<RendererControl>) -> serde_json::Value {
    let brir_layout_error = control.brir_layout().err();
    let live = control.live.read();
    let json = build_renderer_state_json(
        &live,
        &control.active_topology(),
        1.0,
        control.available_backends(),
        control.plugin_params(),
        &[],
        "[]",
        "{}",
        control.crossover_info(),
        &control.binaural_hrir_status(),
        &control.binaural_brir_status(),
        brir_layout_error,
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
    for row in HAND_WIRED_OPTIONS {
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
    for row in HAND_WIRED_OPTIONS {
        assert!(
            at_defaults.get(row.snapshot_key).is_some(),
            "{}: snapshot key {} missing from /state/renderer at defaults",
            row.key,
            row.snapshot_key
        );
    }

    {
        let mut live = control.live.write();
        for row in HAND_WIRED_OPTIONS {
            (row.set_non_default)(&mut live);
        }
    }
    let changed = snapshot_json(&control);
    for row in HAND_WIRED_OPTIONS {
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

    /// A non-default sample for the options [`derived_sample`] cannot make one
    /// for (free-form strings, arrays, values that depend on other rows),
    /// keyed by canonical name. A row with neither fails loudly below.
    fn non_default_samples() -> Vec<(&'static str, RawOptionValue<'static>)> {
        vec![
            ("object_generator_id", RawOptionValue::Str("copy_up")),
            ("hrir_update_lattice", RawOptionValue::Str("coarse")),
            ("drc_mode", RawOptionValue::Str("Standard")),
            // Width stays the reference (1): the file stores metres against
            // it, so a width ratio is folded into the layout radius on reload.
            ("room_ratio", RawOptionValue::Numbers(&[1.0, 3.0, 1.5])),
            ("room_ratio_rear", RawOptionValue::Number(2.5)),
            ("room_ratio_lower", RawOptionValue::Number(0.75)),
            ("room_ratio_center_blend", RawOptionValue::Number(0.25)),
            ("vbap_distance_model", RawOptionValue::Str("linear")),
            ("distance_model_metric", RawOptionValue::Str("chebyshev")),
            ("distance_diffuse", RawOptionValue::Bool(true)),
            ("distance_diffuse_threshold", RawOptionValue::Number(0.5)),
            ("distance_diffuse_curve", RawOptionValue::Number(1.5)),
            ("distance_diffuse_metric", RawOptionValue::Str("chebyshev")),
            ("distance_diffuse_mirror_axes", RawOptionValue::Str("xyz")),
            // The cartesian table in force, so the grid below is saved.
            (
                "render_evaluation_mode",
                RawOptionValue::Str("precomputed_cartesian"),
            ),
            (
                "evaluation_object_size_intervals",
                RawOptionValue::Number(3.0),
            ),
            // Not the fixture's (it renders above the floor only).
            ("vbap_allow_negative_z", RawOptionValue::Bool(true)),
            ("evaluation_cartesian_x_size", RawOptionValue::Number(7.0)),
            ("evaluation_cartesian_y_size", RawOptionValue::Number(6.0)),
            ("evaluation_cartesian_z_size", RawOptionValue::Number(4.0)),
            (
                "evaluation_cartesian_z_neg_size",
                RawOptionValue::Number(2.0),
            ),
            // Values the build's quantization keeps as they are (no negative
            // elevations in the fixture: 45 cells over 90°).
            ("vbap_azimuth_resolution", RawOptionValue::Number(90.0)),
            ("vbap_elevation_resolution", RawOptionValue::Number(45.0)),
            ("vbap_distance_res", RawOptionValue::Number(4.0)),
            ("vbap_distance_max", RawOptionValue::Number(3.0)),
            (
                "render_evaluation_position_interpolation",
                RawOptionValue::Bool(false),
            ),
            ("render_backend", RawOptionValue::Str("barycenter")),
            (
                "hybrid_external_backend",
                RawOptionValue::Str("experimental_distance"),
            ),
            ("hybrid_internal_backend", RawOptionValue::Str("vbap")),
            ("hybrid_curve_smoothing", RawOptionValue::Number(0.5)),
            ("hybrid_metric", RawOptionValue::Str("spherical")),
            ("output_mode", RawOptionValue::Str("binaural")),
            ("binaural_mode", RawOptionValue::Str("cascaded")),
            // A SOFA file: its path rides its own config key.
            (
                "hrir_source",
                RawOptionValue::Str("sofa:/data/hrtf/test.sofa"),
            ),
            ("brir_head_tracking", RawOptionValue::Str("on")),
            ("brir_max_length_s", RawOptionValue::Number(1.5)),
            ("brir_tail_floor_db", RawOptionValue::Number(70.0)),
            ("binaural_unit_scale_m", RawOptionValue::Number(2.0)),
            ("binaural_head_radius_m", RawOptionValue::Number(0.09)),
            ("binaural_air_absorption", RawOptionValue::Bool(false)),
            ("binaural_diffuse_field_eq", RawOptionValue::Bool(true)),
            ("binaural_sphere_coordinates", RawOptionValue::Bool(true)),
            ("reflections_enabled", RawOptionValue::Bool(true)),
            ("reflections_level", RawOptionValue::Number(0.7)),
            ("reflections_wall_cutoff_hz", RawOptionValue::Number(8000.0)),
            ("reflections_room_width_m", RawOptionValue::Number(5.0)),
            ("reflections_room_depth_m", RawOptionValue::Number(6.0)),
            ("reflections_room_height_m", RawOptionValue::Number(3.0)),
            ("reverb_enabled", RawOptionValue::Bool(true)),
            ("reverb_level", RawOptionValue::Number(0.4)),
            ("reverb_rt60_s", RawOptionValue::Number(0.8)),
            ("reverb_predelay_ms", RawOptionValue::Number(10.0)),
            ("reverb_size", RawOptionValue::Number(1.5)),
            ("reverb_rt60_low_ratio", RawOptionValue::Number(1.5)),
            ("reverb_rt60_high_ratio", RawOptionValue::Number(0.5)),
            (
                "head_tracking_osc_address",
                RawOptionValue::Str("/rotation"),
            ),
            ("head_tracking_format", RawOptionValue::Str("quat")),
            ("head_tracking_smoothing", RawOptionValue::Number(0.5)),
            ("head_tracking_invert", RawOptionValue::Bool(true)),
            ("binaural_ear_gains", RawOptionValue::Numbers(&[0.8, 1.2])),
            // +20 dB: the file stores decibels, and 10 survives the trip
            // exactly.
            ("master_gain", RawOptionValue::Number(10.0)),
        ]
    }

    /// A value other than the default, from the row's kind: the other
    /// boolean, a step away, the next integer, another enum value.
    fn derived_sample(spec: &options::OptionSpec) -> Option<RawOptionValue<'static>> {
        use options::{OptionDefault, OptionKind};
        match (spec.kind, spec.default) {
            (OptionKind::Bool, OptionDefault::Bool(default)) => {
                Some(RawOptionValue::Bool(!default))
            }
            (OptionKind::Float { min, max, step }, OptionDefault::Float(default)) => {
                let (default, step) = (default as f64, step as f64);
                let up = default + step;
                Some(RawOptionValue::Number(if up <= max as f64 {
                    up
                } else {
                    (default - step).max(min as f64)
                }))
            }
            (OptionKind::Int { min, max }, OptionDefault::Int(default)) => {
                let value = if default < max {
                    default + 1
                } else {
                    (default - 1).max(min)
                };
                Some(RawOptionValue::Number(value as f64))
            }
            (OptionKind::Enum(names), OptionDefault::Str(default)) => names
                .iter()
                .find(|name| **name != default)
                .map(|name| RawOptionValue::Str(name)),
            _ => None,
        }
    }

    pub(super) fn sample_for(spec: &options::OptionSpec) -> RawOptionValue<'static> {
        non_default_samples()
            .into_iter()
            .find(|(k, _)| *k == spec.key)
            .map(|(_, sample)| sample)
            .or_else(|| derived_sample(spec))
            .unwrap_or_else(|| panic!("no non-default sample for registry option {}", spec.key))
    }

    #[test]
    fn legacy_addresses_are_catalogued_and_resolvable() {
        for spec in options::LIVE_OPTIONS {
            assert!(
                legacy_addr_is_catalogued(spec.legacy_control_addr),
                "{}: legacy address missing from ALL_CONTROL",
                spec.key
            );
            assert!(options::find(spec.key).is_some(), "{}", spec.key);
            if let Some(addr) = legacy_addr_example(spec.legacy_control_addr) {
                assert!(
                    options::find_by_legacy_addr(&addr).is_some_and(|found| found.key == spec.key),
                    "{}: its legacy address resolves to another option",
                    spec.key
                );
            }
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
            if schema_entry["default"].is_null() {
                // The build's value (`OptionDefault::Build`): nothing fixed to
                // compare with.
                continue;
            }
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
                let raw = sample_for(spec);
                assert!(
                    (spec.set)(&mut live, &raw, &env(&control)).is_some(),
                    "{}: sample value rejected",
                    spec.key
                );
            }
        }
        let mut render = RenderConfig::default();
        {
            let live = control.live.read();
            options::store_live_to_config(&mut render, &live, &env(&control));
        }

        let fresh = fixture_control();
        {
            let mut live = fresh.live.write();
            options::seed_live_from_config(&mut live, &render, &env(&fresh));
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

    /// Every option: a real change lights the Save button, the same value
    /// again does not (docs/persistence-policy.md) — Studio re-sends values
    /// on reconnect, and a keepalive that dirtied the config once kept the
    /// Save button lit for good.
    #[test]
    fn every_option_dirties_on_a_change_and_only_then() {
        let dirty = |control: &Arc<RendererControl>| {
            control
                .config_dirty
                .load(std::sync::atomic::Ordering::Relaxed)
        };
        for spec in options::LIVE_OPTIONS {
            let control = fixture_control();
            let sample = sample_for(spec);
            let applied = options::apply_to_control(&control, spec, &sample).expect("accepted");
            assert!(applied.changed, "{}: the sample is not a change", spec.key);
            assert!(dirty(&control), "{}: a change must light Save", spec.key);
            control.mark_clean();
            let again = options::apply_to_control(&control, spec, &sample).expect("accepted");
            assert!(!again.changed, "{}", spec.key);
            assert!(
                !dirty(&control),
                "{}: the same value again lit Save",
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
        let mapping = options::find("output_channel_mapping").expect("registered");

        let epoch = control.options_epoch();
        let applied = options::apply_to_control(&control, placement, &RawOptionValue::Str("back"))
            .expect("accepted");
        assert_eq!(applied.canonical, "back");
        assert!(applied.changed);
        assert_eq!(control.options_epoch(), epoch + 1, "real change must bump");
        assert!(
            control
                .config_dirty
                .load(std::sync::atomic::Ordering::Relaxed),
            "a real change must mark the config dirty"
        );
        control.mark_clean();

        let again = options::apply_to_control(&control, placement, &RawOptionValue::Str("back"))
            .expect("accepted");
        assert!(!again.changed);
        assert!(
            !control
                .config_dirty
                .load(std::sync::atomic::Ordering::Relaxed),
            "a redundant re-send must not light the Save button"
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

        assert!(
            options::apply_to_control(&control, mapping, &RawOptionValue::Str("by_name")).is_some()
        );
        assert_eq!(
            control.options_epoch(),
            epoch + 1,
            "non-REPLAN option must not bump"
        );
        assert!(
            control
                .config_dirty
                .load(std::sync::atomic::Ordering::Relaxed),
            "a real change of a non-REPLAN option still marks the config dirty"
        );
    }

    /// A profile switch resets every row before seeding the incoming profile:
    /// the reset must land each one on the default the schema publishes.
    #[test]
    fn a_reset_puts_every_row_back_on_its_declared_default() {
        let control = fixture_control();
        let mut live = control.live.write();
        for spec in options::LIVE_OPTIONS {
            (spec.set)(&mut live, &sample_for(spec), &env(&control)).expect("sample accepted");
        }
        options::reset_live_to_defaults(&mut live, &env(&control));
        let schema: serde_json::Value =
            serde_json::from_str(&options::schema_json()).expect("valid schema");
        for (spec, entry) in options::LIVE_OPTIONS.iter().zip(schema.as_array().unwrap()) {
            if entry["default"].is_null() {
                continue; // left to the incoming profile's seed
            }
            assert_eq!(
                (spec.get_json)(&live),
                entry["default"],
                "{}: reset missed the declared default",
                spec.key
            );
        }
    }

    /// A batch over a group: every key applied, one dirty mark, one merged
    /// rebuild — and nothing at all when it changes nothing.
    #[test]
    fn a_room_batch_costs_one_topology_rebuild() {
        use renderer::options::Rebuild;
        let control = fixture_control();
        let ratio = options::find("room_ratio").expect("registered");
        let rear = options::find("room_ratio_rear").expect("registered");
        let items = [
            (ratio, RawOptionValue::Numbers(&[1.0, 3.0, 1.5])),
            (rear, RawOptionValue::Number(2.5)),
        ];
        let batch = options::apply_batch(&control, &items);
        assert!(batch.changed);
        assert_eq!(batch.rebuild, Rebuild::Topology);
        assert!(
            batch
                .results
                .iter()
                .all(|r| r.as_ref().is_some_and(|r| r.changed))
        );
        {
            let live = control.live.read();
            assert_eq!(live.room_ratio, [1.0, 3.0, 1.5]);
            assert_eq!(live.room_ratio_rear, 2.5);
        }
        control.mark_clean();

        let again = options::apply_batch(&control, &items);
        assert!(!again.changed);
        assert_eq!(again.rebuild, Rebuild::None);
        assert!(
            !control
                .config_dirty
                .load(std::sync::atomic::Ordering::Relaxed)
        );

        // A group without an effect of its own adds no rebuild to the batch.
        let ramp = options::find("ramp_mode").expect("registered");
        let batch = options::apply_batch(&control, &[(ramp, RawOptionValue::Str("interp"))]);
        assert!(batch.changed);
        assert_eq!(batch.rebuild, Rebuild::None);
    }

    /// The room reaches the file in metres against the layout radius, exactly
    /// as the save wrote it before the room joined the registry, and a
    /// config read back through `Config::load` seeds the same room.
    #[test]
    fn the_room_is_saved_in_metres_and_reloads_identically() {
        let control = fixture_control();
        {
            let mut live = control.live.write();
            live.room_ratio = [1.0, 1.8, 0.9];
            live.room_ratio_rear = 1.2;
            live.room_ratio_lower = 0.4;
            live.room_ratio_center_blend = 0.3;
        }
        let base = temp_path("room-base-missing");
        let out = temp_path("room-out");
        save_live_config_to_path(&control, None, &base, &out).expect("save");
        let yaml = std::fs::read_to_string(&out).expect("saved config readable");
        let config = Config::load_or_default(&out);
        let _ = std::fs::remove_file(&out);
        assert!(!yaml.contains("room_ratio:"), "legacy ratio key written");
        assert!(
            !yaml.contains("room_ratio_rear:"),
            "legacy rear key written"
        );
        let render = config.render.expect("render section");
        let radius = render.current_layout.as_ref().expect("layout").radius_m;
        let round6 = |v: f32| (v * 1_000_000.0).round() / 1_000_000.0;
        assert_eq!(render.room_width_m, Some(round6(2.0 * radius)));
        assert_eq!(render.room_front_m, Some(round6(1.8 * radius)));
        assert_eq!(render.room_height_m, Some(round6(0.9 * radius)));
        assert_eq!(render.room_rear_m, Some(round6(1.2 * radius)));
        assert_eq!(render.room_lower_m, Some(round6(0.4 * radius)));
        assert_eq!(render.room_ratio_center_blend, Some(0.3));

        let fresh = fixture_control();
        options::seed_live_from_config(&mut fresh.live.write(), &render, &env(&fresh));
        let live = fresh.live.read();
        let close = |a: f32, b: f32| (a - b).abs() < 1e-5;
        assert!(close(live.room_ratio[1], 1.8) && close(live.room_ratio[2], 0.9));
        assert!(close(live.room_ratio_rear, 1.2));
        assert!(close(live.room_ratio_lower, 0.4));
        assert!(close(live.room_ratio_center_blend, 0.3));
    }

    /// Every declared option accepts its sample through the setter and reports
    /// a canonical value; the setter rejects a shape no option accepts.
    #[test]
    fn setters_validate_and_report_canonical_values() {
        let control = fixture_control();
        let mut live = control.live.write();
        for spec in options::LIVE_OPTIONS {
            let raw = sample_for(spec);
            let canonical = (spec.set)(&mut live, &raw, &env(&control));
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
                    (spec.set)(
                        &mut live,
                        &RawOptionValue::Str("no_such_value_xyz"),
                        &env(&control)
                    )
                    .is_none(),
                    "{}: junk value accepted",
                    spec.key
                );
            }
        }
    }
}

#[test]
fn legacy_fixed_channel_options_migrate_without_reactivating_host_mode() {
    let control = fixture_control();
    let mut legacy = RenderConfig {
        channel_render_mode: Some(renderer::live_params::ChannelRenderMode::Host),
        options: renderer::options::DeclaredOptionsConfig {
            object_generator_id: Some("pad".to_string()),
            ..Default::default()
        },
        phantom_enabled: Some(true),
        ..Default::default()
    };
    legacy.phantom_params = Some(std::collections::HashMap::from([
        ("method".to_string(), 1.0),
        ("strength".to_string(), 0.75),
    ]));

    {
        let mut live = control.live.write();
        renderer::options::seed_live_from_config(&mut live, &legacy, &env(&control));
        assert_eq!(
            live.channel_render_mode,
            renderer::live_params::ChannelRenderMode::Spatial
        );
        assert!(live.options.synthetic_objects_enabled);
        assert_eq!(
            live.options.phantom_extract_mode,
            PhantomExtractMode::Spectral
        );
        renderer::options::store_live_to_config(&mut legacy, &live, &env(&control));
    }

    assert_eq!(legacy.channel_render_mode, None);
    assert_eq!(legacy.phantom_enabled, None);
    assert_eq!(legacy.options.synthetic_objects_enabled, Some(true));
    assert_eq!(
        legacy.options.phantom_extract_mode,
        Some(PhantomExtractMode::Spectral)
    );
    assert!(
        legacy
            .phantom_params
            .as_ref()
            .is_some_and(|params| !params.contains_key("method"))
    );
}

#[test]
fn disabled_synthesis_master_preserves_non_off_child_selections() {
    let control = fixture_control();
    let mut saved = RenderConfig::default();
    {
        let mut live = control.live.write();
        live.options.synthetic_objects_enabled = false;
        live.options.object_generator_id = "dirac".to_string();
        live.options.phantom_extract_mode = PhantomExtractMode::Broadband;
        renderer::options::store_live_to_config(&mut saved, &live, &env(&control));
    }
    assert_eq!(saved.options.synthetic_objects_enabled, Some(false));

    let restored = fixture_control();
    {
        let mut live = restored.live.write();
        renderer::options::seed_live_from_config(&mut live, &saved, &env(&restored));
        assert!(!live.options.synthetic_objects_enabled);
        assert_eq!(live.options.object_generator_id, "dirac");
        assert_eq!(
            live.options.phantom_extract_mode,
            PhantomExtractMode::Broadband
        );
    }
}

/// Config save persists every live option when non-default and normally omits
/// its key when default. The synthesized-object master is intentionally explicit
/// even when false so remembered child selections cannot reactivate on reload.
#[test]
fn config_save_covers_every_live_option_and_omits_defaults() {
    let control = fixture_control();
    let base = temp_path("base-missing");
    let out = temp_path("out");

    save_live_config_to_path(&control, None, &base, &out).expect("save at defaults");
    let yaml = std::fs::read_to_string(&out).expect("saved config readable");
    for row in HAND_WIRED_OPTIONS {
        if row.key == "synthetic_objects_enabled" {
            assert!(yaml.contains("synthetic_objects_enabled: false"));
            continue;
        }
        assert!(
            !yaml.contains(&format!("{}:", row.key)),
            "{}: default value should keep the key out of the saved config",
            row.key
        );
    }

    {
        let mut live = control.live.write();
        for row in HAND_WIRED_OPTIONS {
            (row.set_non_default)(&mut live);
        }
    }
    save_live_config_to_path(&control, None, &base, &out).expect("save non-defaults");
    let config = Config::load_or_default(&out);
    let render = config.render.expect("saved config has a render section");
    for row in HAND_WIRED_OPTIONS {
        assert!(
            (row.config_reflects)(&render),
            "{}: non-default value did not round-trip through the saved config",
            row.key
        );
    }

    let _ = std::fs::remove_file(&out);
}

/// The plugin parameter values — the object generators' and the phantom
/// stage's, beside the backends' — are on every layer: the two control
/// addresses, the snapshot (per plugin), and the saved config at their new
/// keys, the legacy ones migrated and dropped without losing a value or a key
/// the file had that nothing reads.
#[test]
fn plugin_params_reach_the_catalogue_the_snapshot_and_the_saved_config() {
    use renderer::backend_params::ParamValue;
    use renderer::plugin::{PHANTOM_EXTRACT_ID, PluginKind, PluginParams};
    for addr in [
        osc_contract::CONTROL_OBJECT_GENERATOR_PARAM,
        osc_contract::CONTROL_PHANTOM_EXTRACT_PARAM,
        osc_contract::CONTROL_BACKEND_PARAM,
    ] {
        assert!(osc_contract::ALL_CONTROL.contains(&addr), "{addr}");
    }

    let control = fixture_control();
    let at_defaults = snapshot_json(&control);
    assert_eq!(
        at_defaults["objectGeneratorParamValuesById"],
        serde_json::json!({})
    );
    assert_eq!(at_defaults["phantomParamValues"], serde_json::json!({}));

    // A config written before the store: one flat map for the selected
    // generator, a float-only phantom map with the old method entry, and a
    // key of its own nothing here reads.
    let base = temp_path("plugin-legacy");
    let out = temp_path("plugin-out");
    std::fs::write(
        &base,
        "render:\n  object_generator_id: pad\n  object_generator_params:\n    strength: 0.8\n    \
         hpf_hz: 400.0\n  phantom_params:\n    method: 1.0\n    center: 1.0\n    passes: 2.0\n  \
         some_future_key: kept\n",
    )
    .unwrap();
    let legacy = Config::load_or_default(&base).render.unwrap();
    control.seed_plugin_params(PluginParams::from_config(&legacy));
    control.live.write().options.object_generator_id = "pad".to_string();

    let snapshot = snapshot_json(&control);
    assert_eq!(
        snapshot["objectGeneratorParamValuesById"]["pad"]["strength"],
        serde_json::json!(0.8f32)
    );
    assert_eq!(snapshot["phantomParamValues"]["passes"], 2.0);

    save_live_config_to_path(&control, None, &base, &out).expect("save");
    let yaml = std::fs::read_to_string(&out).unwrap();
    assert!(!yaml.contains("object_generator_params"), "{yaml}");
    assert!(!yaml.contains("phantom_params:"), "{yaml}");
    assert!(yaml.contains("some_future_key: kept"), "{yaml}");
    let saved = Config::load_or_default(&out).render.unwrap();
    let reloaded = PluginParams::from_config(&saved);
    assert_eq!(
        reloaded.get(PluginKind::ObjectGenerator, "pad", "strength"),
        Some(&ParamValue::Float(0.8))
    );
    assert_eq!(
        reloaded.get(PluginKind::ObjectGenerator, "pad", "hpf_hz"),
        Some(&ParamValue::Float(400.0))
    );
    let phantom = reloaded
        .plugin(PluginKind::PhantomExtract, PHANTOM_EXTRACT_ID)
        .unwrap();
    assert_eq!(phantom.len(), 2, "center and passes, without the method");
    assert_eq!(phantom["center"], ParamValue::Float(1.0));
    // A second save of the reloaded file writes the same values.
    assert_eq!(reloaded, control.plugin_params());

    let _ = std::fs::remove_file(&base);
    let _ = std::fs::remove_file(&out);
}

/// The snapshot `spread` block reports what the VBAP backend applies. The
/// spread addresses write the "vbap" param bag, not the live fields, so a block
/// built from the live fields went stale on the first edit.
#[test]
fn snapshot_spread_block_follows_the_vbap_param_bag() {
    use renderer::backend_params::ParamValue;
    let control = fixture_control();
    let live_min = control.live.read().spread_min;
    control.set_backend_param("vbap", "spread_min", ParamValue::Float(live_min + 0.25));
    control.set_backend_param("vbap", "spread_from_distance", ParamValue::Bool(true));
    control.set_backend_param(
        "vbap",
        "size_to_spread_mode",
        ParamValue::Text("mean".into()),
    );
    let spread = &snapshot_json(&control)["spread"];
    assert_eq!(spread["min"], serde_json::json!(live_min + 0.25));
    assert_eq!(spread["fromDistance"], true);
    assert_eq!(spread["sizeToSpreadMode"], "mean");
    // Unset keys keep the live fallback, as in the backend build.
    assert_eq!(
        spread["max"],
        serde_json::json!(control.live.read().spread_max)
    );
}

/// Hosts scope the options: a host with audio I/O leaves the embedded
/// engine's out of what it publishes and saves, and publishes its own.
mod host_scope {
    use super::*;
    use rosc::{OscMessage, OscPacket, OscType};
    use runtime_control::HostControlHandler;
    use runtime_control::osc::ControlEffects;

    /// A host declaring one option, `stub_rate`.
    struct StubHost;

    impl HostControlHandler for StubHost {
        fn handle(&self, _addr: &str, _msg: &OscMessage) -> Option<ControlEffects> {
            None
        }
        fn extend_snapshot(&self) -> Vec<OscPacket> {
            Vec::new()
        }
        fn amend_saved_config(&self, _render: &mut RenderConfig) {}
        fn options_schema(&self) -> Vec<serde_json::Value> {
            vec![serde_json::json!({"key": "stub_rate", "kind": "optional_int"})]
        }
        fn options_json(&self) -> serde_json::Map<String, serde_json::Value> {
            [("stub_rate".to_string(), serde_json::json!(48_000))]
                .into_iter()
                .collect()
        }
        fn options_applied_json(&self) -> serde_json::Map<String, serde_json::Value> {
            [("stub_rate".to_string(), serde_json::Value::Null)]
                .into_iter()
                .collect()
        }
        fn option_groups_pending(&self) -> Vec<(&'static str, bool)> {
            vec![("stub", true)]
        }
    }

    fn published(
        control: &Arc<RendererControl>,
        host: Option<&dyn HostControlHandler>,
    ) -> std::collections::HashMap<String, serde_json::Value> {
        runtime_control::snapshot::build_live_state_bundle_with_host(
            control,
            host.is_some(),
            host.is_some(),
            host,
        )
        .into_iter()
        .filter_map(|packet| match packet {
            OscPacket::Message(OscMessage { addr, args }) => match args.first() {
                Some(OscType::String(json)) => {
                    serde_json::from_str(json).ok().map(|value| (addr, value))
                }
                _ => None,
            },
            OscPacket::Bundle(_) => None,
        })
        .collect()
    }

    fn schema_keys(state: &std::collections::HashMap<String, serde_json::Value>) -> Vec<String> {
        state[osc_contract::STATE_OPTIONS_SCHEMA]
            .as_array()
            .expect("schema array")
            .iter()
            .map(|entry| entry["key"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn each_host_publishes_what_it_offers() {
        let control = fixture_control();
        let embedded = published(&control, None);
        let keys = schema_keys(&embedded);
        assert!(keys.iter().any(|k| k == "decode_thread"));
        assert!(!keys.iter().any(|k| k == "stub_rate"));
        assert!(!embedded.contains_key(osc_contract::STATE_HOST_OPTIONS));

        let standalone = published(&control, Some(&StubHost));
        let keys = schema_keys(&standalone);
        assert!(!keys.iter().any(|k| k == "decode_thread"));
        assert_eq!(keys.last().map(String::as_str), Some("stub_rate"));
        let host_options = &standalone[osc_contract::STATE_HOST_OPTIONS];
        assert_eq!(host_options["options"]["stub_rate"], 48_000);
        assert!(host_options["applied"]["stub_rate"].is_null());
        assert_eq!(host_options["pending"]["stub"], true);
    }

    /// A save by the standalone renderer keeps the embedded engine's
    /// `decode_thread` as the file has it (the two share the config); the
    /// embedded engine writes its own.
    #[test]
    fn a_host_save_keeps_the_options_it_does_not_offer() {
        let control = fixture_control();
        let base = temp_path("scope-base");
        let out = temp_path("scope-out");
        let mut config = Config::default();
        config.render = Some(RenderConfig {
            options: renderer::options::DeclaredOptionsConfig {
                decode_thread: Some(true),
                ..Default::default()
            },
            ..Default::default()
        });
        config.save(&base).expect("base written");
        assert!(!control.live.read().options.decode_thread);

        save_live_config_to_path(&control, Some(&StubHost), &base, &out).expect("save");
        let saved = Config::load_or_default(&out).render.expect("render");
        assert_eq!(
            saved.options.decode_thread,
            Some(true),
            "the file's value survives"
        );

        save_live_config_to_path(&control, None, &base, &out).expect("save");
        let saved = Config::load_or_default(&out).render.expect("render");
        assert_ne!(
            saved.options.decode_thread,
            Some(true),
            "the embedded engine writes its own"
        );

        let _ = std::fs::remove_file(&base);
        let _ = std::fs::remove_file(&out);
    }
}

/// `config.yaml` is edited by hand and copied between machines: a value in it
/// is no more trusted than one on the wire. Every option, written into the
/// file with a value its kind does not allow, is either refused when the file
/// is parsed or reaches the live state inside the kind's bounds — the same
/// bounds the OSC setter enforces.
mod hostile_config_values {
    use super::*;
    use renderer::options::{self, OptionKind};

    /// YAML spellings a hand edit or another build could leave for `kind`.
    fn hostile(kind: OptionKind) -> &'static [&'static str] {
        match kind {
            OptionKind::Float { .. } | OptionKind::FloatArray { .. } => &[
                ".nan",
                ".inf",
                "-.inf",
                "1e30",
                "-1e30",
                "[.nan, .nan, .nan]",
                "[1e30, -1e30, .inf]",
                "\"loud\"",
            ],
            OptionKind::Int { .. } | OptionKind::OptionalInt { .. } => &[
                "-5",
                "0",
                "4.5",
                "9223372036854775807",
                "-9223372036854775808",
                "\"many\"",
                ".nan",
            ],
            OptionKind::Bool => &["2", "\"maybe\"", "[true]", ".nan"],
            OptionKind::Enum(_) | OptionKind::DynamicEnum { .. } | OptionKind::Str => {
                &["\"no_such_value\"", "7", "[a, b]", "{a: 1}", "\"\""]
            }
        }
    }

    /// Whether `value`, as `get_json` reports it, is one `kind` allows.
    fn within(kind: OptionKind, value: &serde_json::Value) -> bool {
        let number_in = |v: &serde_json::Value, min: f64, max: f64| {
            v.as_f64()
                .is_some_and(|x| x.is_finite() && x >= min && x <= max)
        };
        match kind {
            OptionKind::Float { min, max, .. } => number_in(value, min as f64, max as f64),
            OptionKind::FloatArray { len, min, max, .. } => value.as_array().is_some_and(|a| {
                a.len() == len && a.iter().all(|v| number_in(v, min as f64, max as f64))
            }),
            OptionKind::Int { min, max } => number_in(value, min as f64, max as f64),
            OptionKind::OptionalInt { min, max } => {
                value.is_null() || number_in(value, min as f64, max as f64)
            }
            OptionKind::Bool => value.is_boolean(),
            OptionKind::Enum(allowed) => value.as_str().is_some_and(|s| allowed.contains(&s)),
            // Validated against the running host; a string is all that is
            // promised here.
            OptionKind::DynamicEnum { .. } | OptionKind::Str => value.is_string(),
        }
    }

    /// The seed's guard takes a value its kind does not admit for a hostile
    /// one: every value a fresh control holds, and every sample the setters
    /// accept, must be admitted, or a valid config would be rewritten at boot.
    #[test]
    fn every_legitimate_value_is_admitted_by_its_kind() {
        let control = fixture_control();
        let mut live = control.live.write();
        for spec in options::LIVE_OPTIONS {
            let value = (spec.get_json)(&live);
            assert!(spec.kind.admits(&value), "{}: default {value}", spec.key);
            assert!(within(spec.kind, &value), "{}: default {value}", spec.key);
            (spec.set)(
                &mut live,
                &super::registry::sample_for(spec),
                &env(&control),
            )
            .expect("sample");
            let value = (spec.get_json)(&live);
            assert!(spec.kind.admits(&value), "{}: sample {value}", spec.key);
        }
    }

    #[test]
    fn a_hostile_value_in_the_file_is_refused_or_bounded() {
        let mut seeded = 0;
        let mut refused = 0;
        let mut violations = Vec::new();
        // One control, its live state put back before each case: building a
        // renderer per case would make this the slowest test of the suite.
        let control = fixture_control();
        let pristine = control.live.read().clone();
        for spec in options::LIVE_OPTIONS {
            for value in hostile(spec.kind) {
                let yaml = format!("render:\n  {}: {}\n", spec.key, value);
                let Ok(config) = serde_yaml_ng::from_str::<Config>(&yaml) else {
                    // Refused at parse: the file loads as a parse error, and
                    // nothing reaches the live state.
                    refused += 1;
                    continue;
                };
                let render = config.render.unwrap_or_default();
                *control.live.write() = pristine.clone();
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut live = control.live.write();
                    options::seed_live_from_config(&mut live, &render, &env(&control));
                    (spec.get_json)(&live)
                }));
                match outcome {
                    Err(_) => violations.push(format!("{}: {value} panicked the seed", spec.key)),
                    Ok(got) if !within(spec.kind, &got) => {
                        violations.push(format!("{}: {value} seeded as {got}", spec.key))
                    }
                    Ok(_) => seeded += 1,
                }
            }
        }
        assert!(
            violations.is_empty(),
            "values out of their option's bounds reached the live state from config.yaml:\n{}",
            violations.join("\n")
        );
        // Both outcomes must actually occur, or the sweep is not reaching the
        // seed (a key spelt differently in the file, say).
        assert!(
            seeded > 50 && refused > 50,
            "seeded {seeded}, refused {refused}"
        );
    }
}
