//! `render.evaluation_grid` through a boot and a Save (docs/multi-bridge.md,
//! "Evaluation grid"): a config from before the key is migrated once, by the
//! conservative rule, against the first bridge's hint; a forced grid renders
//! as before whatever bridge comes next, and its first Save writes the key
//! and a concrete mode.

use orender_engine::renderer_build::{
    SpatialRendererParams, build_spatial_renderer, seed_runtime_state_from_render_config,
};
use renderer::config::{Config, RenderConfig};
use renderer::evaluation_grid::{EvaluationGrid, EvaluationGridSource, settle_config};
use renderer::live_params::{LiveEvaluationMode, RendererControl};
use renderer::spatial_renderer::SpatialRenderer;
use renderer::speaker_layout::SpeakerLayout;
use std::sync::Arc;

/// Bridge A's hint: 9 × 9 × 5 Cartesian cells, nothing below the floor.
const A: bridge_api::RVbapCartesianDefaults = bridge_api::RVbapCartesianDefaults {
    x_size: 9,
    y_size: 9,
    z_size: 5,
    z_neg_size: 0,
    allow_negative_z: false,
};

/// Bridge B's: another grid, below the floor, on a polar table.
const B: bridge_api::RVbapCartesianDefaults = bridge_api::RVbapCartesianDefaults {
    x_size: 13,
    y_size: 11,
    z_size: 7,
    z_neg_size: 2,
    allow_negative_z: true,
};

fn hint_a() -> EvaluationGrid {
    EvaluationGrid::from_hint(A, bridge_api::RVbapTableMode::Cartesian)
}

fn hint_b() -> EvaluationGrid {
    EvaluationGrid::from_hint(B, bridge_api::RVbapTableMode::Polar)
}

/// What a host does once its bridges are loaded, the first one hinting
/// `hint`: settle the config, build the renderer on it, seed it, and mark a
/// migration unsaved.
fn boot(
    yaml: &str,
    hint: bridge_api::RVbapCartesianDefaults,
    preferred: bridge_api::RVbapTableMode,
) -> (SpatialRenderer, Arc<RendererControl>, Config) {
    let config: Config = serde_yaml_ng::from_str(yaml).expect("config");
    let mut render = config.render.clone().unwrap_or_default();
    let grid = settle_config(&mut render, EvaluationGrid::from_hint(hint, preferred));
    let renderer = build_spatial_renderer(
        &SpatialRendererParams::from_render_config(Some(&render)),
        SpeakerLayout::preset("7.1.4").expect("preset layout"),
        48_000,
        hint,
        preferred,
        Some(&render),
    )
    .expect("renderer");
    let control = renderer.renderer_control();
    seed_runtime_state_from_render_config(&control, Some(&render));
    if grid.migrated {
        control.mark_dirty();
    }
    (renderer, control, config)
}

fn dirty(control: &RendererControl) -> bool {
    control
        .config_dirty
        .load(std::sync::atomic::Ordering::Relaxed)
}

/// What a Save writes into `config`'s render section.
fn save(control: &Arc<RendererControl>, mut config: Config) -> RenderConfig {
    runtime_control::persist::store_live_into_config(control, None, &mut config);
    config.render.expect("render section")
}

/// A stream of bridge B plays: its hint is offered and taken, and requested
/// when the grid follows the bridge (what the listener or the layout
/// follower does).
fn stream_from_b(control: &RendererControl) {
    assert!(control.offer_bridge_grid(hint_b()));
    if control.take_bridge_grid() {
        control.request_live_grid();
    }
}

#[test]
fn auto_with_the_bridges_own_sizes_follows_the_bridge_and_saves_no_grid() {
    // What Save wrote before the key existed: the sizes in force, on `auto`.
    let (_renderer, control, config) = boot(
        "render:\n  render_evaluation_mode: auto\n  evaluation_cartesian_x_size: 9\n  \
         evaluation_cartesian_y_size: 9\n  evaluation_cartesian_z_size: 5\n  \
         evaluation_cartesian_z_neg_size: 0\n",
        A,
        bridge_api::RVbapTableMode::Cartesian,
    );
    assert!(dirty(&control), "a migration is unsaved");
    assert_eq!(
        control.live.read().evaluation.source,
        EvaluationGridSource::Bridge
    );
    assert_eq!(control.live_grid(), hint_a());

    let saved = save(&control, config);
    assert_eq!(saved.evaluation_grid.as_deref(), Some("bridge"));
    assert_eq!(saved.render_evaluation_mode, None);
    assert_eq!(saved.evaluation_cartesian_x_size, None);
    assert_eq!(saved.vbap_allow_negative_z, None);

    // It now follows the next stream's bridge.
    stream_from_b(&control);
    assert_eq!(control.live_grid(), hint_b());
}

#[test]
fn a_chosen_grid_stays_forced_and_renders_as_before_whatever_the_bridge() {
    for (name, yaml, expected) in [
        (
            "polar",
            "render:\n  render_evaluation_mode: precomputed_polar\n",
            EvaluationGrid {
                mode: LiveEvaluationMode::PrecomputedPolar,
                ..hint_a()
            },
        ),
        (
            "realtime",
            "render:\n  render_evaluation_mode: realtime\n",
            EvaluationGrid {
                mode: LiveEvaluationMode::Realtime,
                ..hint_a()
            },
        ),
        (
            "reduced grid on auto",
            "render:\n  render_evaluation_mode: auto\n  evaluation_cartesian_x_size: 4\n",
            EvaluationGrid {
                cartesian: renderer::live_params::CartesianEvaluationParams {
                    x_size: 4,
                    ..hint_a().cartesian
                },
                ..hint_a()
            },
        ),
        (
            "cartesian equal to the hint",
            "render:\n  render_evaluation_mode: precomputed_cartesian\n  \
             evaluation_cartesian_x_size: 9\n  evaluation_cartesian_y_size: 9\n  \
             evaluation_cartesian_z_size: 5\n",
            hint_a(),
        ),
    ] {
        let (_renderer, control, config) = boot(yaml, A, bridge_api::RVbapTableMode::Cartesian);
        assert!(dirty(&control), "{name}");
        assert_eq!(
            control.live.read().evaluation.source,
            EvaluationGridSource::Custom,
            "{name}"
        );
        assert_eq!(control.live_grid(), expected, "{name}");
        if expected.mode != LiveEvaluationMode::Realtime {
            // Built on it.
            assert!(
                control
                    .active_topology()
                    .grid
                    .is_some_and(|grid| grid.same_table(&expected)),
                "{name}"
            );
        }

        // Bridge B's stream changes nothing.
        let topology = control.active_topology();
        stream_from_b(&control);
        assert_eq!(control.live_grid(), expected, "{name}");
        assert!(Arc::ptr_eq(&topology, &control.active_topology()), "{name}");
        assert_eq!(
            control.live.read().evaluation.bridge_hint,
            Some(hint_b()),
            "{name}: the bridge's grid is still published"
        );

        // The first Save writes the key, a concrete mode and the whole grid.
        let saved = save(&control, config);
        assert_eq!(saved.evaluation_grid.as_deref(), Some("custom"), "{name}");
        assert_eq!(
            saved.render_evaluation_mode.as_deref(),
            Some(expected.mode.as_str()),
            "{name}"
        );
        assert_eq!(
            saved.evaluation_cartesian_x_size,
            Some(expected.cartesian.x_size),
            "{name}"
        );
        assert_eq!(
            saved.vbap_allow_negative_z,
            Some(expected.allow_negative_z),
            "{name}"
        );

        // Saved, then restarted with bridge B loaded first: the same grid.
        let yaml = serde_yaml_ng::to_string(&Config {
            render: Some(saved),
            ..Config::default()
        })
        .expect("yaml");
        let (_renderer, control, _) = boot(&yaml, B, bridge_api::RVbapTableMode::Polar);
        assert!(!dirty(&control), "{name}: migrated once");
        assert_eq!(control.live_grid(), expected, "{name}");
    }
}

#[test]
fn an_explicit_negative_z_is_custom_only_when_it_differs_from_the_hint() {
    for (stored, hinted, source) in [
        (true, false, EvaluationGridSource::Custom),
        (false, true, EvaluationGridSource::Custom),
        (true, true, EvaluationGridSource::Bridge),
        (false, false, EvaluationGridSource::Bridge),
    ] {
        let hint = bridge_api::RVbapCartesianDefaults {
            allow_negative_z: hinted,
            ..A
        };
        let (_renderer, control, config) = boot(
            &format!(
                "render:\n  render_evaluation_mode: auto\n  vbap_allow_negative_z: {stored}\n"
            ),
            hint,
            bridge_api::RVbapTableMode::Cartesian,
        );
        let live_source = control.live.read().evaluation.source;
        assert_eq!(live_source, source, "stored {stored}, hinted {hinted}");
        assert_eq!(control.live.read().evaluation.allow_negative_z, stored);
        let saved = save(&control, config);
        assert_eq!(saved.evaluation_grid.as_deref(), Some(source.as_str()));
        match source {
            EvaluationGridSource::Custom => {
                assert_eq!(saved.vbap_allow_negative_z, Some(stored));
                assert_eq!(
                    saved.render_evaluation_mode.as_deref(),
                    Some("precomputed_cartesian"),
                    "`auto` resolved"
                );
            }
            EvaluationGridSource::Bridge => assert_eq!(saved.vbap_allow_negative_z, None),
        }
    }
}

#[test]
fn a_config_with_the_key_is_not_migrated_and_stays_clean() {
    let (_renderer, control, _) = boot(
        "render:\n  evaluation_grid: bridge\n  evaluation_cartesian_x_size: 4\n",
        A,
        bridge_api::RVbapTableMode::Cartesian,
    );
    assert!(!dirty(&control));
    // The stale size is ignored: the grid is the bridge's.
    assert_eq!(control.live_grid(), hint_a());
}

/// A renderer that loaded no bridge (the standby runtime) cannot migrate:
/// its Save keeps the grid keys as the file has them, key absent included,
/// for the next start with a bridge.
#[test]
fn a_renderer_without_a_bridge_saves_the_grid_as_loaded() {
    let yaml = "render:\n  render_evaluation_mode: auto\n  evaluation_cartesian_x_size: 33\n";
    let config: Config = serde_yaml_ng::from_str(yaml).expect("config");
    let render = config.render.clone().expect("render");
    let renderer = build_spatial_renderer(
        &SpatialRendererParams::from_render_config(Some(&render)),
        SpeakerLayout::preset("7.1.4").expect("preset layout"),
        48_000,
        orender_engine::degraded::NO_BRIDGE_VBAP_DEFAULTS,
        orender_engine::degraded::NO_BRIDGE_PREFERRED_MODE,
        Some(&render),
    )
    .expect("renderer");
    let control = renderer.renderer_control();
    control.keep_grid_as_loaded();
    assert!(!dirty(&control));
    let saved = save(&control, config);
    assert_eq!(saved.evaluation_grid, None);
    assert_eq!(saved.render_evaluation_mode.as_deref(), Some("auto"));
    assert_eq!(saved.evaluation_cartesian_x_size, Some(33));
}
