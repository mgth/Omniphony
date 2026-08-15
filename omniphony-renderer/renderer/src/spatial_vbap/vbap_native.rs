// Used only when saf_vbap feature is disabled; suppress warnings in the default build.
#![allow(dead_code)]

// Ported from saf_vbap.c — Copyright (c) 2017-2018 Leo McCormack, ISC License
// VBAP algorithm derived from https://github.com/polarch/Vector-Base-Amplitude-Panning
// Copyright (c) 2015, Archontis Politis, BSD-3-Clause License
//
// Pure-Rust VBAP backend: findLsTriplets, invertLsMtx3D, getSpreadSrcDirs3D,
// vbap3D, generateVBAPgainTable3D.  No unsafe code, no external dependencies.

use super::convhull::convhull_3d_build;

const APERTURE_LIMIT_RAD: f32 = std::f32::consts::PI; // 180 degrees

// ── Coordinate helpers ───────────────────────────────────────────────────────

#[inline]
fn sph_to_cart(az_rad: f32, el_rad: f32) -> [f32; 3] {
    let cos_el = el_rad.cos();
    [cos_el * az_rad.cos(), cos_el * az_rad.sin(), el_rad.sin()]
}

/// 3-D cross product
#[inline]
fn cross3(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Dot product of two 3-vectors
#[inline]
fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Normalise a 3-vector; returns the original vector if the norm is tiny.
#[inline]
fn normalise3(v: [f32; 3]) -> [f32; 3] {
    let n = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if n < 1e-30 {
        v
    } else {
        [v[0] / n, v[1] / n, v[2] / n]
    }
}

// ── Analytical 3×3 matrix inverse ───────────────────────────────────────────

/// Compute the inverse of a 3×3 matrix stored row-major.
/// Returns `None` if the determinant is too small (degenerate triangle).
fn inv3x3(m: &[f32; 9]) -> Option<[f32; 9]> {
    // Cofactors
    let c00 = m[4] * m[8] - m[5] * m[7];
    let c01 = -(m[3] * m[8] - m[5] * m[6]);
    let c02 = m[3] * m[7] - m[4] * m[6];
    let c10 = -(m[1] * m[8] - m[2] * m[7]);
    let c11 = m[0] * m[8] - m[2] * m[6];
    let c12 = -(m[0] * m[7] - m[1] * m[6]);
    let c20 = m[1] * m[5] - m[2] * m[4];
    let c21 = -(m[0] * m[5] - m[2] * m[3]);
    let c22 = m[0] * m[4] - m[1] * m[3];

    let det = m[0] * c00 + m[1] * c01 + m[2] * c02;
    if det.abs() < 1e-30 {
        return None;
    }
    let inv_det = 1.0 / det;

    // Transpose of cofactor matrix (adjugate) divided by det
    Some([
        c00 * inv_det,
        c10 * inv_det,
        c20 * inv_det,
        c01 * inv_det,
        c11 * inv_det,
        c21 * inv_det,
        c02 * inv_det,
        c12 * inv_det,
        c22 * inv_det,
    ])
}

// ── findLsTriplets ───────────────────────────────────────────────────────────

/// Triangulate loudspeaker positions on the unit sphere (Delaunay via convex
/// hull).  Returns:
/// - Cartesian unit vectors for each speaker `u_spkr[i] = [x, y, z]`
/// - Triangle face indices `ls_groups[n] = [i0, i1, i2]`
///
/// Faces whose outward normal opposes the triangle centroid (concave wrap) are
/// discarded.  If `omit_large_triangles` is true, faces with any edge subtending
/// ≥ 180° are also discarded.
pub fn find_ls_triplets(
    ls_dirs_deg: &[[f32; 2]],
    omit_large_triangles: bool,
) -> Option<(Vec<[f32; 3]>, Vec<[usize; 3]>)> {
    let _n = ls_dirs_deg.len();

    // Convert speaker directions to Cartesian unit vectors
    let u_spkr: Vec<[f32; 3]> = ls_dirs_deg
        .iter()
        .map(|&[az, el]| {
            let az_r = az * std::f32::consts::PI / 180.0;
            let el_r = el * std::f32::consts::PI / 180.0;
            sph_to_cart(az_r, el_r)
        })
        .collect();

    // Build convex hull (using f64 vertices as required by convhull_3d_build)
    let verts_f64: Vec<[f64; 3]> = u_spkr
        .iter()
        .map(|&[x, y, z]| [x as f64, y as f64, z as f64])
        .collect();

    let faces = convhull_3d_build(&verts_f64)?;

    // Filter: keep faces whose normal × centroid angle < π/2
    // (i.e. outward normal pointing away from origin)
    let mut valid: Vec<[usize; 3]> = faces
        .into_iter()
        .filter(|&[i0, i1, i2]| {
            let v0 = u_spkr[i0];
            let v1 = u_spkr[i1];
            let v2 = u_spkr[i2];

            let a = [v1[0] - v0[0], v1[1] - v0[1], v1[2] - v0[2]];
            let b = [v2[0] - v1[0], v2[1] - v1[1], v2[2] - v1[2]];
            let cvec = cross3(a, b);

            let centroid = [
                (v0[0] + v1[0] + v2[0]) / 3.0,
                (v0[1] + v1[1] + v2[1]) / 3.0,
                (v0[2] + v1[2] + v2[2]) / 3.0,
            ];

            let dotcc = dot3(cvec, centroid);
            // acos(clamp(dotcc, -1, 1)) < π/2  ⟺  dotcc > 0
            // (since acos is monotonically decreasing and acos(0) = π/2)
            // We clamp to avoid NaN, matching the SAF code's SAF_MAX/SAF_MIN clamp.
            let dotcc_clamped = dotcc.clamp(-0.99999999, 0.99999999);
            dotcc_clamped.acos() < std::f32::consts::FRAC_PI_2
        })
        .collect();

    // Optional: discard faces with any edge subtending ≥ APERTURE_LIMIT_DEG
    if omit_large_triangles {
        valid.retain(|&[i0, i1, i2]| {
            let v0 = u_spkr[i0];
            let v1 = u_spkr[i1];
            let v2 = u_spkr[i2];

            let a01 = dot3(v0, v1).clamp(-1.0, 1.0).acos();
            let a12 = dot3(v1, v2).clamp(-1.0, 1.0).acos();
            let a20 = dot3(v2, v0).clamp(-1.0, 1.0).acos();

            a01 < APERTURE_LIMIT_RAD && a12 < APERTURE_LIMIT_RAD && a20 < APERTURE_LIMIT_RAD
        });
    }

    if valid.is_empty() {
        return None;
    }

    Some((u_spkr, valid))
}

// ── invertLsMtx3D ────────────────────────────────────────────────────────────

/// Pre-compute per-triangle inverse speaker matrices for VBAP.
///
/// `u_spkr`: Cartesian unit vectors for each speaker (length = n_speakers).
/// `ls_groups`: triangle face indices into `u_spkr`.
///
/// Returns one row-major 3×3 inverse matrix per triangle, flattened to `[f32; 9]`.
/// Triangles whose matrix is degenerate are returned as all-zero (never matched
/// in VBAP since min_val check will fail).
pub fn invert_ls_mtx_3d(u_spkr: &[[f32; 3]], ls_groups: &[[usize; 3]]) -> Vec<[f32; 9]> {
    ls_groups
        .iter()
        .map(|&[i0, i1, i2]| {
            let s0 = u_spkr[i0];
            let s1 = u_spkr[i1];
            let s2 = u_spkr[i2];

            // Build transposed group matrix (column vectors of speaker directions)
            // tempGroup[j*3+i] = U_spkr[ls_groups[n*3+i]*3 + j]
            let m: [f32; 9] = [
                s0[0], s1[0], s2[0], s0[1], s1[1], s2[1], s0[2], s1[2], s2[2],
            ];

            inv3x3(&m).unwrap_or([0.0; 9])
        })
        .collect()
}

fn valid_triplet_count(ls_dirs_deg: &[[f32; 2]], omit_large_triangles: bool) -> Option<usize> {
    let (_, ls_groups) = find_ls_triplets(ls_dirs_deg, omit_large_triangles)?;
    if ls_groups.is_empty() {
        None
    } else {
        Some(ls_groups.len())
    }
}

fn has_pole(ls_dirs_deg: &[[f32; 2]], pole_el_deg: f32) -> bool {
    ls_dirs_deg
        .iter()
        .any(|d| (d[1] - pole_el_deg).abs() < 1e-3)
}

fn try_dirs_with_optional_dummy(
    ls_dirs_deg: &[[f32; 2]],
    omit_large_triangles: bool,
    add_neg: bool,
    add_pos: bool,
) -> Option<(Vec<[f32; 2]>, Vec<bool>)> {
    let mut dirs = ls_dirs_deg.to_vec();
    let mut is_dummy = vec![false; ls_dirs_deg.len()];

    if add_neg {
        dirs.push([0.0, -90.0]);
        is_dummy.push(true);
    }
    if add_pos {
        dirs.push([0.0, 90.0]);
        is_dummy.push(true);
    }

    valid_triplet_count(&dirs, omit_large_triangles)?;
    Some((dirs, is_dummy))
}

// ── Out-of-hull rendering mode ───────────────────────────────────────────────

/// How directions outside the speaker convex hull are rendered.
///
/// `Blend` and `VirtualPoles` deliver the full-level, energy-conserving
/// behaviour required by ITU-R BS.2127 §6.1.1 / §7.3.10; they differ in the
/// *image* at steep out-of-hull angles. `Fade` is the historical behaviour,
/// kept selectable for comparison. See docs/dsp-validation-report.md ("Full
/// level below the hull is required") for the measurements behind each.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum OutOfHullMode {
    /// The original (pre-full-level-fix) behaviour: fold onto the single
    /// best-scoring boundary face and fade the gains by the cosine of the
    /// fold angle. Energy decays as cos²(angle outside hull) down to silence
    /// at an uncovered pole, and the argmax face selection steps where faces
    /// tie near a pole. Not standard-conformant — BS.2127 requires full
    /// level over the whole sphere.
    Fade,
    /// Fold onto the hull boundary, blending contributions from candidate
    /// faces by `score^power`. High `power` snaps to the closest face
    /// (Pulkki-like, localised image); low `power` widens the blend near
    /// face ties. The image below the hull stays as localised as the layout
    /// allows. Mechanism specific to this renderer — not in any reference.
    Blend {
        /// Blend sharpness exponent applied to the face score (a cosine in
        /// `(0, 1]`). Must be ≥ 1; the historical behaviour is 12.
        power: f32,
    },
    /// BS.2127-style: virtual loudspeakers close the hull at any pole the
    /// real layout leaves uncovered, and their gain is downmixed at `1/√n`
    /// over the speakers adjacent to the pole. Gives the standard's diffuse
    /// pole image (uniform over the nearest ring at the exact pole).
    VirtualPoles,
}

impl OutOfHullMode {
    /// Default blend sharpness — matches the historical `score^12` constant.
    pub const DEFAULT_BLEND_POWER: f32 = 12.0;
}

impl Default for OutOfHullMode {
    fn default() -> Self {
        Self::VirtualPoles
    }
}

/// A virtual pole speaker and the real speakers its gain downmixes onto.
///
/// Precomputed once per layout by [`compute_dummy_rings`] so `vbap3d` can
/// redistribute pole energy without walking the triangulation per source.
#[derive(Debug, Clone)]
pub struct DummyRing {
    /// Effective-speaker index of the virtual pole.
    pub dummy: usize,
    /// Real speakers sharing a triangulation face with the pole.
    pub ring: Vec<usize>,
    /// `1/√(ring.len())` — the power-preserving equal downmix weight.
    pub weight: f32,
}

/// Collect, for every dummy speaker, the real speakers it shares a face with.
/// The downmix over that set is what keeps the pole continuous: it does not
/// depend on which triangle matched, unlike the per-triangle fold.
pub fn compute_dummy_rings(ls_groups: &[[usize; 3]], is_dummy: &[bool]) -> Vec<DummyRing> {
    let mut rings = Vec::new();
    for (d, flag) in is_dummy.iter().enumerate() {
        if !*flag {
            continue;
        }
        let mut ring: Vec<usize> = Vec::new();
        for face in ls_groups {
            if face.contains(&d) {
                for &s in face {
                    if s != d && !is_dummy[s] && !ring.contains(&s) {
                        ring.push(s);
                    }
                }
            }
        }
        if !ring.is_empty() {
            let weight = 1.0 / (ring.len() as f32).sqrt();
            rings.push(DummyRing {
                dummy: d,
                ring,
                weight,
            });
        }
    }
    rings
}

/// True when some triangulation face contains the direction (same tolerance
/// as the `vbap3d` hit test).
fn direction_in_hull(u: [f32; 3], layout_inv_mtx: &[[f32; 9]]) -> bool {
    layout_inv_mtx.iter().any(|inv| {
        let g0 = inv[0] * u[0] + inv[1] * u[1] + inv[2] * u[2];
        let g1 = inv[3] * u[0] + inv[4] * u[1] + inv[5] * u[2];
        let g2 = inv[6] * u[0] + inv[7] * u[1] + inv[8] * u[2];
        g0.min(g1).min(g2) > -0.001
    })
}

/// Return speaker directions usable for VBAP triangulation.
///
/// The real layout is tried first. If triangulation fails, virtual speakers are
/// injected at the poles only as a fallback. The accompanying `is_dummy` flags
/// match the returned effective direction list.
///
/// In [`OutOfHullMode::VirtualPoles`], a successful real-layout triangulation
/// is additionally checked for pole coverage, and a virtual speaker is
/// injected at each pole the hull leaves uncovered — that is what closes the
/// hull so no direction ever needs the fold path.
pub fn prepare_effective_speaker_dirs(
    ls_dirs_deg: &[[f32; 2]],
    omit_large_triangles: bool,
    enable_dummies: bool,
    mode: OutOfHullMode,
) -> Option<(Vec<[f32; 2]>, Vec<bool>)> {
    if let Some(result) =
        try_dirs_with_optional_dummy(ls_dirs_deg, omit_large_triangles, false, false)
    {
        if !enable_dummies || !matches!(mode, OutOfHullMode::VirtualPoles) {
            return Some(result);
        }
        // VirtualPoles: close every pole the real hull leaves uncovered.
        let (u_spkr, ls_groups) = find_ls_triplets(&result.0, omit_large_triangles)?;
        let inv_mtx = invert_ls_mtx_3d(&u_spkr, &ls_groups);
        let half_pi = std::f32::consts::FRAC_PI_2;
        let add_neg = !has_pole(ls_dirs_deg, -90.0)
            && !direction_in_hull(sph_to_cart(0.0, -half_pi), &inv_mtx);
        let add_pos =
            !has_pole(ls_dirs_deg, 90.0) && !direction_in_hull(sph_to_cart(0.0, half_pi), &inv_mtx);
        if !add_neg && !add_pos {
            return Some(result);
        }
        // If the pole-closed triangulation fails, keep the open real hull —
        // vbap3d then falls back to the fold path for out-of-hull directions.
        return try_dirs_with_optional_dummy(ls_dirs_deg, omit_large_triangles, add_neg, add_pos)
            .or(Some(result));
    }

    if !enable_dummies {
        return None;
    }

    let can_add_neg = !has_pole(ls_dirs_deg, -90.0);
    let can_add_pos = !has_pole(ls_dirs_deg, 90.0);

    if can_add_neg || can_add_pos {
        return try_dirs_with_optional_dummy(
            ls_dirs_deg,
            omit_large_triangles,
            can_add_neg,
            can_add_pos,
        );
    }

    None
}

// ── getSpreadSrcDirs3D ────────────────────────────────────────────────────────

/// Generate a ring of spread directions around a source (MDAP helper).
///
/// Returns `num_rings * num_src + 1` Cartesian unit vectors.
/// The last entry is the original source direction (central source).
pub fn get_spread_src_dirs_3d(
    az_rad: f32,
    el_rad: f32,
    spread_deg: f32,
    num_src: usize,
    num_rings: usize,
) -> Vec<[f32; 3]> {
    let u = sph_to_cart(az_rad, el_rad);

    // Rodrigues rotation matrix R_θ around axis u, θ = 2π/num_src
    let theta = 2.0 * std::f32::consts::PI / num_src as f32;
    let sin_t = theta.sin();
    let cos_t = theta.cos();

    // u⊗u (outer product)
    let uxu = [
        [u[0] * u[0], u[0] * u[1], u[0] * u[2]],
        [u[0] * u[1], u[1] * u[1], u[1] * u[2]],
        [u[0] * u[2], u[1] * u[2], u[2] * u[2]],
    ];
    // [u]× (cross-product matrix)
    let ux = [[0.0, -u[2], u[1]], [u[2], 0.0, -u[0]], [-u[1], u[0], 0.0]];
    // R_θ[i][j] = sin_t * ux[i][j] + (1 - cos_t) * uxu[i][j] + cos_t * δ_ij
    let mut r = [[0.0f32; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] =
                sin_t * ux[i][j] + (1.0 - cos_t) * uxu[i][j] + if i == j { cos_t } else { 0.0 };
        }
    }

    // First spread base vector: perpendicular to u
    let first_base: [f32; 3] = if el_rad > std::f32::consts::FRAC_PI_2 - 0.01
        || el_rad < -(std::f32::consts::FRAC_PI_2 - 0.01)
    {
        [1.0, 0.0, 0.0] // near pole: use X axis
    } else {
        let u2 = [0.0f32, 0.0, 1.0];
        normalise3(cross3(u, u2))
    };

    // Build ring by repeated rotation
    let mut spreadbase = vec![[0.0f32; 3]; num_src];
    spreadbase[0] = first_base;
    for ns in 1..num_src {
        let prev = spreadbase[ns - 1];
        spreadbase[ns] = [
            r[0][0] * prev[0] + r[0][1] * prev[1] + r[0][2] * prev[2],
            r[1][0] * prev[0] + r[1][1] * prev[1] + r[1][2] * prev[2],
            r[2][0] * prev[0] + r[2][1] * prev[1] + r[2][2] * prev[2],
        ];
    }

    // Squeeze ring to desired spread; build output
    let spread_rad = (spread_deg / 2.0) * std::f32::consts::PI / 180.0;
    let ring_rad = spread_rad / num_rings as f32;

    let total = num_rings * num_src + 1;
    let mut out = vec![[0.0f32; 3]; total];

    // Normalisation factor from first vector of first ring
    let tan_r = ring_rad.tan();
    let raw0 = [
        u[0] + spreadbase[0][0] * tan_r,
        u[1] + spreadbase[0][1] * tan_r,
        u[2] + spreadbase[0][2] * tan_r,
    ];
    let norm0 = (raw0[0] * raw0[0] + raw0[1] * raw0[1] + raw0[2] * raw0[2]).sqrt();
    let norm0 = if norm0 < 1e-30 { 1.0 } else { norm0 };

    for nr in 0..num_rings {
        let tan_ring = ((nr as f32 + 1.0) * ring_rad).tan();
        for ns in 0..num_src {
            let sb = spreadbase[ns];
            out[nr * num_src + ns] = [
                (u[0] + sb[0] * tan_ring) / norm0,
                (u[1] + sb[1] * tan_ring) / norm0,
                (u[2] + sb[2] * tan_ring) / norm0,
            ];
        }
    }

    // Append central source direction
    out[num_rings * num_src] = u;

    out
}

// ── Dummy-speaker redistribution ─────────────────────────────────────────────

/// Move the gain assigned to dummy (virtual) speakers in a matched triangle
/// onto the real speakers of the same triangle, preserving total energy.
///
/// When the layout has no real speakers near ±90° elevation, dummy speakers are
/// injected at the poles so the convex-hull triangulation succeeds. Without
/// this redistribution, a source at (or near) a pole would land in a triangle
/// whose dummy vertex absorbs all of the gain, only to be silently dropped
/// when the gain matrix is trimmed back to the real-speaker count.
#[inline]
fn redistribute_dummy_in_triangle(g: [f32; 3], face: [usize; 3], is_dummy: &[bool]) -> [f32; 3] {
    let mask = [is_dummy[face[0]], is_dummy[face[1]], is_dummy[face[2]]];
    if !(mask[0] || mask[1] || mask[2]) {
        return g;
    }
    let dummy_sum: f32 = (0..3).filter(|i| mask[*i]).map(|i| g[i]).sum();
    let real_sum: f32 = (0..3).filter(|i| !mask[*i]).map(|i| g[i]).sum();
    let n_real_in_face = mask.iter().filter(|d| !**d).count();

    let mut out = [0.0_f32; 3];
    for i in 0..3 {
        if mask[i] {
            continue;
        }
        out[i] = if real_sum.abs() > 1e-30 {
            g[i] + dummy_sum * (g[i] / real_sum)
        } else if n_real_in_face > 0 {
            dummy_sum / n_real_in_face as f32
        } else {
            0.0
        };
    }
    out
}

/// Fold an out-of-hull direction onto the hull boundary: for each face, clamp
/// the raw inverse-matrix gains to ≥ 0 and score the candidate by how close the
/// direction those clamped gains actually radiate from (recovered through the
/// face's speaker matrix) lies to the requested one. The best-scoring candidate
/// wins, so content outside the speaker hull renders on the nearest boundary
/// edge/face instead of dropping to silence (issue #169).
///
/// Returns unit-energy gains over all speakers, at **full level** — there is no
/// attenuation based on how far outside the hull a direction lies. A direction
/// outside the hull cannot be reproduced *there*, but it must still be
/// reproduced at full level from the closest direction that can be. This
/// matches ITU-R BS.2127 §6.1.1, which makes full-sphere coverage a defining
/// property of a valid point-source panner, and §7.3.10, where out-of-range
/// positions clamp to the boundary at full gain ("the sum of the squares of the
/// loudspeaker gains will always be 1").
///
/// Contributions from several boundary faces are blended by `score^power`
/// rather than snapping to the single best face, which keeps the gain field
/// continuous at the poles where faces are equidistant. Note this is *not* how
/// the references get there: BS.2127 inserts a virtual loudspeaker and
/// downmixes it at 1/√n over the adjacent ring (available here as
/// [`OutOfHullMode::VirtualPoles`]), while Pulkki's own implementations snap
/// to one face (and are discontinuous below the hull as a result). Same level,
/// different image at steep angles — see docs/dsp-validation-report.md.
/// Cold path: runs only for directions no face contains — bed poses
/// behind/below sparse layouts and the out-of-hull cells of table generation.
///
/// Writes unit-energy gains into `acc` (fully overwritten; caller-provided so
/// the sweep loops allocate nothing per direction). Returns `false` when no
/// face faces the direction, in which case `acc` is all zeros.
fn fold_out_of_hull(
    u: [f32; 3],
    ls_groups: &[[usize; 3]],
    layout_inv_mtx: &[[f32; 9]],
    is_dummy: &[bool],
    power: f32,
    acc: &mut [f32],
) -> bool {
    acc.fill(0.0);
    let mut any = false;

    for (fi, face) in ls_groups.iter().enumerate() {
        if is_dummy[face[0]] || is_dummy[face[1]] || is_dummy[face[2]] {
            continue;
        }
        let inv = &layout_inv_mtx[fi];
        let c0 = (inv[0] * u[0] + inv[1] * u[1] + inv[2] * u[2]).max(0.0);
        let c1 = (inv[3] * u[0] + inv[4] * u[1] + inv[5] * u[2]).max(0.0);
        let c2 = (inv[6] * u[0] + inv[7] * u[1] + inv[8] * u[2]).max(0.0);
        let rms = (c0 * c0 + c1 * c1 + c2 * c2).sqrt();
        if rms <= 1e-20 {
            continue;
        }
        let Some(m) = inv3x3(inv) else { continue };
        let v = normalise3([
            m[0] * c0 + m[1] * c1 + m[2] * c2,
            m[3] * c0 + m[4] * c1 + m[5] * c2,
            m[6] * c0 + m[7] * c1 + m[8] * c2,
        ]);
        let score = dot3(u, v);
        if score <= 0.0 {
            continue;
        }
        // `score ∈ (0, 1]` so `powf` is well-defined for any `power ≥ 1`.
        let w = score.powf(power);
        acc[face[0]] += w * c0 / rms;
        acc[face[1]] += w * c1 / rms;
        acc[face[2]] += w * c2 / rms;
        any = true;
    }

    if !any {
        return false;
    }
    let norm: f32 = acc.iter().map(|g| g * g).sum::<f32>().sqrt();
    if norm <= 1e-20 {
        acc.fill(0.0);
        return false;
    }
    let inv_norm = 1.0 / norm;
    for g in acc.iter_mut() {
        *g *= inv_norm;
    }
    true
}

/// The original out-of-hull fold ([`OutOfHullMode::Fade`]), byte-faithful to
/// the pre-full-level-fix implementation: pick the single best-scoring
/// boundary face and return its clamped gains plus the fold-angle cosine as a
/// fade. The caller applies the fade and, on the pure-VBAP path, bypasses the
/// energy normalise so the fade survives — energy decays as cos²(fold angle).
fn fold_out_of_hull_argmax(
    u: [f32; 3],
    ls_groups: &[[usize; 3]],
    layout_inv_mtx: &[[f32; 9]],
    is_dummy: &[bool],
) -> Option<(usize, [f32; 3], f32)> {
    let mut best_score = f32::NEG_INFINITY;
    let mut best: Option<(usize, [f32; 3])> = None;
    for (fi, face) in ls_groups.iter().enumerate() {
        let inv = &layout_inv_mtx[fi];
        let c0 = (inv[0] * u[0] + inv[1] * u[1] + inv[2] * u[2]).max(0.0);
        let c1 = (inv[3] * u[0] + inv[4] * u[1] + inv[5] * u[2]).max(0.0);
        let c2 = (inv[6] * u[0] + inv[7] * u[1] + inv[8] * u[2]).max(0.0);
        let rms = (c0 * c0 + c1 * c1 + c2 * c2).sqrt();
        if rms <= 1e-30 {
            continue;
        }
        // Invert back to the face's speaker-direction matrix: the candidate's
        // radiated direction is the clamped-gain combination of the speakers.
        let Some(m) = inv3x3(inv) else { continue };
        let v = normalise3([
            m[0] * c0 + m[1] * c1 + m[2] * c2,
            m[3] * c0 + m[4] * c1 + m[5] * c2,
            m[6] * c0 + m[7] * c1 + m[8] * c2,
        ]);
        let score = dot3(u, v);
        if score > best_score {
            best_score = score;
            best = Some((
                fi,
                redistribute_dummy_in_triangle([c0 / rms, c1 / rms, c2 / rms], *face, is_dummy),
            ));
        }
    }
    if best_score <= 0.0 {
        return None;
    }
    best.map(|(fi, g)| (fi, g, best_score.min(1.0)))
}

/// Downmix any gain landed on virtual pole speakers onto their adjacent real
/// ring at `1/√n`, then silence the pole entries. Distribution is independent
/// of which triangle matched, so the field stays continuous across the pole.
#[inline]
fn downmix_dummy_rings(gains: &mut [f32], dummy_rings: &[DummyRing]) {
    for r in dummy_rings {
        let gd = gains[r.dummy];
        if gd > 0.0 {
            for &s in &r.ring {
                gains[s] += gd * r.weight;
            }
            gains[r.dummy] = 0.0;
        }
    }
}

// ── vbap3D ───────────────────────────────────────────────────────────────────

/// Compute VBAP (or MDAP with spread) gains for a batch of source directions.
///
/// `src_dirs`: `[azimuth_deg, elevation_deg]` per source direction.
/// `n_speakers`: total number of speakers (gains vector length).
/// `ls_groups`: triangle indices into speakers.
/// `spread_deg`: spread in degrees; 0 = pure VBAP, >0 = MDAP.
/// `layout_inv_mtx`: per-triangle 3×3 inverse speaker matrices.
/// `is_dummy`: per-speaker flag (length `n_speakers`); when set, the speaker is
/// a virtual pole inserted to make the triangulation 3-D. In
/// [`OutOfHullMode::Blend`] its gain is folded back into the real speakers of
/// the matched triangle; in [`OutOfHullMode::VirtualPoles`] it is downmixed at
/// `1/√n` over the pole's adjacent ring (`dummy_rings`, precomputed by
/// [`compute_dummy_rings`] — pass `&[]` when `is_dummy` is all false).
///
/// Returns a flat `[n_sources × n_speakers]` gain matrix.
#[allow(clippy::too_many_arguments)] // C-port style signature, matches the rest of the module
pub fn vbap3d(
    src_dirs: &[[f32; 2]],
    n_speakers: usize,
    ls_groups: &[[usize; 3]],
    spread_deg: f32,
    layout_inv_mtx: &[[f32; 9]],
    is_dummy: &[bool],
    mode: OutOfHullMode,
    dummy_rings: &[DummyRing],
) -> Vec<f32> {
    debug_assert_eq!(is_dummy.len(), n_speakers);
    let n_src = src_dirs.len();
    let _n_faces = ls_groups.len();
    let mut gain_mtx = vec![0.0f32; n_src * n_speakers];

    // In VirtualPoles the pole gain is redistributed ring-wide after the face
    // pass, so the per-triangle fold must not touch it first.
    let per_triangle_fold = !matches!(mode, OutOfHullMode::VirtualPoles);
    // Legacy fade: the original argmax fold, with its cos² decay.
    let legacy_fade = matches!(mode, OutOfHullMode::Fade);
    // Out-of-hull directions cannot occur in VirtualPoles once the hull is
    // closed; the fold stays as a safety net (at the historical sharpness)
    // for the degenerate case where the pole triangulation failed.
    let fold_power = match mode {
        OutOfHullMode::Blend { power } => power.max(1.0),
        _ => OutOfHullMode::DEFAULT_BLEND_POWER,
    };
    // Scratch buffers reused across sources and spread members — the sweep
    // allocates nothing per direction.
    let mut gains = vec![0.0f32; n_speakers];
    let mut fold_acc = vec![0.0f32; n_speakers];

    if spread_deg > 0.1 {
        // MDAP
        const N_SPREAD_SRCS: usize = 8;
        const N_RINGS: usize = 1;

        for (ns, &[az_deg, el_deg]) in src_dirs.iter().enumerate() {
            let az_rad = az_deg * std::f32::consts::PI / 180.0;
            let el_rad = el_deg * std::f32::consts::PI / 180.0;

            let u_spread =
                get_spread_src_dirs_3d(az_rad, el_rad, spread_deg, N_SPREAD_SRCS, N_RINGS);

            gains.fill(0.0);

            for u_vec in &u_spread {
                let u = *u_vec;
                let mut hit = false;

                // Find matching triangle and accumulate gains
                for (fi, face) in ls_groups.iter().enumerate() {
                    let inv = &layout_inv_mtx[fi];

                    let g0 = inv[0] * u[0] + inv[1] * u[1] + inv[2] * u[2];
                    let g1 = inv[3] * u[0] + inv[4] * u[1] + inv[5] * u[2];
                    let g2 = inv[6] * u[0] + inv[7] * u[1] + inv[8] * u[2];

                    let min_val = g0.min(g1).min(g2);
                    if min_val > -0.001 {
                        let rms = (g0 * g0 + g1 * g1 + g2 * g2).sqrt();
                        if rms > 1e-30 {
                            let raw = [g0 / rms, g1 / rms, g2 / rms];
                            let gr = if per_triangle_fold {
                                redistribute_dummy_in_triangle(raw, *face, is_dummy)
                            } else {
                                raw
                            };
                            gains[face[0]] += gr[0];
                            gains[face[1]] += gr[1];
                            gains[face[2]] += gr[2];
                        }
                        hit = true;
                    }
                }

                // Out-of-hull spread source: fold onto the hull boundary so the
                // spread cloud keeps its below/behind-hull energy, at full
                // level like the non-spread path.
                //
                // Divergence worth recording: Pulkki's own spread path
                // (`additive_vbap` in his Pd external, and rvbap.c) guards its
                // accumulate with `if (gains_modified != 1)`, so an out-of-hull
                // spread member contributes *nothing*. Folding it in at full
                // weight is the opposite choice. It keeps a spread source's
                // energy constant as it crosses the hull boundary, which is
                // what the energy gate asserts, but it is not what Pulkki does.
                if !hit {
                    if legacy_fade {
                        // Original behaviour: single argmax face, contribution
                        // faded by the fold angle so far-outside members
                        // contribute less to the mix.
                        if let Some((fi, gr, fade)) =
                            fold_out_of_hull_argmax(u, ls_groups, layout_inv_mtx, is_dummy)
                        {
                            let face = ls_groups[fi];
                            gains[face[0]] += gr[0] * fade;
                            gains[face[1]] += gr[1] * fade;
                            gains[face[2]] += gr[2] * fade;
                        }
                    } else if fold_out_of_hull(
                        u,
                        ls_groups,
                        layout_inv_mtx,
                        is_dummy,
                        fold_power,
                        &mut fold_acc,
                    ) {
                        for (g, &add) in gains.iter_mut().zip(fold_acc.iter()) {
                            *g += add;
                        }
                    }
                }
            }

            downmix_dummy_rings(&mut gains, dummy_rings);

            // Energy-normalise and clamp to ≥ 0
            let gains_rms = gains.iter().map(|&g| g * g).sum::<f32>().sqrt();
            let out = &mut gain_mtx[ns * n_speakers..(ns + 1) * n_speakers];
            if gains_rms > 1e-30 {
                for (o, &g) in out.iter_mut().zip(gains.iter()) {
                    *o = (g / gains_rms).max(0.0);
                }
            }
        }
    } else {
        // Pure VBAP
        for (ns, &[az_deg, el_deg]) in src_dirs.iter().enumerate() {
            let az_rad = az_deg * std::f32::consts::PI / 180.0;
            let el_rad = el_deg * std::f32::consts::PI / 180.0;
            let u = sph_to_cart(az_rad, el_rad);

            gains.fill(0.0);
            let mut hit = false;

            'faces: for (fi, face) in ls_groups.iter().enumerate() {
                let inv = &layout_inv_mtx[fi];

                let g0 = inv[0] * u[0] + inv[1] * u[1] + inv[2] * u[2];
                let g1 = inv[3] * u[0] + inv[4] * u[1] + inv[5] * u[2];
                let g2 = inv[6] * u[0] + inv[7] * u[1] + inv[8] * u[2];

                let min_val = g0.min(g1).min(g2);
                if min_val > -0.001 {
                    let rms = (g0 * g0 + g1 * g1 + g2 * g2).sqrt();
                    if rms > 1e-30 {
                        let raw = [g0 / rms, g1 / rms, g2 / rms];
                        let gr = if per_triangle_fold {
                            redistribute_dummy_in_triangle(raw, *face, is_dummy)
                        } else {
                            raw
                        };
                        gains[face[0]] = gr[0];
                        gains[face[1]] = gr[1];
                        gains[face[2]] = gr[2];
                    }
                    hit = true;
                    break 'faces;
                }
            }

            // Out-of-hull: no face contains the direction; fold onto the hull
            // boundary instead of leaving the source silent. In `Fade` the
            // folded gains bypass the energy normalise below so the cos fade
            // survives (the original behaviour); in `Blend` they take the
            // same normalise as every other path.
            let mut folded_faded = false;
            if !hit {
                if legacy_fade {
                    if let Some((fi, gr, fade)) =
                        fold_out_of_hull_argmax(u, ls_groups, layout_inv_mtx, is_dummy)
                    {
                        let face = ls_groups[fi];
                        gains[face[0]] = gr[0] * fade;
                        gains[face[1]] = gr[1] * fade;
                        gains[face[2]] = gr[2] * fade;
                        folded_faded = true;
                    }
                } else if fold_out_of_hull(
                    u,
                    ls_groups,
                    layout_inv_mtx,
                    is_dummy,
                    fold_power,
                    &mut fold_acc,
                ) {
                    gains[..n_speakers].copy_from_slice(&fold_acc);
                }
            }

            downmix_dummy_rings(&mut gains, dummy_rings);

            let out = &mut gain_mtx[ns * n_speakers..(ns + 1) * n_speakers];
            if folded_faded {
                // Preserve the fade: clamp only, no energy normalise.
                for (o, &g) in out.iter_mut().zip(gains.iter()) {
                    *o = g.max(0.0);
                }
            } else {
                // Energy-normalise
                let gains_rms = gains.iter().map(|&g| g * g).sum::<f32>().sqrt();
                if gains_rms > 1e-30 {
                    for (o, &g) in out.iter_mut().zip(gains.iter()) {
                        *o = (g / gains_rms).max(0.0);
                    }
                }
            }
        }
    }

    gain_mtx
}

// ── generateVBAPgainTable3D ──────────────────────────────────────────────────

/// Build a complete VBAP gain table over an azimuth × elevation grid.
///
/// Returns `(gtable, n_gtable, n_triangles)` where:
/// - `gtable[dir_idx * n_speakers + spk_idx]` is the gain for that direction/speaker pair
/// - `n_gtable = N_azi × N_el`
/// - `n_triangles` is the number of valid Delaunay triangles found
///
/// `ls_dirs_deg`: speaker directions `[az, el]` in degrees.
/// `az_res_deg` / `el_res_deg`: grid resolution.
/// `omit_large_triangles`: filter faces with edge ≥ 180°.
/// `enable_dummies`: add virtual ±90° elevation speakers only if triangulation
/// of the real layout fails (or, in [`OutOfHullMode::VirtualPoles`], at any
/// uncovered pole).
/// `spread`: spread in degrees (0 = pure VBAP, >0 = MDAP).
/// `mode`: out-of-hull rendering mode (see [`OutOfHullMode`]).
pub fn generate_vbap_gain_table_3d(
    ls_dirs_deg: &[[f32; 2]],
    az_res_deg: i32,
    el_res_deg: i32,
    omit_large_triangles: bool,
    enable_dummies: bool,
    spread: f32,
    mode: OutOfHullMode,
) -> Option<(Vec<f32>, usize, usize)> {
    let n_az = ((360.0 / az_res_deg as f32) + 1.5) as usize;
    let n_el = ((180.0 / el_res_deg as f32) + 1.5) as usize;

    // Build source direction grid [-180:az_res:180] × [-90:el_res:90]
    let mut src_dirs: Vec<[f32; 2]> = Vec::with_capacity(n_az * n_el);
    for el_i in 0..n_el {
        let el = -90.0 + el_i as f32 * el_res_deg as f32;
        for az_i in 0..n_az {
            let az = -180.0 + az_i as f32 * az_res_deg as f32;
            src_dirs.push([az, el]);
        }
    }

    let n_real = ls_dirs_deg.len();
    let (effective_dirs, is_dummy) =
        prepare_effective_speaker_dirs(ls_dirs_deg, omit_large_triangles, enable_dummies, mode)?;

    let (u_spkr, ls_groups) = find_ls_triplets(&effective_dirs, omit_large_triangles)?;
    let layout_inv_mtx = invert_ls_mtx_3d(&u_spkr, &ls_groups);
    let dummy_rings = match mode {
        OutOfHullMode::VirtualPoles => compute_dummy_rings(&ls_groups, &is_dummy),
        _ => Vec::new(),
    };

    let n_eff = effective_dirs.len();
    let n_triangles = ls_groups.len();
    let n_points = n_az * n_el;

    // Compute gains for all grid directions (using effective speaker count)
    let mut gtable = vbap3d(
        &src_dirs,
        n_eff,
        &ls_groups,
        spread,
        &layout_inv_mtx,
        &is_dummy,
        mode,
        &dummy_rings,
    );

    // Strip dummy speaker columns — shrink each row from n_eff to n_real
    if n_eff > n_real {
        let mut trimmed = vec![0.0f32; n_points * n_real];
        for i in 0..n_points {
            trimmed[i * n_real..(i + 1) * n_real]
                .copy_from_slice(&gtable[i * n_eff..i * n_eff + n_real]);
        }
        gtable = trimmed;
    }

    Some((gtable, n_points, n_triangles))
}
