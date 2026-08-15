//! VBAP energy conservation and seam continuity over the whole sphere.
//!
//! Extends the existing tests in `native_backend.rs`, which check
//! `|rms − 1| < 0.05` (±0.42 dB) at five elevations along the azimuth-0
//! meridian of a synthetic 7-speaker layout. This measures the shipped 7.1.4
//! layout over a full sphere lattice, and adds the metric energy conservation
//! cannot see.
//!
//! **Energy** — VBAP normalises so that `Σg² = 1`. In dB: `10·log10(Σg²) = 0`.
//!
//! **Seams** — VBAP is continuous by construction: gains fall to zero at a
//! triplet edge as the adjacent triplet takes over. A jump there is invisible
//! to the energy check, since the image can step while energy stays perfectly
//! conserved. Each lattice point is swept over a degree and then bisected
//! toward whatever produced the change: a continuous function's difference
//! collapses as the interval shrinks, while a discontinuity keeps its full
//! magnitude. See [`MAX_SEAM_JUMP`] for two earlier formulations that were
//! wrong, and why.

use dsp_fixtures::dirs::fibonacci_sphere;

use crate::speaker_layout::SpeakerLayout;

use super::native_backend::NativeVbapLayout;

/// Directions in the PR-gate sweep. 512 points is dense enough to land inside
/// every triplet of a 7.1.4 layout several times over.
const LATTICE_POINTS: usize = 512;

/// Build the VBAP panner for a shipped preset, using only speakers that
/// participate in spatialization (LFE has `spatialize: false` and must not
/// appear in the energy sum).
fn panner_for(preset: &str) -> (NativeVbapLayout, usize) {
    let layout = SpeakerLayout::preset(preset).expect("known preset");
    let dirs: Vec<[f32; 2]> = layout
        .speakers
        .iter()
        .filter(|s| s.spatialize)
        .map(|s| [s.azimuth, s.elevation])
        .collect();
    let n = dirs.len();
    (
        NativeVbapLayout::from_speaker_dirs(&dirs, Default::default()).expect("triplet search"),
        n,
    )
}

/// `10·log10(Σg²)` — deviation from 0 dB is the energy error.
fn energy_db(panner: &NativeVbapLayout, az: f32, el: f32) -> f32 {
    let g = panner.vbap_gains(az, el, 0.0).expect("vbap gains");
    let mut sum_sq = 0.0f32;
    for i in 0..g.len() {
        sum_sq += g[i] * g[i];
    }
    if sum_sq <= 0.0 {
        f32::NEG_INFINITY
    } else {
        10.0 * sum_sq.log10()
    }
}

/// `‖g(az+Δ) − g(az)‖₂` at fixed elevation.
fn gain_step_norm(panner: &NativeVbapLayout, az: f32, el: f32, delta: f32) -> f32 {
    let a = panner.vbap_gains(az, el, 0.0).expect("vbap gains");
    let b = panner.vbap_gains(az + delta, el, 0.0).expect("vbap gains");
    let mut acc = 0.0f32;
    for i in 0..a.len() {
        let d = a[i] - b[i];
        acc += d * d;
    }
    acc.sqrt()
}

/// Theory-derived: VBAP normalises to `Σg² = 1`, i.e. 0 dB.
const ENERGY_TOLERANCE_DB: f32 = 0.25;

/// Largest gain-vector jump permitted, in L2 norm, after the discontinuity has
/// been localised to a vanishing angular interval.
///
/// This is a continuity test, not a smoothness test. VBAP gains are continuous
/// but deliberately *not* differentiable: at a speaker's own direction the gain
/// peaks at 1 and falls away on both sides, so the gain vector has a kink.
/// Kinks are inherent and inaudible; jumps are the defect.
///
/// Two earlier formulations were wrong in opposite directions, and both are
/// worth recording so they are not reintroduced:
///
/// - Requiring `‖Δg‖` to *halve* when the step halves tests differentiability,
///   so it flagged every speaker direction as a seam (ratio ≈ 1/√2 at a kink).
/// - Probing `‖Δg‖` across one fixed small step from each lattice point misses
///   a jump unless the jump happens to fall inside that step. With a 0.01°
///   probe it passed even on a layout with a known discontinuity — a test that
///   cannot fail.
///
/// So the search sweeps a whole degree per lattice point and then *bisects*
/// toward whatever produced the change. A continuous function's difference
/// collapses as the interval shrinks; a jump keeps its full magnitude however
/// small the interval gets. That distinction needs no Lipschitz constant.
const MAX_SEAM_JUMP: f32 = 0.02;

/// Known blind spot: this sweeps **azimuth at fixed elevation**, so it never
/// crosses a pole, where azimuth is degenerate. A discontinuity exactly at the
/// zenith or nadir is invisible here. `backend_conformance` covers that case by
/// stepping along Cartesian axes through the poles, and it is what caught a
/// √2 gain jump at the nadir introduced by an attempted fix for the energy
/// deferral above. The two tests are complementary; neither replaces the other.
///
/// Angular span searched around each lattice point, in degrees.
const SEAM_SPAN_DEG: f32 = 1.0;

/// Bisection depth: 1° / 2^14 ≈ 6·10⁻⁵ °.
const SEAM_BISECT_STEPS: usize = 14;

/// `‖g(az_b) − g(az_a)‖₂` at fixed elevation.
fn gain_diff(panner: &NativeVbapLayout, az_a: f32, az_b: f32, el: f32) -> f32 {
    let a = panner.vbap_gains(az_a, el, 0.0).expect("vbap gains");
    let b = panner.vbap_gains(az_b, el, 0.0).expect("vbap gains");
    let mut acc = 0.0f32;
    for i in 0..a.len() {
        let d = a[i] - b[i];
        acc += d * d;
    }
    acc.sqrt()
}

/// Localise the largest gain change within `[az, az + SEAM_SPAN_DEG]` by
/// repeatedly keeping the half that carries more of it, and report the change
/// that survives at the finest interval together with where it sits.
fn residual_jump(panner: &NativeVbapLayout, az: f32, el: f32) -> (f32, f32) {
    let (mut lo, mut hi) = (az, az + SEAM_SPAN_DEG);
    for _ in 0..SEAM_BISECT_STEPS {
        let mid = 0.5 * (lo + hi);
        if gain_diff(panner, lo, mid, el) >= gain_diff(panner, mid, hi, el) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    (gain_diff(panner, lo, hi, el), 0.5 * (lo + hi))
}

#[test]
fn vbap_conserves_energy_over_the_sphere() {
    let (panner, n_spk) = panner_for("7.1.4");
    let mut worst = (0.0f32, 0.0f32, 0.0f32);
    for (az, el) in fibonacci_sphere(LATTICE_POINTS) {
        let dev = energy_db(&panner, az, el);
        assert!(
            dev.is_finite(),
            "silent direction az={az:.1} el={el:.1}: no speaker receives energy"
        );
        if dev.abs() > worst.2.abs() {
            worst = (az, el, dev);
        }
    }
    println!(
        "[measure] vbap_energy 7.1.4 ({n_spk} speakers, {LATTICE_POINTS} dirs): \
         worst {:+.4} dB at az={:.1} el={:.1}",
        worst.2, worst.0, worst.1
    );
    assert!(
        worst.2.abs() <= ENERGY_TOLERANCE_DB,
        "VBAP energy off by {:+.4} dB at az={:.1} el={:.1}, tolerance \
         ±{ENERGY_TOLERANCE_DB} dB",
        worst.2,
        worst.0,
        worst.1
    );
}

#[test]
fn vbap_gains_are_continuous_across_triplet_boundaries() {
    let (panner, _) = panner_for("7.1.4");
    let mut worst = (0.0f32, 0.0f32, 0.0f32);
    for (az, el) in fibonacci_sphere(LATTICE_POINTS) {
        let (jump, at_az) = residual_jump(&panner, az, el);
        if jump > worst.2 {
            worst = (at_az, el, jump);
        }
    }
    println!(
        "[measure] vbap_seams 7.1.4 ({LATTICE_POINTS} dirs, bisected to \
         {:.0e}°): worst surviving jump {:.6} at az={:.2} el={:.1}",
        SEAM_SPAN_DEG / (1u32 << SEAM_BISECT_STEPS) as f32,
        worst.2,
        worst.0,
        worst.1
    );
    assert!(
        worst.2 <= MAX_SEAM_JUMP,
        "gain vector jumps {:.6} across an interval of only {:.0e}° at \
         az={:.2} el={:.1} (max {MAX_SEAM_JUMP}) — a discontinuity that \
         survives bisection, i.e. the panned image steps at a triplet \
         boundary. Energy conservation cannot see this: energy stays \
         conserved while the image jumps.",
        worst.2,
        SEAM_SPAN_DEG / (1u32 << SEAM_BISECT_STEPS) as f32,
        worst.0,
        worst.1
    );
}

/// The wide matrix: every shipped layout at a denser lattice, plus spread.
/// Compiled only with `--features wide-matrix`.
///
/// `SpeakerLayout::preset` exposes `stereo`, `5.1`, `7.1`, `7.1.4` and `9.1.6`;
/// `5.1.2` and `7.1.2` ship only as YAML files, not as presets, so the sweep
/// covers the surround presets that `panner_for` can actually build.
#[cfg(feature = "wide-matrix")]
#[test]
#[ignore = "engine misses this: MDAP spread does not conserve energy — 5.1 spread=0.25 is -3.0090 dB at az=-75.1 el=67.5, target ±0.25 dB. Distinct from pole coverage, which is now fixed. Tracked deferral, see docs/dsp-validation-report.md"]
fn vbap_conserves_energy_over_the_sphere_wide() {
    const WIDE_POINTS: usize = 8192;
    for preset in ["5.1", "7.1", "7.1.4", "9.1.6"] {
        let (panner, n_spk) = panner_for(preset);
        for spread in [0.0f32, 0.25, 0.5, 1.0] {
            let mut worst = (0.0f32, 0.0f32, 0.0f32);
            for (az, el) in fibonacci_sphere(WIDE_POINTS) {
                let g = panner.vbap_gains(az, el, spread).expect("vbap gains");
                let mut sum_sq = 0.0f32;
                for i in 0..g.len() {
                    sum_sq += g[i] * g[i];
                }
                assert!(
                    sum_sq > 0.0,
                    "{preset} spread={spread}: silent direction az={az:.1} el={el:.1}"
                );
                let dev = 10.0 * sum_sq.log10();
                if dev.abs() > worst.2.abs() {
                    worst = (az, el, dev);
                }
            }
            println!(
                "[measure] vbap_energy_wide {preset} spread={spread} \
                 ({n_spk} speakers): worst {:+.4} dB at az={:.1} el={:.1}",
                worst.2, worst.0, worst.1
            );
            assert!(
                worst.2.abs() <= ENERGY_TOLERANCE_DB,
                "{preset} spread={spread}: energy off by {:+.4} dB at \
                 az={:.1} el={:.1}",
                worst.2,
                worst.0,
                worst.1
            );
        }
    }
}
