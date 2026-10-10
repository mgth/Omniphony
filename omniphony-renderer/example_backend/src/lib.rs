//! # Example render backend
//!
//! A minimal, dependency-light [`GainModel`] you can copy as the starting point
//! for your own spatial panner. It depends on `renderer` through its **public
//! API only** — if this crate compiles, the public surface is enough to write a
//! backend from outside the renderer, which is exactly what we want for
//! contributors. CI builds it as a workspace member to keep that guarantee.
//!
//! ## What it does
//!
//! A simple cosine (dot-product) panner: each speaker's gain is how well its
//! direction lines up with the object's direction, raised to a sharpness
//! exponent, then the whole vector is normalised to constant energy. It is
//! intentionally not trying to sound good — it is the smallest thing that
//! exercises the real contract.
//!
//! ## The hot-path contract (read this before writing `compute_gains`)
//!
//! `compute_gains` runs in the realtime audio thread, once per object per band
//! per frame. It MUST NOT panic, allocate on the heap, lock, or block, and it
//! MUST write every one of the [`speaker_count`](GainModel::speaker_count)
//! gains it is handed, all finite: the buffer is the caller's and arrives
//! holding its previous contents, not zeros. Do expensive setup (here:
//! normalising the speaker directions) when the model is built, never in
//! `compute_gains`. A backend that needs working memory per call (a second
//! gain set, a solver's arrays) sizes it once in
//! [`new_scratch`](GainModel::new_scratch) and gets it back on every call;
//! this one needs none. See the `GainModel` trait docs.
//!
//! ## Selecting it at runtime
//!
//! Implement [`PluginFactory`] (its id, label and parameter schema — the
//! contract every plugin shares, object generators included) and
//! [`BackendFactory`] (how to build it; see [`ExampleFactory`]), and a host
//! registers it with `RendererControl::register_backend`; selecting
//! `backend_id = "example"` then routes a topology rebuild through it — no
//! central enum or `match` to edit. The backend's identity lives entirely on
//! this crate: its `backend_id`, `backend_label` and parameter schema come from
//! the [`GainModel`] impl and [`PluginFactory`], with no closed enum in
//! `renderer` to extend.

use renderer::backend_params::{ParamSpec, ParamValue};
use renderer::backend_registry::{
    BackendBuildCtx, BackendBuildPlan, BackendFactory, DynamicBackendPlan,
};
use renderer::plugin::PluginFactory;
use renderer::render_backend::{
    BackendCapabilities, GainModel, GainScratch, RenderRequest, room_scaled_position,
};
use renderer::spatial_vbap::spherical_to_adm;
use renderer::speaker_layout::SpeakerLayout;

/// Default sharpness of the cosine lobe when the host has not set the param.
const DEFAULT_SHARPNESS: f32 = 2.0;

/// A cosine-panning gain model over a fixed set of speaker directions.
pub struct ExampleBackend {
    /// Unit direction vectors, one per speaker, precomputed at construction so
    /// the hot path only does dot products. A zero vector marks a speaker that
    /// had no usable direction (e.g. one placed at the origin); it never wins
    /// any gain.
    speaker_dirs: Vec<[f32; 3]>,
    /// Cosine-lobe exponent: higher = tighter localisation, lower = more spread.
    sharpness: f32,
}

impl ExampleBackend {
    /// Build the backend from speaker positions in scene space. Positions are
    /// normalised to unit directions here, on the build thread — the expensive
    /// work stays out of `compute_gains`.
    pub fn new(speaker_positions: &[[f32; 3]], sharpness: f32) -> Self {
        let speaker_dirs = speaker_positions.iter().map(|p| normalize(*p)).collect();
        Self {
            speaker_dirs,
            sharpness,
        }
    }
}

impl GainModel for ExampleBackend {
    fn backend_id(&self) -> &'static str {
        "example"
    }

    fn backend_label(&self) -> &'static str {
        "Cosine panner"
    }

    fn capabilities(&self) -> BackendCapabilities {
        // Declare only what is actually implemented. This panner is realtime and
        // ignores spread / distance / object-size / table export, so everything
        // else is false. The host trusts these flags to decide which controls and
        // evaluation modes to expose.
        BackendCapabilities {
            supports_realtime: true,
            supports_precomputed_polar: false,
            supports_precomputed_cartesian: false,
            supports_position_interpolation: false,
            supports_distance_model: false,
            supports_spread: false,
            supports_spread_from_distance: false,
            supports_event_size: false,
            supports_distance_diffuse: false,
            supports_table_export: false,
        }
    }

    fn speaker_count(&self) -> usize {
        self.speaker_dirs.len()
    }

    fn compute_gains(&self, req: &RenderRequest, _scratch: &mut GainScratch, gains: &mut [f32]) {
        // `gains` is the caller's buffer, one gain per speaker: writing it
        // allocates nothing. This backend needs no working memory beyond it,
        // so it leaves `new_scratch` at its default and ignores the scratch.

        // The speakers were placed in the room the topology pans in (the
        // factory read them so); the object goes through the same warp,
        // which the request carries, or a non-unit room would pull it off
        // the speaker it sits on. Pure arithmetic: still allocation-free.
        let dir = normalize(room_scaled_position(
            [
                req.adm_position[0] as f32,
                req.adm_position[1] as f32,
                req.adm_position[2] as f32,
            ],
            req.room_ratio,
            req.room_ratio_rear,
            req.room_ratio_lower,
            req.room_ratio_center_blend,
        ));

        // Pass 1: raw cosine weights into the gain buffer, accumulating energy.
        // Every gain is written: what the buffer held before is not ours.
        let n = gains.len();
        let mut sum_sq = 0.0f32;
        for (gain, sd) in gains.iter_mut().zip(&self.speaker_dirs) {
            let dot = dir[0] * sd[0] + dir[1] * sd[1] + dir[2] * sd[2];
            let w = dot.max(0.0).powf(self.sharpness);
            *gain = w;
            sum_sq += w * w;
        }

        // Pass 2: normalise to constant energy. If nothing won any gain (object
        // at the origin, or all speakers behind it), fall back to equal power so
        // we never emit silence or non-finite values.
        if sum_sq > f32::EPSILON {
            let inv = 1.0 / sum_sq.sqrt();
            for g in gains.iter_mut() {
                *g *= inv;
            }
        } else if n > 0 {
            let eq = (1.0 / n as f32).sqrt();
            for g in gains.iter_mut() {
                *g = eq;
            }
        }
    }

    fn save_to_file(&self, _path: &std::path::Path, _layout: &SpeakerLayout) -> anyhow::Result<()> {
        // `supports_table_export` is false, so the host never calls this for a
        // real export; be explicit rather than silently succeeding.
        anyhow::bail!("example backend does not support table export")
    }
}

/// Registers [`ExampleBackend`] under the id `"example"`.
///
/// This is the other half of the skeleton: implement [`BackendFactory`] and a
/// host can `register` it (`RendererControl::register_backend`) at startup, after
/// which selecting `backend_id = "example"` routes a topology rebuild through it.
/// Note there is no central enum or `match` to edit — the factory returns a
/// [`BackendBuildPlan::Dynamic`] whose closure builds the model from the layout.
pub struct ExampleFactory;

impl PluginFactory for ExampleFactory {
    fn id(&self) -> &'static str {
        "example"
    }

    fn label(&self) -> &'static str {
        "Cosine panner"
    }

    fn param_schema(&self) -> Vec<ParamSpec> {
        // One tunable, declared as data: the UI renders a slider and the host
        // stores the value generically — no typed field anywhere in the renderer.
        vec![
            ParamSpec::float("sharpness", "Sharpness", 0.5, 8.0, 0.1, DEFAULT_SHARPNESS)
                .help("Cosine-lobe exponent: higher = tighter localisation, lower = more spread."),
        ]
    }
}

impl BackendFactory for ExampleFactory {
    fn build_plan(&self, ctx: &BackendBuildCtx<'_>) -> Option<BackendBuildPlan> {
        // Capture the spatializable speaker directions now (build thread), so the
        // model builder closure owns everything it needs and the hot path does no
        // layout lookups. Read in the room the topology pans in, as the
        // objects are warped with it (a cartesian speaker is a fraction of
        // that room). Azimuth/elevation pairs are converted to unit vectors.
        let room = ctx.room;
        let (azimuth_elevation, _spatializable_indices) = ctx
            .layout
            .spatializable_positions_for_room(room.ratio, room.rear, room.lower, room.center_blend);
        let speaker_positions: Vec<[f32; 3]> = azimuth_elevation
            .iter()
            .map(|[az, el]| {
                let (x, y, z) = spherical_to_adm(*az, *el, 1.0);
                [x, y, z]
            })
            .collect();

        // Read the host-set sharpness (falling back to the default), resolved at
        // build time and captured into the model.
        let sharpness = ctx
            .backend_param(self.id(), "sharpness")
            .and_then(ParamValue::as_f32)
            .unwrap_or(DEFAULT_SHARPNESS);

        Some(BackendBuildPlan::Dynamic(DynamicBackendPlan::new(
            "example",
            move || Ok(Box::new(ExampleBackend::new(&speaker_positions, sharpness))),
        )))
    }
}

/// Normalise a vector to unit length, returning a zero vector for inputs at (or
/// extremely close to) the origin so callers can treat "no direction" uniformly.
fn normalize(v: [f32; 3]) -> [f32; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if len > f32::EPSILON {
        [v[0] / len, v[1] / len, v[2] / len]
    } else {
        [0.0, 0.0, 0.0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer::backend_conformance::{
        ConformanceOptions, CountingAllocator, ZeroAllocReport, check, check_zero_alloc,
    };

    // Counting global allocator for the *test build only*, so the zero-allocation
    // conformance check can actually observe heap traffic. Consumers that depend
    // on `example_backend` normally are unaffected (this is gated on `test`).
    #[global_allocator]
    static GLOBAL: CountingAllocator = CountingAllocator;

    /// A neutral request at `pos`, from the shared conformance harness helper.
    fn request(pos: [f64; 3]) -> RenderRequest {
        let mut req = renderer::backend_conformance::neutral_request();
        req.adm_position = pos;
        req
    }

    /// A square of four speakers in the horizontal plane.
    fn quad() -> ExampleBackend {
        ExampleBackend::new(
            &[
                [1.0, 1.0, 0.0],
                [-1.0, 1.0, 0.0],
                [-1.0, -1.0, 0.0],
                [1.0, -1.0, 0.0],
            ],
            DEFAULT_SHARPNESS,
        )
    }

    fn energy(gains: &[f32]) -> f32 {
        gains.iter().map(|g| g * g).sum()
    }

    /// Built through the factory on cartesian speakers in a non-unit room,
    /// the object is warped as the speakers were: an object on a speaker's
    /// place favours that speaker. Read raw against warped speakers, A's
    /// object favoured B (#803 review).
    #[test]
    fn the_object_is_warped_like_the_speakers() {
        use renderer::backend_registry::{BackendBuildCtx, BackendRegistry};
        use renderer::live_params::RoomRatios;
        use renderer::speaker_layout::Speaker;

        let layout = SpeakerLayout::from_speakers(vec![
            Speaker::from_cartesian("A", 1.0, 1.0, 0.0, true, 0.0),
            Speaker::from_cartesian("B", 1.0, 0.5, 0.0, true, 0.0),
            Speaker::from_cartesian("C", -1.0, -1.0, 0.0, true, 0.0),
        ])
        .expect("three speakers");
        let room = RoomRatios {
            ratio: [1.0, 2.0, 1.0],
            rear: 2.0,
            lower: 1.0,
            center_blend: 0.5,
        };
        let control = renderer::test_support::fixture_control();
        let registry = BackendRegistry::builtin();
        let params = std::collections::HashMap::new();
        let live = control.live.read();
        let ctx = BackendBuildCtx {
            layout: &layout,
            live: &live,
            room,
            backend_rebuild_params: None,
            registry: &registry,
            backend_params: &params,
        };
        let model = ExampleFactory
            .build_plan(&ctx)
            .expect("a plan")
            .build_gain_model()
            .expect("the model");
        let gains_at = |p: [f64; 3]| {
            let mut req = request(p);
            req.room_ratio = room.ratio;
            req.room_ratio_rear = room.rear;
            req.room_ratio_lower = room.lower;
            req.room_ratio_center_blend = room.center_blend;
            model.gains_at(&req).to_vec()
        };
        let on_a = gains_at([1.0, 1.0, 0.0]);
        assert!(
            on_a[0] > on_a[1] && on_a[0] > on_a[2],
            "on A, A is favoured: {on_a:?}"
        );
        let on_c = gains_at([-1.0, -1.0, 0.0]);
        assert!(
            on_c[2] > on_c[0] && on_c[2] > on_c[1],
            "on C, C is favoured: {on_c:?}"
        );
    }

    #[test]
    fn returns_one_finite_gain_per_speaker() {
        let backend = quad();
        let gains = backend.gains_at(&request([0.7, 0.7, 0.0]));
        assert_eq!(gains.len(), 4);
        assert!(gains.iter().all(|g| g.is_finite()));
    }

    #[test]
    fn normalises_to_unit_energy() {
        let backend = quad();
        for pos in [[0.7, 0.7, 0.0], [1.0, 0.0, 0.0], [-0.3, 0.9, 0.0]] {
            let gains = backend.gains_at(&request(pos));
            assert!(
                (energy(&gains) - 1.0).abs() < 1e-4,
                "energy at {pos:?} was {}",
                energy(&gains)
            );
        }
    }

    #[test]
    fn centre_falls_back_to_equal_power() {
        let backend = quad();
        let gains = backend.gains_at(&request([0.0, 0.0, 0.0]));
        assert!((energy(&gains) - 1.0).abs() < 1e-4);
        // Equal power: every speaker gets the same gain.
        let first = gains[0];
        assert!(gains.iter().all(|g| (g - first).abs() < 1e-6));
    }

    #[test]
    fn declares_a_sharpness_param() {
        let schema = ExampleFactory.param_schema();
        let sharpness = schema
            .iter()
            .find(|p| p.key == "sharpness")
            .expect("sharpness param declared");
        assert!(matches!(
            sharpness.kind,
            renderer::backend_params::ParamKind::Float { .. }
        ));
    }

    #[test]
    fn favours_the_aligned_speaker() {
        let backend = quad();
        // Object towards speaker 0 ([1,1,0]); it should get the largest gain.
        let gains = backend.gains_at(&request([1.0, 1.0, 0.0]));
        let max_idx = gains
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, _)| i)
            .unwrap();
        assert_eq!(max_idx, 0);
    }

    /// The reference backend must pass the public conformance harness — this is
    /// the template a contributor copies. Energy bounds are tight here because
    /// this panner is explicitly unit-energy normalised.
    #[test]
    fn passes_backend_conformance() {
        let opts = ConformanceOptions {
            energy_bounds: Some((0.99, 1.01)),
            ..Default::default()
        };
        check(&quad(), &opts).assert_passed();
    }

    /// `compute_gains` must not touch the heap. With the counting allocator
    /// installed above, the check actually runs (not skipped) and must see zero
    /// allocations.
    #[test]
    fn compute_gains_is_allocation_free() {
        let report = check_zero_alloc(&quad(), &ConformanceOptions::default());
        assert!(
            matches!(report, ZeroAllocReport::Ran { .. }),
            "counting allocator should be active in the test build"
        );
        report.assert_zero();
    }
}
