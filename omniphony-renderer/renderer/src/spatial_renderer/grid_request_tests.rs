//! The evaluation grid follows the active bridge, or is forced
//! (`crate::evaluation_grid`): what the speaker stage installs when grid
//! requests change their mind while a table is being built.

use super::*;
use crate::evaluation_grid::{EvaluationGrid, EvaluationGridSource, GridDecision};
use crate::live_params::LiveEvaluationMode;
use crate::speaker_layout::SpeakerLayout;
use crate::test_support;
use bridge_api::{RVbapCartesianDefaults, RVbapTableMode};

/// A Cartesian hint of `x` × 5 × 3 (+ 3) cells, as the small grid spec's.
fn hint(x: u32) -> EvaluationGrid {
    EvaluationGrid::from_hint(
        RVbapCartesianDefaults {
            x_size: x,
            y_size: 5,
            z_size: 3,
            z_neg_size: 3,
            allow_negative_z: false,
        },
        RVbapTableMode::Cartesian,
    )
}

/// A renderer built on bridge A's hint (5 cells across) with the grid
/// following the bridge, its bands installed — what a host's build leaves.
/// `own_follower`: whether the renderer's layout follower takes the hints
/// (no host does); otherwise the test drives the control itself.
fn renderer_following_the_bridge(own_follower: bool) -> SpatialRenderer {
    let layout = SpeakerLayout::preset("7.1.4").expect("7.1.4 preset");
    let mut r = SpatialRenderer::new(test_support::small_grid_spec(layout)).expect("renderer");
    let control = r.renderer_control();
    control.seed_bridge_grid(hint(5));
    {
        let mut live = control.live.write();
        live.evaluation.source = EvaluationGridSource::Bridge;
        hint(5).apply(&mut live);
    }
    control.set_relayout_by_host(!own_follower);
    r.prepare_speaker_stage().expect("bands");
    r
}

const PCM: [f32; 40] = [0.25; 40];

fn frame(r: &mut SpatialRenderer) {
    r.render_frame(&PCM, 1, &[], Vec::new(), false)
        .expect("render");
}

/// Frames until `done`, or panic after a minute.
fn render_until(r: &mut SpatialRenderer, what: &str, done: impl Fn(&SpatialRenderer) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        frame(r);
        if done(r) {
            return;
        }
        assert!(std::time::Instant::now() < deadline, "{what}: never");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// The grid of the bands the stage renders with.
fn installed_grid(r: &SpatialRenderer) -> Option<EvaluationGrid> {
    r.speaker_stage
        .installed_topology()
        .and_then(|topology| topology.grid)
}

/// Build and publish the topology of the live grid, as a host's recompute
/// does for a grid-only request.
fn publish_grid_only_rebuild(control: &RendererControl) -> bool {
    let from = control.active_topology();
    let plan = control.prepare_topology_rebuild().expect("plan");
    let topology = plan.build_topology_reusing(Some(&from)).expect("topology");
    control.publish_topology_if_current(topology, Some(from))
}

/// A stream whose bridge hints another grid is rendered on it after one
/// rebuild, done off the render thread; the same hint again rebuilds
/// nothing.
#[test]
fn a_new_hint_rebuilds_the_grid_once_and_the_same_hint_never() {
    let mut r = renderer_following_the_bridge(true);
    let control = r.renderer_control();
    let builds = r.speaker_stage_builds();

    // The hint the renderer was built on: nothing to take.
    assert!(control.offer_bridge_grid(hint(5)));
    assert!(!control.bridge_grid_pending());

    assert!(control.offer_bridge_grid(hint(7)));
    assert!(control.bridge_grid_pending());
    render_until(&mut r, "the new grid installed", |r| {
        installed_grid(r) == Some(hint(7)) && !r.speaker_stage_rebuild_pending()
    });
    assert_eq!(r.speaker_stage_builds(), builds + 1, "one rebuild");
    assert_eq!(control.live.read().evaluation.cartesian.x_size, 7);
    assert_eq!(control.live.read().evaluation.bridge_hint, Some(hint(7)));

    // The next stream of that bridge: same hint, nothing moves.
    let topology = control.active_topology();
    assert!(control.offer_bridge_grid(hint(7)));
    assert!(!control.bridge_grid_pending());
    for _ in 0..8 {
        frame(&mut r);
    }
    assert!(Arc::ptr_eq(&topology, &control.active_topology()));
    assert_eq!(r.speaker_stage_builds(), builds + 1);
}

/// A forced grid ignores the bridges: a stream with other hints rebuilds
/// nothing, and the hint is still published for the clients.
#[test]
fn a_forced_grid_does_not_follow_a_new_hint() {
    let mut r = renderer_following_the_bridge(true);
    let control = r.renderer_control();
    control.live.write().evaluation.source = EvaluationGridSource::Custom;
    let topology = control.active_topology();
    let builds = r.speaker_stage_builds();

    assert!(control.offer_bridge_grid(hint(7)));
    // Taken is not yet published: `take_bridge_grid` marks the hint taken
    // before it writes it to the live params, so wait for what the
    // assertions read.
    render_until(&mut r, "the hint published", |_| {
        !control.bridge_grid_pending()
            && control.live.read().evaluation.bridge_hint == Some(hint(7))
    });
    for _ in 0..8 {
        frame(&mut r);
    }
    assert!(Arc::ptr_eq(&topology, &control.active_topology()));
    assert_eq!(r.speaker_stage_builds(), builds);
    let live = control.live.read();
    assert_eq!(live.evaluation.cartesian.x_size, 5);
    assert_eq!(live.evaluation.bridge_hint, Some(hint(7)));
}

/// A → B → A while B's bands are built: the stage keeps A's, installs no B,
/// and nothing is built again for A.
#[test]
fn a_hint_that_comes_back_while_the_new_bands_build_ends_on_the_installed_ones() {
    let mut r = renderer_following_the_bridge(false);
    let control = r.renderer_control();
    let builds = r.speaker_stage_builds();
    let a = control.active_topology();

    assert!(control.offer_bridge_grid(hint(7)));
    assert!(control.take_bridge_grid());
    assert_eq!(control.request_live_grid(), GridDecision::Rebuild);
    assert!(publish_grid_only_rebuild(&control));
    // The stage asks its worker for B's bands…
    frame(&mut r);
    assert!(r.speaker_stage_rebuild_pending());

    // …and A's hint comes back before they land: A is in force again, as it
    // was.
    assert!(control.offer_bridge_grid(hint(5)));
    assert!(control.take_bridge_grid());
    assert_eq!(control.request_live_grid(), GridDecision::Adopted);
    assert!(Arc::ptr_eq(&a, &control.active_topology()));

    render_until(&mut r, "B's set answered", |r| {
        !r.speaker_stage_rebuild_pending()
    });
    assert_eq!(r.speaker_stage_builds(), builds, "no B, no second A");
    assert_eq!(installed_grid(&r), Some(hint(5)));
    assert!(control.topology_grid_is_current(&control.active_topology()));
}

/// Forcing the grid while the bridge's new one is being built starts from
/// the table in force, the installed one: B's bands are not installed, and
/// nothing moves until the forced grid is edited.
#[test]
fn forcing_the_grid_while_the_bridges_one_builds_keeps_the_installed_table() {
    let mut r = renderer_following_the_bridge(false);
    let control = r.renderer_control();
    let builds = r.speaker_stage_builds();

    assert!(control.offer_bridge_grid(hint(7)));
    assert!(control.take_bridge_grid());
    assert_eq!(control.request_live_grid(), GridDecision::Rebuild);
    assert!(publish_grid_only_rebuild(&control));
    frame(&mut r);
    assert!(r.speaker_stage_rebuild_pending());

    let spec = crate::options::find("evaluation_grid").expect("row");
    let applied = crate::options::apply_to_control(
        &control,
        spec,
        &crate::options::RawOptionValue::Str("custom"),
    )
    .expect("accepted");
    assert!(applied.changed);
    assert_eq!(control.live_grid(), hint(5), "the installed grid, not B");
    assert_ne!(
        control.live.read().evaluation.mode,
        LiveEvaluationMode::Auto
    );
    assert_eq!(control.request_live_grid(), GridDecision::Adopted);

    render_until(&mut r, "B's set answered", |r| {
        !r.speaker_stage_rebuild_pending()
    });
    assert_eq!(r.speaker_stage_builds(), builds);
    assert_eq!(installed_grid(&r), Some(hint(5)));
}

/// A request that overtakes a published topology before its bands land
/// (C after B, both rebuilt): B's bands are dropped when they arrive, not
/// installed, and not asked for again; C's are, once published.
#[test]
fn the_bands_of_an_overtaken_topology_are_not_installed() {
    let mut r = renderer_following_the_bridge(false);
    let control = r.renderer_control();
    let builds = r.speaker_stage_builds();

    assert!(control.offer_bridge_grid(hint(7)));
    assert!(control.take_bridge_grid());
    assert_eq!(control.request_live_grid(), GridDecision::Rebuild);
    assert!(publish_grid_only_rebuild(&control));
    frame(&mut r);
    assert!(r.speaker_stage_rebuild_pending());

    // C, which needs its own rebuild: B is still published, but no longer
    // the answer.
    assert!(control.offer_bridge_grid(hint(9)));
    assert!(control.take_bridge_grid());
    assert_eq!(control.request_live_grid(), GridDecision::Rebuild);
    assert!(!control.topology_grid_is_current(&control.active_topology()));
    render_until(&mut r, "B's set answered", |r| {
        !r.speaker_stage_rebuild_pending()
    });
    for _ in 0..4 {
        frame(&mut r);
        assert!(!r.speaker_stage_rebuild_pending(), "B is not asked again");
    }
    assert_eq!(r.speaker_stage_builds(), builds);
    assert_eq!(installed_grid(&r), Some(hint(5)));

    // C's own topology is published, and its bands installed.
    assert!(publish_grid_only_rebuild(&control));
    render_until(&mut r, "C installed", |r| {
        installed_grid(r) == Some(hint(9)) && !r.speaker_stage_rebuild_pending()
    });
    assert_eq!(r.speaker_stage_builds(), builds + 1);
}

/// A topology planned before a later grid request is not published.
#[test]
fn a_topology_planned_before_a_later_request_is_not_published() {
    let r = renderer_following_the_bridge(false);
    let control = r.renderer_control();
    let a = control.active_topology();
    assert!(control.offer_bridge_grid(hint(7)));
    assert!(control.take_bridge_grid());
    assert_eq!(control.request_live_grid(), GridDecision::Rebuild);
    let plan = control.prepare_topology_rebuild().expect("plan");
    let b = plan.build_topology_reusing(Some(&a)).expect("topology");
    // A switch to a forced grid lands before B is published.
    {
        let mut live = control.live.write();
        live.evaluation.source = EvaluationGridSource::Custom;
        hint(5).apply(&mut live);
    }
    assert_eq!(control.request_live_grid(), GridDecision::Adopted);
    assert!(!control.publish_topology_if_current(b, Some(Arc::clone(&a))));
    assert!(Arc::ptr_eq(&a, &control.active_topology()));
}
