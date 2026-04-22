// Used only when saf_vbap feature is disabled; suppress warnings in the default build.
#![allow(dead_code)]

// Ported from convhull_3d.c
// Copyright (c) 2017-2018 Leo McCormack, MIT License
// Originally derived from "computational-geometry-toolbox" by
// George Papazafeiropoulos (c) 2014, BSD-2-Clause License
//
// Reference: Barber, Dobkin, Huhdanpaa, "The Quickhull Algorithm for Convex Hull",
// Geometry Center Technical Report GCG53, July 30, 1993.
//
// Compute the 3-D convex hull of a set of points using the incremental Quickhull
// algorithm. Used by `find_ls_triplets` to triangulate loudspeaker positions.

const D: usize = 3; // Dimensions
const S: usize = 4; // Stride = D+1 (homogeneous coordinate)
const NOISE_VAL: f64 = 1e-7; // Small noise to avoid degenerate configurations
const MAX_FACES: usize = 50_000; // Safety cap matching the original C code

// ── Deterministic pseudo-random noise (XorShift32) ──────────────────────────

struct Xorshift32(u32);
impl Xorshift32 {
    #[inline]
    fn next(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x as f64) / (u32::MAX as f64 + 1.0)
    }
}

// ── Plane helpers ────────────────────────────────────────────────────────────

/// 4×4 determinant (explicit expansion). Input is a row-major flat array of 16 doubles.
#[inline]
fn det_4x4(m: &[f64; 16]) -> f64 {
    m[3] * m[6] * m[9] * m[12] - m[2] * m[7] * m[9] * m[12] - m[3] * m[5] * m[10] * m[12]
        + m[1] * m[7] * m[10] * m[12]
        + m[2] * m[5] * m[11] * m[12]
        - m[1] * m[6] * m[11] * m[12]
        - m[3] * m[6] * m[8] * m[13]
        + m[2] * m[7] * m[8] * m[13]
        + m[3] * m[4] * m[10] * m[13]
        - m[0] * m[7] * m[10] * m[13]
        - m[2] * m[4] * m[11] * m[13]
        + m[0] * m[6] * m[11] * m[13]
        + m[3] * m[5] * m[8] * m[14]
        - m[1] * m[7] * m[8] * m[14]
        - m[3] * m[4] * m[9] * m[14]
        + m[0] * m[7] * m[9] * m[14]
        + m[1] * m[4] * m[11] * m[14]
        - m[0] * m[5] * m[11] * m[14]
        - m[2] * m[5] * m[8] * m[15]
        + m[1] * m[6] * m[8] * m[15]
        + m[2] * m[4] * m[9] * m[15]
        - m[0] * m[6] * m[9] * m[15]
        - m[1] * m[4] * m[10] * m[15]
        + m[0] * m[5] * m[10] * m[15]
}

/// Compute the plane coefficients (normal `c`, offset `d`) for a triangle
/// specified by three row-major 3-D points stored in `p[0..9]`
/// (p[0..3] = point 0, p[3..6] = point 1, p[6..9] = point 2).
///
/// The plane equation is:  c · x + d = 0.
fn plane_3d(p: &[f64; 9]) -> ([f64; 3], f64) {
    // Edge vectors
    let pdiff = [
        [p[3] - p[0], p[4] - p[1], p[5] - p[2]], // p1 - p0
        [p[6] - p[3], p[7] - p[4], p[8] - p[5]], // p2 - p1
    ];

    let mut c = [0.0f64; 3];
    let mut sign = 1.0f64;
    for i in 0..3usize {
        // 2×2 minor of pdiff, excluding column i
        let cols: [usize; 2] = match i {
            0 => [1, 2],
            1 => [0, 2],
            _ => [0, 1],
        };
        let det = pdiff[0][cols[0]] * pdiff[1][cols[1]] - pdiff[1][cols[0]] * pdiff[0][cols[1]];
        c[i] = sign * det;
        sign = -sign;
    }

    let norm = (c[0] * c[0] + c[1] * c[1] + c[2] * c[2]).sqrt();
    if norm < 1e-15 {
        return ([0.0, 0.0, 1.0], 0.0);
    }
    let c = [c[0] / norm, c[1] / norm, c[2] / norm];
    let d = -(p[0] * c[0] + p[1] * c[1] + p[2] * c[2]);
    (c, d)
}

// ── Main public function ─────────────────────────────────────────────────────

/// Compute the 3-D convex hull of `in_vertices` and return the triangle face
/// indices, or `None` if the triangulation fails (too few points, degenerate
/// point set, or exceeded `MAX_FACES`).
///
/// Each returned face is `[i0, i1, i2]` where the indices index into
/// `in_vertices`. Face normals are oriented outward.
pub fn convhull_3d_build(in_vertices: &[[f64; 3]]) -> Option<Vec<[usize; 3]>> {
    let n_vert = in_vertices.len();
    if n_vert <= D {
        return None;
    }

    let mut rng = Xorshift32(12345);

    // ── Build padded point matrix: each row = [x+ε, y+ε, z+ε, 1.0] ──────────
    let mut points = vec![0.0f64; n_vert * S];
    for i in 0..n_vert {
        for j in 0..D {
            points[i * S + j] = in_vertices[i][j] + NOISE_VAL * rng.next();
        }
        points[i * S + D] = 1.0;
    }

    // ── Span check ────────────────────────────────────────────────────────────
    let mut span = [0.0f64; D];
    for j in 0..D {
        let mut max_p = f64::NEG_INFINITY;
        let mut min_p = f64::INFINITY;
        for i in 0..n_vert {
            let v = points[i * S + j];
            if v > max_p {
                max_p = v;
            }
            if v < min_p {
                min_p = v;
            }
        }
        span[j] = max_p - min_p;
        if span[j] <= 1e-7 {
            return None;
        }
    }

    // ── Initial simplex: D+1 = 4 faces using vertices 0..4 ───────────────────
    let mut n_faces = D + 1;
    // Face i contains all initial vertices except vertex i
    let mut faces: Vec<usize> = vec![0; n_faces * D];
    for i in 0..n_faces {
        let mut k = 0;
        for j in 0..=D {
            if j != i {
                faces[i * D + k] = j;
                k += 1;
            }
        }
    }

    // ── Plane coefficients for initial faces ──────────────────────────────────
    let mut cf = vec![0.0f64; n_faces * D]; // normal components (row-major per face)
    let mut df = vec![0.0f64; n_faces]; // plane offsets
    let mut p_s = [0.0f64; 9]; // 3×3 workspace

    for i in 0..n_faces {
        for j in 0..D {
            for k in 0..D {
                p_s[j * D + k] = points[faces[i * D + j] * S + k];
            }
        }
        let (cfi, dfi) = plane_3d(&p_s);
        cf[i * D..i * D + D].copy_from_slice(&cfi);
        df[i] = dfi;
    }

    // ── Orient initial simplex faces outward ──────────────────────────────────
    let mut a_mat = [0.0f64; 16]; // 4×4 workspace
    for k in 0..=D {
        // Face k is opposite vertex k; rows 0..D = face vertices, row D = vertex k
        for i in 0..D {
            for l in 0..=D {
                a_mat[i * S + l] = points[faces[k * D + i] * S + l];
            }
        }
        for l in 0..=D {
            a_mat[D * S + l] = points[k * S + l]; // opposite vertex = k
        }
        if det_4x4(&a_mat) < 0.0 {
            faces.swap(k * D + 1, k * D + 2);
            for j in 0..D {
                cf[k * D + j] = -cf[k * D + j];
            }
            df[k] = -df[k];
        }
    }

    // ── Early exit for minimal input ──────────────────────────────────────────
    let remaining = n_vert - D - 1;
    if remaining == 0 {
        return Some(to_face_array(&faces, n_faces));
    }

    // ── Sort remaining points by squared relative distance (descending) ───────
    let mut mean_p = [0.0f64; D];
    for i in (D + 1)..n_vert {
        for j in 0..D {
            mean_p[j] += points[i * S + j];
        }
    }
    for j in 0..D {
        mean_p[j] /= remaining as f64;
    }

    let mut order: Vec<(f64, usize)> = ((D + 1)..n_vert)
        .map(|i| {
            let dist: f64 = (0..D)
                .map(|j| {
                    let v = (points[i * S + j] - mean_p[j]) / span[j];
                    v * v
                })
                .sum();
            (dist, i)
        })
        .collect();
    order.sort_unstable_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut pleft: Vec<usize> = order.into_iter().map(|(_, i)| i).collect();

    // ── Main incremental hull loop ────────────────────────────────────────────
    let mut fucked = false;

    while !pleft.is_empty() {
        let pt = pleft.remove(0);

        let px = points[pt * S];
        let py = points[pt * S + 1];
        let pz = points[pt * S + 2];

        // Which faces are visible from pt?
        let visible_ind: Vec<bool> = (0..n_faces)
            .map(|fi| px * cf[fi * D] + py * cf[fi * D + 1] + pz * cf[fi * D + 2] + df[fi] > 0.0)
            .collect();

        let num_visible = visible_ind.iter().filter(|&&v| v).count();
        if num_visible == 0 {
            continue;
        }
        let num_nonvisible = n_faces - num_visible;

        let visible: Vec<usize> = (0..n_faces).filter(|&i| visible_ind[i]).collect();
        // nonvisible_faces: flat array [num_nonvisible × D] of vertex indices
        let nonvisible_faces: Vec<usize> = (0..n_faces)
            .filter(|&i| !visible_ind[i])
            .flat_map(|i| [faces[i * D], faces[i * D + 1], faces[i * D + 2]])
            .collect();

        // ── Find horizon edges ─────────────────────────────────────────────────
        // A horizon edge is shared by exactly one visible and one nonvisible face.
        let mut horizon: Vec<[usize; 2]> = Vec::new();
        for &vis in &visible {
            let mut face_s = [faces[vis * D], faces[vis * D + 1], faces[vis * D + 2]];
            face_s.sort_unstable();

            for ni in 0..num_nonvisible {
                let nf = [
                    nonvisible_faces[ni * D],
                    nonvisible_faces[ni * D + 1],
                    nonvisible_faces[ni * D + 2],
                ];
                // f0[l] = true iff nf[l] is a vertex of face vis
                let f0 = [
                    face_s.binary_search(&nf[0]).is_ok(),
                    face_s.binary_search(&nf[1]).is_ok(),
                    face_s.binary_search(&nf[2]).is_ok(),
                ];
                if f0.iter().filter(|&&v| v).count() == D - 1 {
                    // Shared edge = the 2 common vertices
                    let mut edge = [0usize; 2];
                    let mut h = 0;
                    for l in 0..D {
                        if f0[l] {
                            edge[h] = nf[l];
                            h += 1;
                        }
                    }
                    horizon.push(edge);
                }
            }
        }
        let horizon_size = horizon.len();

        // ── Delete visible faces ───────────────────────────────────────────────
        let new_n = num_nonvisible;
        let mut new_faces = vec![0usize; new_n * D];
        let mut new_cf = vec![0.0f64; new_n * D];
        let mut new_df = vec![0.0f64; new_n];
        let mut j = 0;
        for i in 0..n_faces {
            if !visible_ind[i] {
                new_faces[j * D..j * D + D].copy_from_slice(&faces[i * D..i * D + D]);
                new_cf[j * D..j * D + D].copy_from_slice(&cf[i * D..i * D + D]);
                new_df[j] = df[i];
                j += 1;
            }
        }
        faces = new_faces;
        cf = new_cf;
        df = new_df;
        n_faces = new_n;

        // Safety: prevent unbounded growth
        if n_faces + horizon_size > MAX_FACES {
            fucked = true;
            break;
        }

        let start = n_faces;

        // ── Add new faces connecting each horizon edge to pt ───────────────────
        for h in 0..horizon_size {
            let new_face = [horizon[h][0], horizon[h][1], pt];
            faces.extend_from_slice(&new_face);
            for j in 0..D {
                for k in 0..D {
                    p_s[j * D + k] = points[new_face[j] * S + k];
                }
            }
            let (cfi, dfi) = plane_3d(&p_s);
            cf.extend_from_slice(&cfi);
            df.push(dfi);
        }
        n_faces += horizon_size;

        // ── Orient each new face outward ───────────────────────────────────────
        for k in start..n_faces {
            let mut face_s = [faces[k * D], faces[k * D + 1], faces[k * D + 2]];
            face_s.sort_unstable();

            // pp: face-indices that are not vertex-indices of face k.
            // Used as a pool of reference point-indices for the determinant test.
            // This mirrors the original C code's hVec-based approach.
            let pp: Vec<usize> = (0..n_faces)
                .filter(|j| face_s.binary_search(j).is_err())
                .collect();

            let mut index = 0;
            let mut det_a = 0.0f64;

            while det_a == 0.0 && index < pp.len() {
                let p_idx = pp[index];
                for i in 0..D {
                    for l in 0..S {
                        a_mat[i * S + l] = points[faces[k * D + i] * S + l];
                    }
                }
                for l in 0..S {
                    a_mat[D * S + l] = points[p_idx * S + l];
                }
                index += 1;
                det_a = det_4x4(&a_mat);
            }

            if det_a < 0.0 {
                faces.swap(k * D + 1, k * D + 2);
                for j in 0..D {
                    cf[k * D + j] = -cf[k * D + j];
                }
                df[k] = -df[k];
            }
        }
    }

    if fucked {
        return None;
    }

    Some(to_face_array(&faces, n_faces))
}

#[inline]
fn to_face_array(faces: &[usize], n_faces: usize) -> Vec<[usize; 3]> {
    (0..n_faces)
        .map(|i| [faces[i * D], faces[i * D + 1], faces[i * D + 2]])
        .collect()
}
