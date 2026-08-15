//! Backend conformance harness.
//!
//! A reusable, public set of checks that any [`GainModel`] — built-in or
//! contributor-provided — must satisfy to be safe on the realtime audio thread.
//! It generalises the build-time smoke test (`backend_registry::smoke_test_engine`,
//! which guards a fully built engine) into something a contributor can call from
//! their own crate's `#[test]`s while developing a backend, before ever wiring it
//! into the host.
//!
//! The checks fall into four families, matching the [`GainModel`] hot-path
//! contract:
//!
//! * **Contract** — over a grid of positions, `compute_gains` must not panic,
//!   must return exactly [`speaker_count`](GainModel::speaker_count) gains, every
//!   gain finite, non-negative, and not absurdly large (a runaway gain clips and
//!   can damage equipment).
//! * **Energy** — the panner must not be silent everywhere, and (optionally) keep
//!   its total energy within a caller-declared band. Energy normalisation differs
//!   between panners (power-preserving vs amplitude-preserving), so the tight band
//!   is opt-in; only the "not silent everywhere" floor is universal.
//! * **Continuity** — a small step in object position must produce a bounded
//!   change in the gain vector. This catches glitchy panners that would click on a
//!   moving object. Realtime gain models are continuous; precomputed
//!   nearest-cell *evaluators* are not, which is why this harness targets the raw
//!   model layer.
//! * **Zero-allocation** — `compute_gains` must not touch the heap. This is
//!   opt-in because it needs a counting global allocator installed in the test
//!   binary; see [`CountingAllocator`].
//!
//! ```ignore
//! use renderer::backend_conformance::{check, ConformanceOptions};
//!
//! #[test]
//! fn my_backend_conforms() {
//!     let model = MyBackend::new(/* … */);
//!     check(&model, &ConformanceOptions::default()).assert_passed();
//! }
//! ```

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use crate::render_backend::{GainModel, RenderRequest};
use crate::spatial_vbap::DistanceModel;

/// A neutral [`RenderRequest`] template: point source, no spread / distance /
/// diffuse processing, identity room. The position is overwritten per probe.
/// Use this as a base and tweak only the fields a specific check needs.
pub fn neutral_request() -> RenderRequest {
    RenderRequest {
        adm_position: [0.0, 0.0, 0.0],
        event_size: [0.0, 0.0, 0.0],
        room_ratio: [1.0, 1.0, 1.0],
        room_ratio_rear: 1.0,
        room_ratio_lower: 1.0,
        room_ratio_center_blend: 0.0,
        use_distance_diffuse: false,
        diffuse_mirror_axes: crate::spatial_vbap::MirrorAxes::default(),
        distance_diffuse_threshold: 1.0,
        distance_diffuse_curve: 1.0,
        distance_model: DistanceModel::None,
    }
}

/// A grid of probe positions: an `n × n × n` lattice over `[-1, 1]³` plus the
/// scene centre. Used as the default position set.
pub fn lattice_positions(n: usize) -> Vec<[f64; 3]> {
    let n = n.max(2);
    let mut out = Vec::with_capacity(n * n * n + 1);
    out.push([0.0, 0.0, 0.0]);
    for i in 0..n {
        let x = -1.0 + 2.0 * i as f64 / (n - 1) as f64;
        for j in 0..n {
            let y = -1.0 + 2.0 * j as f64 / (n - 1) as f64;
            for k in 0..n {
                let z = -1.0 + 2.0 * k as f64 / (n - 1) as f64;
                out.push([x, y, z]);
            }
        }
    }
    out
}

/// Continuity check parameters: perturb each probe position by `step` along each
/// axis and require the L2 change in the gain vector to stay at or below
/// `max_l2_per_step`.
#[derive(Debug, Clone, Copy)]
pub struct ContinuityCheck {
    pub step: f64,
    pub max_l2_per_step: f32,
    /// Probes whose distance from the scene origin is below this radius are
    /// skipped: the object→listener direction is undefined at the origin, so any
    /// direction-based panner is legitimately discontinuous there (a real object
    /// is never exactly at the listener's head). Default `0.1`.
    pub skip_radius: f64,
}

/// What [`check`] verifies, and with what tolerances.
pub struct ConformanceOptions {
    /// Positions to probe.
    pub positions: Vec<[f64; 3]>,
    /// Base request used for every probe (its position is overwritten).
    pub request_template: RenderRequest,
    /// Require every gain `>= 0` (gains are amplitudes). Default `true`.
    pub require_non_negative: bool,
    /// Reject any gain greater than this (runaway / clipping guard). `None`
    /// disables the cap. Default `Some(16.0)`.
    pub max_gain: Option<f32>,
    /// Require total energy (`Σ gainᵢ²`) at every probe to fall within
    /// `[min, max]`. `None` skips it (the universal "not silent everywhere"
    /// floor is always checked). Default `None`.
    pub energy_bounds: Option<(f32, f32)>,
    /// Continuity tolerance. `None` skips the check. Default a generous bound
    /// that only flags gross discontinuities.
    pub continuity: Option<ContinuityCheck>,
}

impl Default for ConformanceOptions {
    fn default() -> Self {
        Self {
            positions: lattice_positions(5),
            request_template: neutral_request(),
            require_non_negative: true,
            max_gain: Some(16.0),
            // step 0.05 along an axis; a continuous panner moves far less than
            // this. Generous so legitimately steep (but continuous) panners pass.
            continuity: Some(ContinuityCheck {
                step: 0.05,
                max_l2_per_step: 1.0,
                skip_radius: 0.1,
            }),
            energy_bounds: None,
        }
    }
}

/// Aggregate energy statistics gathered while probing, for diagnostics.
#[derive(Debug, Clone, Copy)]
pub struct ConformanceStats {
    pub probes: usize,
    pub min_energy: f32,
    pub max_energy: f32,
    pub max_l2_per_step: f32,
}

/// Outcome of [`check`]. Holds every failure message (so a contributor sees all
/// problems at once, not just the first) plus summary stats.
pub struct ConformanceReport {
    pub failures: Vec<String>,
    pub stats: ConformanceStats,
}

impl ConformanceReport {
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }

    /// Panic with every failure if the model did not conform. Intended to be the
    /// last line of a `#[test]`.
    pub fn assert_passed(&self) {
        assert!(
            self.passed(),
            "backend conformance failed ({} issue(s)):\n  - {}",
            self.failures.len(),
            self.failures.join("\n  - ")
        );
    }
}

fn energy(gains: &[f32]) -> f32 {
    gains.iter().map(|g| g * g).sum()
}

/// Run the contract / energy / continuity checks on `model`. Does not allocate
/// inside the timed region, but is not itself zero-alloc — use
/// [`check_zero_alloc`] for that, with a [`CountingAllocator`] installed.
pub fn check(model: &dyn GainModel, opts: &ConformanceOptions) -> ConformanceReport {
    let mut failures = Vec::new();
    let expected = model.speaker_count();
    let mut min_energy = f32::INFINITY;
    let mut max_energy = 0.0f32;
    let mut max_l2 = 0.0f32;
    let mut any_audible = false;

    // Probe each position, collecting (position, gain-vector) for the contract
    // and energy checks, and reusing it for continuity.
    let gains_at = |position: [f64; 3]| -> Result<Vec<f32>, String> {
        let mut request = opts.request_template;
        request.adm_position = position;
        let response = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            model.compute_gains(&request)
        }))
        .map_err(|payload| {
            format!(
                "compute_gains panicked at {position:?}: {}",
                panic_message(payload.as_ref())
            )
        })?;
        Ok(response.gains.to_vec())
    };

    for &position in &opts.positions {
        let gains = match gains_at(position) {
            Ok(g) => g,
            Err(e) => {
                failures.push(e);
                continue;
            }
        };

        if gains.len() != expected {
            failures.push(format!(
                "returned {} gains at {position:?}, expected speaker_count() = {expected}",
                gains.len()
            ));
            continue;
        }
        for (i, &g) in gains.iter().enumerate() {
            if !g.is_finite() {
                failures.push(format!(
                    "non-finite gain {g} for speaker {i} at {position:?}"
                ));
            } else {
                if opts.require_non_negative && g < 0.0 {
                    failures.push(format!("negative gain {g} for speaker {i} at {position:?}"));
                }
                if let Some(cap) = opts.max_gain
                    && g > cap
                {
                    failures.push(format!(
                        "gain {g} for speaker {i} at {position:?} exceeds max_gain {cap}"
                    ));
                }
            }
        }

        let e = energy(&gains);
        if e.is_finite() {
            min_energy = min_energy.min(e);
            max_energy = max_energy.max(e);
            if e > f32::EPSILON {
                any_audible = true;
            }
        }
        if let Some((lo, hi)) = opts.energy_bounds
            && e.is_finite()
            && (e < lo || e > hi)
        {
            failures.push(format!(
                "energy {e:.4} at {position:?} outside declared bounds [{lo}, {hi}]"
            ));
        }

        let from_origin =
            (position[0] * position[0] + position[1] * position[1] + position[2] * position[2])
                .sqrt();
        if let Some(c) = opts.continuity
            && from_origin >= c.skip_radius
        {
            for axis in 0..3 {
                let mut neighbour = position;
                // Step toward the interior so the neighbour stays in range.
                neighbour[axis] += if position[axis] > 0.0 {
                    -c.step
                } else {
                    c.step
                };
                if let Ok(other) = gains_at(neighbour) {
                    if other.len() == gains.len() {
                        let l2: f32 = gains
                            .iter()
                            .zip(&other)
                            .map(|(a, b)| (a - b) * (a - b))
                            .sum::<f32>()
                            .sqrt();
                        max_l2 = max_l2.max(l2);
                        if l2 > c.max_l2_per_step {
                            failures.push(format!(
                                "discontinuity: gain vector changed by L2 {l2:.4} for a {:.3} step \
                                 on axis {axis} near {position:?} (max {})",
                                c.step, c.max_l2_per_step
                            ));
                        }
                    }
                }
            }
        }
    }

    if !any_audible {
        failures.push(
            "backend is silent at every probed position (no position produced any gain)"
                .to_string(),
        );
    }

    ConformanceReport {
        failures,
        stats: ConformanceStats {
            probes: opts.positions.len(),
            min_energy: if min_energy.is_finite() {
                min_energy
            } else {
                0.0
            },
            max_energy,
            max_l2_per_step: max_l2,
        },
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic with non-string payload".to_string()
    }
}

// ── Zero-allocation checking ────────────────────────────────────────────────

thread_local! {
    /// Per-thread allocation counter. Thread-local (not a global atomic) so that
    /// [`count_allocations`] measures only the calling thread — otherwise tests
    /// running in parallel would inflate each other's counts. A `Cell<u64>` with
    /// const init has no destructor and never allocates, so reading/writing it
    /// from inside the global allocator is reentrancy-safe.
    static THREAD_ALLOCS: Cell<u64> = const { Cell::new(0) };
}

#[inline]
fn bump_thread_allocs() {
    // `try_with` avoids a panic if the TLS is being torn down; a missed count
    // during teardown is harmless for this purpose.
    let _ = THREAD_ALLOCS.try_with(|c| c.set(c.get().wrapping_add(1)));
}

/// A `#[global_allocator]` that counts allocations per thread (delegating the
/// actual work to the system allocator). Install it in a test binary to enable
/// the zero-alloc conformance check:
///
/// ```ignore
/// use renderer::backend_conformance::CountingAllocator;
/// #[global_allocator]
/// static GLOBAL: CountingAllocator = CountingAllocator;
/// ```
pub struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        bump_thread_allocs();
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        bump_thread_allocs();
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        bump_thread_allocs();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Run `f`, returning its result and the number of allocations made *on this
/// thread* during the call (as counted by [`CountingAllocator`]).
pub fn count_allocations<R>(f: impl FnOnce() -> R) -> (R, u64) {
    let before = THREAD_ALLOCS.with(Cell::get);
    let result = f();
    let after = THREAD_ALLOCS.with(Cell::get);
    (result, after.wrapping_sub(before))
}

/// Result of [`check_zero_alloc`].
pub enum ZeroAllocReport {
    /// The counting allocator was not installed, so the check could not run.
    /// Treated as a skip, not a pass, to avoid false confidence.
    Skipped,
    /// `compute_gains` allocated this many times across the probe set (0 = pass).
    Ran { allocations: u64 },
}

impl ZeroAllocReport {
    /// Assert the backend allocated nothing. A `Skipped` result also passes (the
    /// test binary opted out by not installing the allocator); pair this with a
    /// dedicated allocator-installed test if you want a hard guarantee.
    pub fn assert_zero(&self) {
        if let ZeroAllocReport::Ran { allocations } = self {
            assert_eq!(
                *allocations, 0,
                "compute_gains allocated {allocations} time(s); the hot-path contract forbids heap \
                 allocation"
            );
        }
    }
}

/// Verify `compute_gains` does not allocate, by running it over `opts.positions`
/// while watching [`ALLOCATIONS`]. Requires a [`CountingAllocator`] global
/// allocator; if it is not installed (detected via a probe allocation), returns
/// [`ZeroAllocReport::Skipped`].
pub fn check_zero_alloc(model: &dyn GainModel, opts: &ConformanceOptions) -> ZeroAllocReport {
    // Self-test: a deliberate allocation must move the counter, else the counting
    // allocator is not active and the result would be a meaningless zero.
    let (probe, delta) = count_allocations(|| Vec::<u8>::with_capacity(64));
    std::hint::black_box(probe);
    if delta == 0 {
        return ZeroAllocReport::Skipped;
    }

    let mut request = opts.request_template;
    let (_, allocations) = count_allocations(|| {
        for &position in &opts.positions {
            request.adm_position = position;
            let response = model.compute_gains(&request);
            std::hint::black_box(&response);
        }
    });
    ZeroAllocReport::Ran { allocations }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_backend::{BackendCapabilities, RenderResponse, VbapBackend};
    use crate::spatial_vbap::{Gains, VbapPanner};
    use crate::speaker_layout::SpeakerLayout;

    /// Build a real VBAP gain model from the 7.1.4 preset.
    fn vbap_714() -> VbapBackend {
        let layout = SpeakerLayout::preset("7.1.4").expect("7.1.4 preset");
        let (dirs, _) = layout.spatializable_positions();
        let panner = VbapPanner::new(&dirs, 1, 1, 0.0, Default::default())
            .expect("panner")
            .with_negative_z(true);
        VbapBackend::new(panner, crate::render_backend::VbapSpreadParams::default())
    }

    #[test]
    fn builtin_vbap_conforms() {
        let report = check(&vbap_714(), &ConformanceOptions::default());
        report.assert_passed();
        // Sanity on the gathered stats: VBAP is audible and bounded.
        assert!(report.stats.max_energy > 0.0);
        assert!(report.stats.min_energy >= 0.0);
    }

    /// A backend whose `compute_gains` misbehaves in a configurable way, so we can
    /// assert the harness *catches* each contract violation.
    struct Misbehaving {
        speakers: usize,
        mode: Mode,
    }
    #[derive(Clone, Copy)]
    enum Mode {
        Nan,
        Negative,
        TooFew,
        Silent,
        Panic,
        Runaway,
    }
    impl GainModel for Misbehaving {
        fn backend_id(&self) -> &'static str {
            "misbehaving"
        }
        fn backend_label(&self) -> &'static str {
            "Misbehaving"
        }
        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities {
                supports_realtime: true,
                ..Default::default()
            }
        }
        fn speaker_count(&self) -> usize {
            self.speakers
        }
        fn compute_gains(&self, _req: &RenderRequest) -> RenderResponse {
            let n = self.speakers;
            let mut gains = Gains::zeroed(n);
            match self.mode {
                Mode::Nan => gains[0] = f32::NAN,
                Mode::Negative => gains[0] = -0.5,
                Mode::Runaway => gains[0] = 1000.0,
                Mode::Silent => {} // all zeros, every position
                Mode::TooFew => {
                    return RenderResponse {
                        gains: Gains::zeroed(n - 1),
                    };
                }
                Mode::Panic => panic!("boom"),
            }
            RenderResponse { gains }
        }
        fn save_to_file(&self, _p: &std::path::Path, _l: &SpeakerLayout) -> anyhow::Result<()> {
            anyhow::bail!("unsupported")
        }
    }

    fn fails_with(mode: Mode, needle: &str) {
        let model = Misbehaving { speakers: 8, mode };
        let report = check(&model, &ConformanceOptions::default());
        assert!(!report.passed(), "expected failure for {needle}");
        assert!(
            report.failures.iter().any(|f| f.contains(needle)),
            "no failure mentioned {needle:?}; got: {:?}",
            report.failures
        );
    }

    #[test]
    fn harness_catches_nan() {
        fails_with(Mode::Nan, "non-finite");
    }
    #[test]
    fn harness_catches_negative() {
        fails_with(Mode::Negative, "negative gain");
    }
    #[test]
    fn harness_catches_runaway() {
        fails_with(Mode::Runaway, "exceeds max_gain");
    }
    #[test]
    fn harness_catches_wrong_count() {
        fails_with(Mode::TooFew, "expected speaker_count");
    }
    #[test]
    fn harness_catches_silence() {
        fails_with(Mode::Silent, "silent at every probed position");
    }
    #[test]
    fn harness_catches_panic() {
        fails_with(Mode::Panic, "panicked");
    }

    #[test]
    fn zero_alloc_skipped_without_counting_allocator() {
        // The renderer test binary uses the default allocator, so the self-test
        // probe sees no count movement and the check skips rather than lying.
        let report = check_zero_alloc(&vbap_714(), &ConformanceOptions::default());
        assert!(matches!(report, ZeroAllocReport::Skipped));
        report.assert_zero(); // Skipped must not panic.
    }
}
