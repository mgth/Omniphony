// Used only when saf_vbap feature is disabled; suppress warnings in the default build.
#![allow(dead_code)]

// Ported from saf_vbap.c — Copyright (c) 2017-2018 Leo McCormack, ISC License
// VBAP algorithm derived from https://github.com/polarch/Vector-Base-Amplitude-Panning
// Copyright (c) 2015, Archontis Politis, BSD-3-Clause License
//
// Pure-Rust VBAP backend: findLsTriplets, invertLsMtx3D, getSpreadSrcDirs3D,
// vbap3D, generateVBAPgainTable3D.  No unsafe code, no external dependencies.

use super::convhull::convhull_3d_build;

const ADD_DUMMY_LIMIT: f32 = 60.0;
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

// ── vbap3D ───────────────────────────────────────────────────────────────────

/// Compute VBAP (or MDAP with spread) gains for a batch of source directions.
///
/// `src_dirs`: `[azimuth_deg, elevation_deg]` per source direction.
/// `n_speakers`: total number of speakers (gains vector length).
/// `ls_groups`: triangle indices into speakers.
/// `spread_deg`: spread in degrees; 0 = pure VBAP, >0 = MDAP.
/// `layout_inv_mtx`: per-triangle 3×3 inverse speaker matrices.
///
/// Returns a flat `[n_sources × n_speakers]` gain matrix.
pub fn vbap3d(
    src_dirs: &[[f32; 2]],
    n_speakers: usize,
    ls_groups: &[[usize; 3]],
    spread_deg: f32,
    layout_inv_mtx: &[[f32; 9]],
) -> Vec<f32> {
    let n_src = src_dirs.len();
    let _n_faces = ls_groups.len();
    let mut gain_mtx = vec![0.0f32; n_src * n_speakers];

    if spread_deg > 0.1 {
        // MDAP
        const N_SPREAD_SRCS: usize = 8;
        const N_RINGS: usize = 1;

        for (ns, &[az_deg, el_deg]) in src_dirs.iter().enumerate() {
            let az_rad = az_deg * std::f32::consts::PI / 180.0;
            let el_rad = el_deg * std::f32::consts::PI / 180.0;

            let u_spread =
                get_spread_src_dirs_3d(az_rad, el_rad, spread_deg, N_SPREAD_SRCS, N_RINGS);

            let mut gains = vec![0.0f32; n_speakers];

            for u_vec in &u_spread {
                let u = *u_vec;

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
                            gains[face[0]] += g0 / rms;
                            gains[face[1]] += g1 / rms;
                            gains[face[2]] += g2 / rms;
                        }
                    }
                }
            }

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

            let mut gains = vec![0.0f32; n_speakers];

            'faces: for (fi, face) in ls_groups.iter().enumerate() {
                let inv = &layout_inv_mtx[fi];

                let g0 = inv[0] * u[0] + inv[1] * u[1] + inv[2] * u[2];
                let g1 = inv[3] * u[0] + inv[4] * u[1] + inv[5] * u[2];
                let g2 = inv[6] * u[0] + inv[7] * u[1] + inv[8] * u[2];

                let min_val = g0.min(g1).min(g2);
                if min_val > -0.001 {
                    let rms = (g0 * g0 + g1 * g1 + g2 * g2).sqrt();
                    if rms > 1e-30 {
                        gains[face[0]] = g0 / rms;
                        gains[face[1]] = g1 / rms;
                        gains[face[2]] = g2 / rms;
                    }
                    break 'faces;
                }
            }

            // Energy-normalise
            let gains_rms = gains.iter().map(|&g| g * g).sum::<f32>().sqrt();
            let out = &mut gain_mtx[ns * n_speakers..(ns + 1) * n_speakers];
            if gains_rms > 1e-30 {
                for (o, &g) in out.iter_mut().zip(gains.iter()) {
                    *o = (g / gains_rms).max(0.0);
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
/// `enable_dummies`: add virtual ±90° elevation speakers if none exist near poles.
/// `spread`: spread in degrees (0 = pure VBAP, >0 = MDAP).
pub fn generate_vbap_gain_table_3d(
    ls_dirs_deg: &[[f32; 2]],
    az_res_deg: i32,
    el_res_deg: i32,
    omit_large_triangles: bool,
    enable_dummies: bool,
    spread: f32,
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

    // Optionally add dummy speakers at ±90° elevation
    let effective_dirs: Vec<[f32; 2]>;
    let n_real = ls_dirs_deg.len();

    if enable_dummies {
        let need_dummy_neg = ls_dirs_deg.iter().all(|d| d[1] > -ADD_DUMMY_LIMIT);
        let need_dummy_pos = ls_dirs_deg.iter().all(|d| d[1] < ADD_DUMMY_LIMIT);

        if need_dummy_neg || need_dummy_pos {
            let mut dirs = ls_dirs_deg.to_vec();
            if need_dummy_neg {
                dirs.push([0.0, -90.0]);
            }
            if need_dummy_pos {
                dirs.push([0.0, 90.0]);
            }
            effective_dirs = dirs;
        } else {
            effective_dirs = ls_dirs_deg.to_vec();
        }
    } else {
        effective_dirs = ls_dirs_deg.to_vec();
    }

    let (u_spkr, ls_groups) = find_ls_triplets(&effective_dirs, omit_large_triangles)?;
    let layout_inv_mtx = invert_ls_mtx_3d(&u_spkr, &ls_groups);

    let n_eff = effective_dirs.len();
    let n_triangles = ls_groups.len();
    let n_points = n_az * n_el;

    // Compute gains for all grid directions (using effective speaker count)
    let mut gtable = vbap3d(&src_dirs, n_eff, &ls_groups, spread, &layout_inv_mtx);

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
