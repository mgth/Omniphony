//! Where the evaluation grid comes from: the active bridge's hint, or the
//! user's (`docs/multi-bridge.md`, "Evaluation grid: follow the bridge, or
//! forced").
//!
//! The VBAP gain table is sampled on a grid: an evaluation mode, the
//! Cartesian cell counts and whether positions below the floor are rendered.
//! With several bridges loaded, each stream's bridge may hint another one.
//! `render.evaluation_grid` says which applies:
//!
//! - **`bridge`** (the default): the grid is the active bridge's hint, all of
//!   it. A stream that brings other hints starts an evaluation-only rebuild
//!   off the render thread; the installed table renders until the new one is
//!   swapped in. The grid is not editable.
//! - **`custom`**: the user's grid, whatever the stream.
//!
//! A rebuild for a grid carries a **grid request**: the grid and a generation.
//! The control records the latest; a topology built for an older one is not
//! published ([`RendererControl::publish_topology_if_current`]), and the
//! speaker stage does not install the band set of one either, so a request
//! that changes its mind (a switch to `custom` while the bridge's grid is
//! being built, A → B → A) ends on the latest, never on whichever build
//! finished last.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bridge_api::{RVbapCartesianDefaults, RVbapTableMode};
use parking_lot::Mutex;

use crate::config::RenderConfig;
use crate::live_params::{
    CartesianEvaluationParams, LiveEvaluationMode, LiveParams, PreferredEvaluationMode,
    RenderTopology, RendererControl,
};

/// Where the evaluation grid comes from (`render.evaluation_grid`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EvaluationGridSource {
    /// The active bridge's hint.
    #[default]
    Bridge,
    /// The user's grid, whatever the stream.
    Custom,
}

/// The canonical spellings of [`EvaluationGridSource`], in its order.
pub const EVALUATION_GRID_SOURCES: &[&str] = &["bridge", "custom"];

impl EvaluationGridSource {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bridge => "bridge",
            Self::Custom => "custom",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "bridge" => Some(Self::Bridge),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

/// A grid the gain table is sampled on: what a bridge hints and what a user
/// forces. The mode is concrete, never `auto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvaluationGrid {
    pub mode: LiveEvaluationMode,
    /// Cells per axis (not grid points).
    pub cartesian: CartesianEvaluationParams,
    pub allow_negative_z: bool,
}

impl EvaluationGrid {
    /// A bridge's hint: its preferred table and its Cartesian grid.
    pub fn from_hint(defaults: RVbapCartesianDefaults, preferred: RVbapTableMode) -> Self {
        Self {
            mode: match preferred {
                RVbapTableMode::Polar => LiveEvaluationMode::PrecomputedPolar,
                RVbapTableMode::Cartesian => LiveEvaluationMode::PrecomputedCartesian,
            },
            cartesian: CartesianEvaluationParams {
                x_size: (defaults.x_size as usize).max(1),
                y_size: (defaults.y_size as usize).max(1),
                z_size: (defaults.z_size as usize).max(1),
                z_neg_size: defaults.z_neg_size as usize,
            },
            allow_negative_z: defaults.allow_negative_z,
        }
    }

    /// The grid `live` asks for, `auto` resolved to `preferred`.
    pub fn of_live(live: &LiveParams, preferred: PreferredEvaluationMode) -> Self {
        let mode = match live.evaluation.mode {
            LiveEvaluationMode::Auto => match preferred {
                PreferredEvaluationMode::PrecomputedPolar => LiveEvaluationMode::PrecomputedPolar,
                PreferredEvaluationMode::PrecomputedCartesian => {
                    LiveEvaluationMode::PrecomputedCartesian
                }
            },
            mode => mode,
        };
        Self {
            mode,
            cartesian: live.evaluation.cartesian,
            allow_negative_z: live.evaluation.allow_negative_z,
        }
    }

    /// Whether a table sampled on `self` is the one `other` asks for: the
    /// same mode and negative z, and the same cells when the mode samples
    /// them. Sizes a polar or realtime evaluation never reads do not count.
    pub fn same_table(&self, other: &Self) -> bool {
        self.mode == other.mode
            && self.allow_negative_z == other.allow_negative_z
            && (self.mode != LiveEvaluationMode::PrecomputedCartesian
                || self.cartesian == other.cartesian)
    }

    /// Make it the grid `live` asks for.
    pub fn apply(&self, live: &mut LiveParams) {
        live.evaluation.mode = self.mode;
        live.evaluation.cartesian = self.cartesian;
        live.evaluation.allow_negative_z = self.allow_negative_z;
    }

    /// As published in `/omniphony/state/renderer` (`evaluationGridBridge`).
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "mode": self.mode.as_str(),
            "xSize": self.cartesian.x_size,
            "ySize": self.cartesian.y_size,
            "zSize": self.cartesian.z_size,
            "zNegSize": self.cartesian.z_neg_size,
            "allowNegativeZ": self.allow_negative_z,
        })
    }

    /// What a renderer with no bridge hint at all builds on: the reference
    /// bridge's balanced Cartesian grid.
    pub fn balanced() -> Self {
        Self::from_hint(RVbapCartesianDefaults::BALANCED, RVbapTableMode::Cartesian)
    }
}

/// A stream's bridge's hint: its grid, and which bridge hints it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BridgeHint {
    pub grid: EvaluationGrid,
    /// The hinting bridge's place among the loaded bridges, in load order
    /// (`/omniphony/state/render/bridges` lists them in that order).
    pub bridge: usize,
}

/// What a config says about the grid, read against a bridge hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfigGrid {
    pub source: EvaluationGridSource,
    /// The config has no `evaluation_grid` key: the source was inferred from
    /// its grid keys ([`resolve_config`]).
    pub migrated: bool,
    /// The grid in force: the hint in `bridge` (when there is one); in
    /// `custom`, the stored grid with what it leaves out taken from the hint
    /// and `auto` resolved to the hint's mode.
    pub grid: Option<EvaluationGrid>,
}

/// The `render.render_evaluation_mode` a config stores, read: `Some(Auto)`
/// for `auto`, `None` when absent or unknown.
fn stored_mode(render: &RenderConfig) -> Option<LiveEvaluationMode> {
    render
        .render_evaluation_mode
        .as_deref()
        .and_then(LiveEvaluationMode::from_str)
}

/// Read where `render`'s grid comes from, against `hint`, the first loaded
/// bridge's (`None` when unknown).
///
/// A config without the key predates it and is migrated by a conservative
/// rule: its grid keys may be a choice or only what Save pinned (Save wrote
/// the sizes whenever the Cartesian table was in force, even on `auto`):
///
/// - a concrete mode (`realtime`, `precomputed_polar`,
///   `precomputed_cartesian`) was chosen: `custom`, whatever the values;
/// - a size or `vbap_allow_negative_z` that differs from the hint (or any,
///   when the hint is unknown): `custom`;
/// - otherwise (`auto` or no mode, values absent or equal to the hint):
///   `bridge`.
///
/// An unknown value of the key (a newer build's) reads as the default.
pub fn resolve_config(render: &RenderConfig, hint: Option<EvaluationGrid>) -> ConfigGrid {
    let mode = stored_mode(render);
    let (source, migrated) = match render.evaluation_grid.as_deref() {
        Some(value) => (
            EvaluationGridSource::parse(value).unwrap_or_default(),
            false,
        ),
        None => {
            let differs = |stored: Option<usize>, hinted: fn(&EvaluationGrid) -> usize| {
                stored.is_some_and(|value| hint.is_none_or(|h| hinted(&h) != value))
            };
            let chose_mode = mode.is_some_and(|m| m != LiveEvaluationMode::Auto);
            let sizes_differ = differs(render.evaluation_cartesian_x_size, |h| h.cartesian.x_size)
                || differs(render.evaluation_cartesian_y_size, |h| h.cartesian.y_size)
                || differs(render.evaluation_cartesian_z_size, |h| h.cartesian.z_size)
                || differs(render.evaluation_cartesian_z_neg_size, |h| {
                    h.cartesian.z_neg_size
                });
            let negative_z_differs = render
                .vbap_allow_negative_z
                .is_some_and(|value| hint.is_none_or(|h| h.allow_negative_z != value));
            let source = if chose_mode || sizes_differ || negative_z_differs {
                EvaluationGridSource::Custom
            } else {
                EvaluationGridSource::Bridge
            };
            (source, true)
        }
    };
    let grid = match source {
        EvaluationGridSource::Bridge => hint,
        EvaluationGridSource::Custom => Some(forced_grid(
            render,
            hint.unwrap_or_else(EvaluationGrid::balanced),
        )),
    };
    ConfigGrid {
        source,
        migrated,
        grid,
    }
}

/// The grid `render` forces: what it stores, the rest (and the mode of an
/// `auto`) taken from `base`.
pub fn forced_grid(render: &RenderConfig, base: EvaluationGrid) -> EvaluationGrid {
    EvaluationGrid {
        mode: match stored_mode(render) {
            Some(LiveEvaluationMode::Auto) | None => base.mode,
            Some(mode) => mode,
        },
        cartesian: CartesianEvaluationParams {
            x_size: render
                .evaluation_cartesian_x_size
                .unwrap_or(base.cartesian.x_size)
                .max(1),
            y_size: render
                .evaluation_cartesian_y_size
                .unwrap_or(base.cartesian.y_size)
                .max(1),
            z_size: render
                .evaluation_cartesian_z_size
                .unwrap_or(base.cartesian.z_size)
                .max(1),
            z_neg_size: render
                .evaluation_cartesian_z_neg_size
                .unwrap_or(base.cartesian.z_neg_size),
        },
        allow_negative_z: render
            .vbap_allow_negative_z
            .unwrap_or(base.allow_negative_z),
    }
}

/// Settle `render`'s grid against the first loaded bridge's hint, before a
/// renderer is built on it: the key is set (migrated when absent, see
/// [`resolve_config`]); in `custom` every grid key holds the forced grid,
/// `auto` resolved to a concrete mode; in `bridge` the grid keys are dropped,
/// the build takes the hint. Only this in-memory config changes: nothing is
/// written, a migration is for the host to report and mark unsaved. Returns
/// what was read.
pub fn settle_config(render: &mut RenderConfig, hint: EvaluationGrid) -> ConfigGrid {
    if let Some(value) = render.evaluation_grid.as_deref()
        && EvaluationGridSource::parse(value).is_none()
    {
        log::warn!(
            "config: render.evaluation_grid = {value:?} is not a value this build knows; \
             following the bridge"
        );
    }
    let resolved = resolve_config(render, Some(hint));
    render.evaluation_grid = Some(resolved.source.as_str().to_string());
    match (resolved.source, resolved.grid) {
        (EvaluationGridSource::Custom, Some(grid)) => {
            render.render_evaluation_mode = Some(grid.mode.as_str().to_string());
            render.evaluation_cartesian_x_size = Some(grid.cartesian.x_size);
            render.evaluation_cartesian_y_size = Some(grid.cartesian.y_size);
            render.evaluation_cartesian_z_size = Some(grid.cartesian.z_size);
            render.evaluation_cartesian_z_neg_size = Some(grid.cartesian.z_neg_size);
            render.vbap_allow_negative_z = Some(grid.allow_negative_z);
        }
        _ => {
            render.render_evaluation_mode = None;
            render.evaluation_cartesian_x_size = None;
            render.evaluation_cartesian_y_size = None;
            render.evaluation_cartesian_z_size = None;
            render.evaluation_cartesian_z_neg_size = None;
            render.vbap_allow_negative_z = None;
        }
    }
    if resolved.migrated {
        log::info!(
            "config: no render.evaluation_grid yet; the grid {} (the first bridge hints {}); \
             Save to keep it",
            match resolved.source {
                EvaluationGridSource::Bridge => "follows the bridge".to_string(),
                EvaluationGridSource::Custom => format!(
                    "stays forced at {}",
                    describe(&resolved.grid.unwrap_or(hint))
                ),
            },
            describe(&hint)
        );
    }
    resolved
}

/// A grid in a log line.
fn describe(grid: &EvaluationGrid) -> String {
    let c = &grid.cartesian;
    format!(
        "{} {}x{}x{}+{}{}",
        grid.mode.as_str(),
        c.x_size,
        c.y_size,
        c.z_size,
        c.z_neg_size,
        if grid.allow_negative_z {
            ", negative z"
        } else {
            ""
        }
    )
}

/// What became of a grid request ([`RendererControl::request_live_grid`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GridDecision {
    /// The latest request already asks for this grid.
    Unchanged,
    /// A topology built on this grid is in force (the active one, or the one
    /// a grid-only rebuild just replaced, published again): no rebuild.
    Adopted,
    /// A topology has to be built on it.
    Rebuild,
}

/// A topology a grid-only rebuild published, and the one it replaced: the
/// two differ by their grid alone, so a request for the replaced one's grid
/// publishes it again instead of building it a third time.
struct GridPair {
    published: Arc<RenderTopology>,
    replaced: Arc<RenderTopology>,
}

#[derive(Default)]
struct GridInner {
    /// The last hint a stream's bridge offered.
    hint: Option<BridgeHint>,
    /// The grid of the latest request.
    requested: Option<EvaluationGrid>,
    pair: Option<GridPair>,
    /// The rebuild running now was started by a grid request alone, from
    /// the topology then in force.
    grid_only_in_flight: bool,
}

/// The grid requests of a [`RendererControl`]: the latest one, the hints the
/// render thread offers and the grid the speaker stage installed. See the
/// [module docs](self).
#[derive(Default)]
pub struct GridRequests {
    inner: Mutex<GridInner>,
    /// Generation of the latest request, read lock-free by the speaker stage.
    latest: AtomicU64,
    /// Bumped by every hint offered that differs from the one before.
    hints_offered: AtomicU64,
    /// [`Self::hints_offered`] as of the last hint taken.
    hints_taken: AtomicU64,
    /// The grid of the band set the speaker stage installed last.
    installed: Mutex<Option<EvaluationGrid>>,
    /// No bridge hint settled the grid ([`RendererControl::keep_grid_as_loaded`]).
    unsettled: std::sync::atomic::AtomicBool,
}

impl RendererControl {
    /// Generation of the latest grid request.
    pub fn grid_generation(&self) -> u64 {
        self.grid.latest.load(Ordering::Acquire)
    }

    /// Whether `topology` answers the latest grid request: a speaker stage
    /// installs the bands of no other.
    pub fn topology_grid_is_current(&self, topology: &RenderTopology) -> bool {
        topology.grid_generation() == self.grid_generation()
    }

    /// The preferred table the renderer was built with (`auto` resolves to
    /// it).
    fn preferred_mode(&self) -> PreferredEvaluationMode {
        self.backend_rebuild_params()
            .map(|params| params.preferred_evaluation_mode())
            .unwrap_or(PreferredEvaluationMode::PrecomputedCartesian)
    }

    /// The grid the live params ask for now.
    pub fn live_grid(&self) -> EvaluationGrid {
        let preferred = self.preferred_mode();
        EvaluationGrid::of_live(&self.live.read(), preferred)
    }

    /// The hint the renderer was built on, taken at once (no rebuild: the
    /// renderer is built on it). Called by the build, before the seed.
    pub fn seed_bridge_grid(&self, hint: EvaluationGrid) {
        self.grid.inner.lock().hint = Some(BridgeHint {
            grid: hint,
            bridge: 0,
        });
        let mut live = self.live.write();
        live.evaluation.bridge_hint = Some(hint);
        live.evaluation.bridge_index = Some(0);
    }

    /// The first bridge hints `grid` ([`Self::offer_bridge_hint`]).
    pub fn offer_bridge_grid(&self, grid: EvaluationGrid) -> bool {
        self.offer_bridge_hint(BridgeHint { grid, bridge: 0 })
    }

    /// A stream's bridge hints `hint`. From the thread that applies the
    /// bridge's declaration: never blocks and allocates nothing. Returns
    /// `false` when the hints were busy, for the caller to offer it again
    /// with a later frame.
    pub fn offer_bridge_hint(&self, hint: BridgeHint) -> bool {
        let Some(mut inner) = self.grid.inner.try_lock() else {
            return false;
        };
        if inner.hint != Some(hint) {
            inner.hint = Some(hint);
            self.grid.hints_offered.fetch_add(1, Ordering::AcqRel);
        }
        true
    }

    /// A hint was offered that no host has taken yet
    /// ([`Self::take_bridge_grid`]).
    pub fn bridge_grid_pending(&self) -> bool {
        self.grid.hints_offered.load(Ordering::Acquire)
            != self.grid.hints_taken.load(Ordering::Acquire)
    }

    /// The count of hints offered: part of what the render thread's layout
    /// follower watches.
    pub fn bridge_grids_offered(&self) -> u64 {
        self.grid.hints_offered.load(Ordering::Acquire)
    }

    /// Take the last hint offered: it becomes the bridge's grid the state
    /// publishes and, while the grid follows the bridge, the grid the live
    /// params ask for. Returns whether it does: the caller then requests it
    /// ([`Self::request_live_grid`]). Off the audio thread.
    pub fn take_bridge_grid(&self) -> bool {
        let offered = self.grid.hints_offered.load(Ordering::Acquire);
        let hint = self.grid.inner.lock().hint;
        self.grid.hints_taken.store(offered, Ordering::Release);
        let Some(hint) = hint else {
            return false;
        };
        let follows = {
            let mut live = self.live.write();
            live.evaluation.bridge_hint = Some(hint.grid);
            live.evaluation.bridge_index = Some(hint.bridge);
            let follows = live.evaluation.source == EvaluationGridSource::Bridge;
            if follows {
                hint.grid.apply(&mut live);
            }
            follows
        };
        // `evaluationGridBridge`, and the grid itself when it follows.
        self.bump_live_state();
        follows
    }

    /// Record the live grid as the latest request, for a rebuild that
    /// covers more than the grid (a layout edit, a profile switch): a
    /// grid-only build still running is then out of date.
    pub fn record_live_grid(&self) {
        let grid = self.live_grid();
        let mut inner = self.grid.inner.lock();
        if inner.requested != Some(grid) {
            inner.requested = Some(grid);
            self.grid.latest.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// Request the grid the live params ask for, as a grid-only change (the
    /// source switched, the bridge's hint moved): a new generation, then no
    /// rebuild when a topology built on that grid can be put back in force.
    /// A change of negative z needs new gain models (the panner keeps or
    /// clamps z), so the geometry generation is bumped for that rebuild.
    pub fn request_live_grid(&self) -> GridDecision {
        let grid = self.live_grid();
        let mut inner = self.grid.inner.lock();
        if inner.requested == Some(grid) {
            return GridDecision::Unchanged;
        }
        inner.requested = Some(grid);
        let generation = self.grid.latest.fetch_add(1, Ordering::AcqRel) + 1;
        let active = self.topology.load_full();
        if active.grid.is_some_and(|built| built.same_table(&grid)) {
            active.stamp_grid_generation(generation);
            return GridDecision::Adopted;
        }
        if let Some(pair) = inner.pair.take() {
            if Arc::ptr_eq(&pair.published, &active)
                && pair
                    .replaced
                    .grid
                    .is_some_and(|built| built.same_table(&grid))
            {
                pair.replaced.stamp_grid_generation(generation);
                self.topology.store(Arc::clone(&pair.replaced));
                // The same two, the other way round.
                inner.pair = Some(GridPair {
                    published: pair.replaced,
                    replaced: pair.published,
                });
                return GridDecision::Adopted;
            }
            inner.pair = Some(pair);
        }
        if active.grid.map(|built| built.allow_negative_z) != Some(grid.allow_negative_z) {
            self.bump_geometry_generation();
        }
        GridDecision::Rebuild
    }

    /// Whether the rebuild in flight was started by a grid request alone.
    pub fn grid_only_rebuild_in_flight(&self) -> bool {
        self.grid.inner.lock().grid_only_in_flight
    }

    /// Note what the rebuild about to start is for (see
    /// [`Self::grid_only_rebuild_in_flight`]).
    pub fn set_grid_only_rebuild_in_flight(&self, grid_only: bool) {
        self.grid.inner.lock().grid_only_in_flight = grid_only;
    }

    /// Publish `topology` unless a later grid request was made since its plan
    /// was prepared, whatever its identity: the topology in force stays.
    /// `grid_only_from`: for a rebuild started by a grid request alone, the
    /// topology in force when it started, which a request for its grid may
    /// put back ([`Self::request_live_grid`]). Returns whether it was
    /// published.
    pub fn publish_topology_if_current(
        &self,
        topology: RenderTopology,
        grid_only_from: Option<Arc<RenderTopology>>,
    ) -> bool {
        let mut inner = self.grid.inner.lock();
        if topology.grid_generation() != self.grid_generation() {
            return false;
        }
        let published = Arc::new(topology);
        let replaced = self.topology.swap(Arc::clone(&published));
        inner.pair = grid_only_from
            .filter(|from| Arc::ptr_eq(from, &replaced))
            .map(|replaced| GridPair {
                published,
                replaced,
            });
        drop(inner);
        drop(replaced);
        true
    }

    /// A rebuild for the latest request failed: the topology in force keeps
    /// rendering and is taken as its answer, so the speaker stage keeps
    /// following it (a crossover change still rebuilds its bands).
    pub fn grid_rebuild_failed(&self) {
        let _inner = self.grid.inner.lock();
        self.topology
            .load()
            .stamp_grid_generation(self.grid_generation());
    }

    /// Forget the grid-only pair: a topology published by any other way is
    /// not one of the two.
    pub(crate) fn forget_grid_pair(&self) {
        self.grid.inner.lock().pair = None;
    }

    /// A host that loaded no bridge (the standby runtime) never settles the
    /// grid against a hint: a config from before `evaluation_grid` cannot be
    /// migrated there, so a save keeps the grid keys as the file has them,
    /// for the next start with a bridge to migrate.
    pub fn keep_grid_as_loaded(&self) {
        self.grid.unsettled.store(true, Ordering::Release);
    }

    /// Whether a save writes the grid (see [`Self::keep_grid_as_loaded`]).
    pub fn grid_settled(&self) -> bool {
        !self.grid.unsettled.load(Ordering::Acquire)
    }

    /// The speaker stage installed the band set of a topology built on
    /// `grid`. From the render thread, on an install (never per frame).
    pub fn note_installed_grid(&self, grid: Option<EvaluationGrid>) {
        if grid.is_some() {
            *self.grid.installed.lock() = grid;
        }
    }

    /// The grid of the table in force: the speaker stage's installed one,
    /// else the published topology's. What a switch to `custom` starts from.
    pub fn installed_grid(&self) -> Option<EvaluationGrid> {
        let installed = *self.grid.installed.lock();
        installed.or_else(|| self.topology.load().grid)
    }
}

impl RenderTopology {
    /// The grid request this topology answers.
    pub fn grid_generation(&self) -> u64 {
        self.grid_generation.load(Ordering::Acquire)
    }

    /// Take this topology as the answer to request `generation`.
    pub(crate) fn stamp_grid_generation(&self, generation: u64) {
        self.grid_generation.store(generation, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hint(x: u32, negative_z: bool, preferred: RVbapTableMode) -> EvaluationGrid {
        EvaluationGrid::from_hint(
            RVbapCartesianDefaults {
                x_size: x,
                y_size: 9,
                z_size: 5,
                z_neg_size: 0,
                allow_negative_z: negative_z,
            },
            preferred,
        )
    }

    fn cartesian(x: u32) -> EvaluationGrid {
        hint(x, false, RVbapTableMode::Cartesian)
    }

    #[test]
    fn a_config_with_the_key_is_not_migrated() {
        let render = RenderConfig {
            evaluation_grid: Some("custom".into()),
            ..Default::default()
        };
        let read = resolve_config(&render, Some(cartesian(9)));
        assert_eq!(read.source, EvaluationGridSource::Custom);
        assert!(!read.migrated);
        // What the config leaves out is the hint's, the mode included.
        assert_eq!(read.grid, Some(cartesian(9)));
    }

    #[test]
    fn auto_with_the_hints_sizes_follows_the_bridge() {
        let hint = cartesian(9);
        for render in [
            RenderConfig::default(),
            RenderConfig {
                render_evaluation_mode: Some("auto".into()),
                evaluation_cartesian_x_size: Some(9),
                evaluation_cartesian_y_size: Some(9),
                evaluation_cartesian_z_size: Some(5),
                evaluation_cartesian_z_neg_size: Some(0),
                ..Default::default()
            },
        ] {
            let read = resolve_config(&render, Some(hint));
            assert_eq!(read.source, EvaluationGridSource::Bridge);
            assert!(read.migrated);
            assert_eq!(read.grid, Some(hint));
        }
    }

    #[test]
    fn a_chosen_mode_or_another_size_stays_custom() {
        let hint = cartesian(9);
        let cases = [
            RenderConfig {
                render_evaluation_mode: Some("precomputed_polar".into()),
                ..Default::default()
            },
            RenderConfig {
                render_evaluation_mode: Some("realtime".into()),
                ..Default::default()
            },
            // Equal to the hint, but a mode was chosen.
            RenderConfig {
                render_evaluation_mode: Some("precomputed_cartesian".into()),
                evaluation_cartesian_x_size: Some(9),
                ..Default::default()
            },
            RenderConfig {
                evaluation_cartesian_x_size: Some(4),
                ..Default::default()
            },
        ];
        for render in cases {
            let read = resolve_config(&render, Some(hint));
            assert_eq!(read.source, EvaluationGridSource::Custom, "{render:?}");
            assert!(read.migrated);
            let grid = read.grid.expect("a custom grid");
            assert_ne!(grid.mode, LiveEvaluationMode::Auto);
        }
    }

    #[test]
    fn negative_z_counts_only_when_it_differs_from_the_hint() {
        for (hinted, stored, source) in [
            (false, true, EvaluationGridSource::Custom),
            (true, false, EvaluationGridSource::Custom),
            (true, true, EvaluationGridSource::Bridge),
            (false, false, EvaluationGridSource::Bridge),
        ] {
            let render = RenderConfig {
                render_evaluation_mode: Some("auto".into()),
                vbap_allow_negative_z: Some(stored),
                ..Default::default()
            };
            let read = resolve_config(&render, Some(hint(9, hinted, RVbapTableMode::Cartesian)));
            assert_eq!(read.source, source, "hint {hinted}, stored {stored}");
            if source == EvaluationGridSource::Custom {
                assert_eq!(read.grid.unwrap().allow_negative_z, stored);
            }
        }
    }

    #[test]
    fn settling_writes_a_complete_custom_grid_or_drops_the_bridge_one() {
        let hint = hint(9, false, RVbapTableMode::Polar);
        let mut render = RenderConfig {
            evaluation_cartesian_z_size: Some(3),
            ..Default::default()
        };
        let read = settle_config(&mut render, hint);
        assert!(read.migrated);
        assert_eq!(render.evaluation_grid.as_deref(), Some("custom"));
        // `auto` resolved to the hint's mode, the rest filled in.
        assert_eq!(
            render.render_evaluation_mode.as_deref(),
            Some("precomputed_polar")
        );
        assert_eq!(render.evaluation_cartesian_x_size, Some(9));
        assert_eq!(render.evaluation_cartesian_z_size, Some(3));
        assert_eq!(render.vbap_allow_negative_z, Some(false));
        // Settled once: read again, nothing moves.
        let again = settle_config(&mut render, hint);
        assert!(!again.migrated);
        assert_eq!(again.grid, read.grid);

        let mut render = RenderConfig {
            evaluation_cartesian_x_size: Some(9),
            ..Default::default()
        };
        assert_eq!(
            settle_config(&mut render, hint).source,
            EvaluationGridSource::Bridge
        );
        assert_eq!(render.evaluation_cartesian_x_size, None);
    }

    #[test]
    fn sizes_a_polar_table_never_reads_are_not_another_table() {
        let a = hint(9, false, RVbapTableMode::Polar);
        let b = hint(20, false, RVbapTableMode::Polar);
        assert!(a.same_table(&b));
        assert!(!cartesian(9).same_table(&cartesian(20)));
        assert!(!a.same_table(&hint(9, true, RVbapTableMode::Polar)));
    }
}
