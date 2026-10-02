// 3-D convex hull, incremental, with conflict lists ("outside sets").
//
// Reference: Barber, Dobkin, Huhdanpaa, "The Quickhull Algorithm for Convex
// Hulls", ACM TOMS 22(4), 1996.
//
// Triangulates the measurement directions of a SOFA set for the binaural
// loader. The convhull_3d port next door does the same job for loudspeaker
// layouts, but it tests every point it adds against every face of the hull -
// and, to find the horizon and to orient each new face, every face again - so
// its cost grows with the square of the point count: nothing for a few dozen
// loudspeakers, but over 30 s on a desktop core for a 16 020-direction set,
// and minutes on a set-top box, all of it spent rendering with the built-in
// HRIR set. Here every face keeps the points outside it, and a point is only
// ever tested against the faces it could see: milliseconds for the same set.
//
// The jitter - generator, seed and order - is the port's. It breaks exact ties
// (the four corners of a latitude/longitude cell are coplanar), and the hull of
// a fixed set of jittered points in general position is unique, so this
// returns the same triangles, wound the same way, as `convhull_3d_build`. Only
// their order differs. That is why loudspeaker layouts stay on the port: the
// VBAP table takes the first triangle that holds a direction, and on a shared
// edge the order picks between two equal answers in the last bits, which the
// speaker goldens pin.

use std::collections::HashMap;

const D: usize = 3; // Dimensions
const NOISE_VAL: f64 = 1e-7; // Small noise to avoid degenerate configurations

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

// ── Vector helpers ───────────────────────────────────────────────────────────

#[inline]
fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

#[inline]
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

#[inline]
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// One triangle of the hull under construction.
struct Face {
    /// Vertex indices, counter-clockwise seen from outside.
    v: [usize; 3],
    /// Unit outward normal and plane offset: `n · x - d` is the signed
    /// distance of `x` above the face.
    n: [f64; 3],
    d: f64,
    /// `adj[i]` is the face across the edge `v[i] → v[(i + 1) % 3]`.
    adj: [usize; 3],
    /// Still part of the hull (faces seen by a new point are retired).
    alive: bool,
    /// The points above this face not yet added: its conflict list.
    outside: Vec<usize>,
}

impl Face {
    fn new(v: [usize; 3], p: &[[f64; 3]]) -> Self {
        let mut n = cross(sub(p[v[1]], p[v[0]]), sub(p[v[2]], p[v[0]]));
        let len = dot(n, n).sqrt();
        if len > 0.0 {
            n = [n[0] / len, n[1] / len, n[2] / len];
        }
        Face {
            v,
            n,
            d: dot(n, p[v[0]]),
            adj: [usize::MAX; 3],
            alive: true,
            outside: Vec::new(),
        }
    }

    #[inline]
    fn height(&self, q: [f64; 3]) -> f64 {
        dot(self.n, q) - self.d
    }
}

// ── Main public function ─────────────────────────────────────────────────────

/// Compute the 3-D convex hull of `in_vertices` and return the triangle face
/// indices, or `None` if the triangulation fails (too few points, or a
/// degenerate - flat or collinear - point set).
///
/// Each returned face is `[i0, i1, i2]` where the indices index into
/// `in_vertices`. Face normals are oriented outward.
pub fn quickhull_3d(in_vertices: &[[f64; 3]]) -> Option<Vec<[usize; 3]>> {
    let n_vert = in_vertices.len();
    if n_vert <= D {
        return None;
    }

    let mut rng = Xorshift32(12345);
    let p: Vec<[f64; 3]> = in_vertices
        .iter()
        .map(|q| {
            [
                q[0] + NOISE_VAL * rng.next(),
                q[1] + NOISE_VAL * rng.next(),
                q[2] + NOISE_VAL * rng.next(),
            ]
        })
        .collect();

    // ── Span check: a set that is flat along an axis has no 3-D hull ────────
    for j in 0..D {
        let (lo, hi) = p
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), q| {
                (lo.min(q[j]), hi.max(q[j]))
            });
        if hi - lo <= 1e-7 {
            return None;
        }
    }
    let scale = p.iter().fold(0.0f64, |m, q| {
        m.max(q[0].abs()).max(q[1].abs()).max(q[2].abs())
    });
    // A point counts as above a face from this height on. Rounding in a plane
    // test is ~1e-16 of the coordinates; the jitter separates tied points by
    // ~1e-9 and more.
    let eps = 1e-13 * scale;

    // ── Initial tetrahedron from extreme points ──────────────────────────────
    let sq = |a: [f64; 3]| dot(a, a);
    let i0 = (0..n_vert).min_by(|&a, &b| p[a][0].total_cmp(&p[b][0]))?;
    let i1 = (0..n_vert).max_by(|&a, &b| sq(sub(p[a], p[i0])).total_cmp(&sq(sub(p[b], p[i0]))))?;
    let e01 = sub(p[i1], p[i0]);
    let i2 = (0..n_vert).max_by(|&a, &b| {
        sq(cross(e01, sub(p[a], p[i0]))).total_cmp(&sq(cross(e01, sub(p[b], p[i0]))))
    })?;
    let base = cross(e01, sub(p[i2], p[i0]));
    let base_len = sq(base).sqrt();
    if base_len <= 1e-12 * scale * scale {
        return None;
    }
    let base = [base[0] / base_len, base[1] / base_len, base[2] / base_len];
    let i3 = (0..n_vert).max_by(|&a, &b| {
        dot(base, sub(p[a], p[i0]))
            .abs()
            .total_cmp(&dot(base, sub(p[b], p[i0])).abs())
    })?;
    if dot(base, sub(p[i3], p[i0])).abs() <= eps {
        return None;
    }

    let mut faces: Vec<Face> = Vec::with_capacity(8 * n_vert);
    let tet = [i0, i1, i2, i3];
    for &apex in &tet {
        let mut v = [0usize; 3];
        let mut k = 0;
        for &t in &tet {
            if t != apex {
                v[k] = t;
                k += 1;
            }
        }
        // Wind the face so the vertex it leaves out is below it.
        if Face::new(v, &p).height(p[apex]) > 0.0 {
            v.swap(1, 2);
        }
        faces.push(Face::new(v, &p));
    }
    let mut edge_face: HashMap<(usize, usize), usize> = HashMap::with_capacity(12);
    for (fi, f) in faces.iter().enumerate() {
        for i in 0..3 {
            edge_face.insert((f.v[i], f.v[(i + 1) % 3]), fi);
        }
    }
    for f in faces.iter_mut() {
        for i in 0..3 {
            let (a, b) = (f.v[i], f.v[(i + 1) % 3]);
            f.adj[i] = *edge_face.get(&(b, a))?;
        }
    }

    // Every other point goes on the conflict list of one face it is above;
    // a point above none is inside the tetrahedron and never on the hull.
    for (q, &pq) in p.iter().enumerate() {
        if tet.contains(&q) {
            continue;
        }
        if let Some(f) = faces.iter_mut().find(|f| f.height(pq) > eps) {
            f.outside.push(q);
        }
    }

    // ── Main loop ────────────────────────────────────────────────────────────
    let mut pending: Vec<usize> = (0..faces.len())
        .filter(|&f| !faces[f].outside.is_empty())
        .collect();
    // Per-face scratch for the visibility search, reset by bumping `epoch`.
    let mut seen_in: Vec<u32> = vec![0; faces.capacity()];
    let mut seen_visible: Vec<bool> = vec![false; faces.capacity()];
    let mut epoch = 0u32;
    // Per-vertex scratch for linking the new faces around the horizon.
    let mut new_from: Vec<usize> = vec![usize::MAX; n_vert];
    let mut new_to: Vec<usize> = vec![usize::MAX; n_vert];
    let mut visible: Vec<usize> = Vec::new();
    let mut horizon: Vec<(usize, usize, usize)> = Vec::new();
    let mut stack: Vec<usize> = Vec::new();
    let mut orphans: Vec<usize> = Vec::new();

    while let Some(f0) = pending.pop() {
        if !faces[f0].alive || faces[f0].outside.is_empty() {
            continue;
        }

        // The farthest point above this face goes on the hull next.
        let (k, _) = faces[f0]
            .outside
            .iter()
            .enumerate()
            .map(|(k, &q)| (k, faces[f0].height(p[q])))
            .max_by(|a, b| a.1.total_cmp(&b.1))?;
        let apex = faces[f0].outside.swap_remove(k);
        let pa = p[apex];

        // Every face the apex sees forms one connected region; walk it from
        // f0 and keep its boundary - the horizon - as directed edges, each
        // with the face beyond it.
        epoch = epoch.wrapping_add(1);
        if seen_in.len() < faces.len() {
            seen_in.resize(2 * faces.len(), 0);
            seen_visible.resize(2 * faces.len(), false);
        }
        visible.clear();
        horizon.clear();
        stack.clear();
        seen_in[f0] = epoch;
        seen_visible[f0] = true;
        stack.push(f0);
        while let Some(g) = stack.pop() {
            visible.push(g);
            for i in 0..3 {
                let h = faces[g].adj[i];
                if seen_in[h] != epoch {
                    seen_in[h] = epoch;
                    seen_visible[h] = faces[h].height(pa) > eps;
                    if seen_visible[h] {
                        stack.push(h);
                    }
                }
                if !seen_visible[h] {
                    horizon.push((faces[g].v[i], faces[g].v[(i + 1) % 3], h));
                }
            }
        }

        // Cone the horizon to the apex: one new face per horizon edge, wound
        // like the face it replaces, so it faces outward too.
        let first = faces.len();
        for (k, &(a, b, beyond)) in horizon.iter().enumerate() {
            let nf = first + k;
            let mut f = Face::new([a, b, apex], &p);
            f.adj[0] = beyond;
            let j =
                (0..3).find(|&j| faces[beyond].v[j] == b && faces[beyond].v[(j + 1) % 3] == a)?;
            faces[beyond].adj[j] = nf;
            // A horizon is one simple loop; anything else is a numerical
            // failure, and an honest None beats a broken mesh.
            if new_from[a] != usize::MAX || new_to[b] != usize::MAX {
                return None;
            }
            new_from[a] = nf;
            new_to[b] = nf;
            faces.push(f);
        }
        for (k, &(a, b, _)) in horizon.iter().enumerate() {
            let nf = first + k;
            // Across (b, apex) is the new face starting at b; across
            // (apex, a), the one ending at a.
            faces[nf].adj[1] = new_from[b];
            faces[nf].adj[2] = new_to[a];
            if faces[nf].adj[1] == usize::MAX || faces[nf].adj[2] == usize::MAX {
                return None;
            }
        }
        for &(a, b, _) in &horizon {
            new_from[a] = usize::MAX;
            new_to[b] = usize::MAX;
        }

        // The retired faces' points move to the new faces they are above;
        // a point above none of them is now inside the hull.
        orphans.clear();
        for &g in &visible {
            faces[g].alive = false;
            orphans.append(&mut faces[g].outside);
        }
        for &q in &orphans {
            if let Some(nf) = (first..faces.len()).find(|&nf| faces[nf].height(p[q]) > eps) {
                faces[nf].outside.push(q);
            }
        }
        pending.extend((first..faces.len()).filter(|&nf| !faces[nf].outside.is_empty()));
    }

    Some(faces.iter().filter(|f| f.alive).map(|f| f.v).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Directions on a latitude/longitude grid, poles once: every cell's four
    /// corners are coplanar, the hardest case for a hull.
    fn sphere_grid(step_deg: i32) -> Vec<[f64; 3]> {
        let mut pts = vec![[0.0, 0.0, 1.0], [0.0, 0.0, -1.0]];
        let mut el = -90 + step_deg;
        while el < 90 {
            let mut az = 0;
            while az < 360 {
                let (e, a) = ((el as f64).to_radians(), (az as f64).to_radians());
                pts.push([e.cos() * a.cos(), e.cos() * a.sin(), e.sin()]);
                az += step_deg;
            }
            el += step_deg;
        }
        pts
    }

    /// A closed, consistently wound, outward, convex triangulation of every
    /// point (all of them lie on the sphere, so all are hull vertices).
    fn assert_sphere_hull(pts: &[[f64; 3]], faces: &[[usize; 3]]) {
        let mut used = vec![false; pts.len()];
        let mut directed: HashMap<(usize, usize), u32> = HashMap::new();
        for f in faces {
            for i in 0..3 {
                used[f[i]] = true;
                *directed.entry((f[i], f[(i + 1) % 3])).or_default() += 1;
            }
        }
        assert!(used.iter().all(|&u| u), "every direction is a hull vertex");
        assert_eq!(faces.len(), 2 * pts.len() - 4, "Euler: F = 2V - 4");
        for (&(a, b), &count) in &directed {
            assert_eq!(count, 1, "edge {a}->{b} used once per direction");
            assert_eq!(
                directed.get(&(b, a)),
                Some(&1),
                "edge {a}-{b} closed by its twin"
            );
        }
        for f in faces {
            let (a, b, c) = (pts[f[0]], pts[f[1]], pts[f[2]]);
            let n = cross(sub(b, a), sub(c, a));
            let len = dot(n, n).sqrt();
            let n = [n[0] / len, n[1] / len, n[2] / len];
            let centroid = [
                (a[0] + b[0] + c[0]) / 3.0,
                (a[1] + b[1] + c[1]) / 3.0,
                (a[2] + b[2] + c[2]) / 3.0,
            ];
            assert!(dot(n, centroid) > 0.0, "face {f:?} points outward");
            let d = dot(n, a);
            for q in pts {
                // Coplanar cells and the jitter allow a hair above the plane.
                assert!(dot(n, *q) - d < 1e-6, "face {f:?} has a point above it");
            }
        }
    }

    #[test]
    fn a_latitude_longitude_grid_keeps_every_direction() {
        let pts = sphere_grid(5);
        let faces = quickhull_3d(&pts).expect("hull");
        assert_sphere_hull(&pts, &faces);
    }

    #[test]
    fn a_set_beyond_the_port_face_cap_is_triangulated() {
        // 30 000 directions on a Fibonacci sphere: 59 996 faces, past the
        // 50 000-face cap the convhull_3d port gives up at.
        let n = 30_000;
        let golden = std::f64::consts::PI * (3.0 - 5f64.sqrt());
        let pts: Vec<[f64; 3]> = (0..n)
            .map(|i| {
                let z = 1.0 - 2.0 * (i as f64 + 0.5) / n as f64;
                let r = (1.0 - z * z).sqrt();
                let t = golden * i as f64;
                [r * t.cos(), r * t.sin(), z]
            })
            .collect();
        let faces = quickhull_3d(&pts).expect("hull");
        assert_eq!(faces.len(), 2 * n - 4);
    }

    #[test]
    fn same_triangles_and_winding_as_the_port() {
        let canon = |faces: Vec<[usize; 3]>| -> std::collections::BTreeSet<[usize; 3]> {
            faces
                .into_iter()
                .map(|f| {
                    let k = (0..3).min_by_key(|&i| f[i]).unwrap();
                    [f[k], f[(k + 1) % 3], f[(k + 2) % 3]]
                })
                .collect()
        };
        let mut speakers: Vec<[f64; 3]> = [
            (30.0, 0.0),
            (-30.0, 0.0),
            (0.0, 0.0),
            (110.0, 0.0),
            (-110.0, 0.0),
            (45.0, 45.0),
            (-45.0, 45.0),
            (135.0, 45.0),
            (-135.0, 45.0),
            (0.0, -90.0),
        ]
        .iter()
        .map(|&(az, el): &(f64, f64)| {
            let (a, e) = (az.to_radians(), el.to_radians());
            [e.cos() * a.cos(), e.cos() * a.sin(), e.sin()]
        })
        .collect();
        for pts in [
            sphere_grid(10),
            sphere_grid(5),
            std::mem::take(&mut speakers),
        ] {
            let port = crate::spatial_vbap::convhull::convhull_3d_build(&pts).expect("port");
            let ours = quickhull_3d(&pts).expect("hull");
            assert_eq!(canon(ours), canon(port), "{} points", pts.len());
        }
    }

    #[test]
    fn points_inside_are_left_out() {
        // A cube's corners, plus points strictly inside it.
        let mut pts = Vec::new();
        for &x in &[-1.0, 1.0] {
            for &y in &[-1.0, 1.0] {
                for &z in &[-1.0, 1.0] {
                    pts.push([x, y, z]);
                }
            }
        }
        pts.extend([[0.0, 0.0, 0.0], [0.5, -0.3, 0.2], [-0.9, 0.9, -0.9]]);
        let faces = quickhull_3d(&pts).expect("hull");
        assert_eq!(faces.len(), 12, "a cube has 6 square sides, 12 triangles");
        assert!(
            faces.iter().flatten().all(|&i| i < 8),
            "no interior point is a vertex"
        );
    }

    #[test]
    fn flat_or_tiny_sets_have_no_hull() {
        assert!(quickhull_3d(&[[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]).is_none());
        // A ring on the horizon: flat, as the port also reports.
        let ring: Vec<[f64; 3]> = (0..8)
            .map(|i| {
                let a = (i as f64 * 45.0).to_radians();
                [a.cos(), a.sin(), 0.0]
            })
            .collect();
        assert!(quickhull_3d(&ring).is_none());
    }
}
