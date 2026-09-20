//! Emit the golden vectors the Studio frontend's JS mirror is asserted against.
//!
//! The Studio keeps its own JS copy of this math because per-frame Three.js
//! work has to stay in the browser (see the rebalancing plan's Phase 2.2
//! decision: mirrored JS validated by golden vectors, not WASM). A mirror only
//! stays honest if something checks it, so this dumps the Rust answers and
//! `omniphony-studio/scripts/test-math.mjs` replays them through
//! `src/coordinates.js`.
//!
//! Regenerate after any change to the geometry:
//!
//! ```text
//! cargo run -p omniphony-geometry --example dump_golden_vectors \
//!   > ../omniphony-studio/scripts/golden/geometry.json
//! ```
//!
//! f64 is used throughout: JS numbers are f64, so the JS mirror can only ever
//! match that precision.
//!
//! Written by hand rather than with serde — this crate is deliberately
//! dependency-free, and the shape is small enough not to justify breaking that.

use omniphony_geometry::f64 as geometry;

/// Room configurations the vectors sweep. `(width, front, height, rear, lower,
/// center_blend)`, matching the Studio's `app.roomRatio`.
const ROOMS: &[(f64, f64, f64, f64, f64, f64)] = &[
    // Studio defaults.
    (1.0, 2.0, 1.0, 1.0, 0.5, 0.5),
    // Unit room: every ratio 1, warp degenerates to identity.
    (1.0, 1.0, 1.0, 1.0, 1.0, 0.5),
    // Deep front, shallow rear, blend pinned to the rear ratio.
    (1.5, 3.0, 1.2, 0.8, 0.6, 0.0),
    // Reversed, blend pinned to the front ratio.
    (0.8, 1.2, 0.9, 2.4, 0.4, 1.0),
];

/// ADM positions covering cardinals, the vertical axis, corners and interior.
const POSITIONS: &[[f64; 3]] = &[
    [0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0],
    [1.0, 0.0, 0.0],
    [-1.0, 0.0, 0.0],
    [0.0, -1.0, 0.0],
    [0.0, 0.0, 1.0],
    [0.0, 0.0, -1.0],
    [1.0, 1.0, 0.0],
    [-1.0, 1.0, 0.0],
    [1.0, -1.0, 1.0],
    [-1.0, -1.0, -1.0],
    [0.3, 0.7, -0.25],
    [-0.45, -0.2, 0.9],
    [0.6, 0.6, 0.6],
    // Just off the vertical axis, inside the 1e-6 pole guard. Without the same
    // guard the JS mirror lands ~6e-8 degrees off a clean +90, which is what
    // makes this vector worth carrying.
    [1e-9, 0.0, 1.0],
];

const ANGLES: &[(f64, f64, f64)] = &[
    (0.0, 0.0, 1.0),
    (45.0, 0.0, 1.0),
    (-45.0, 0.0, 1.0),
    (90.0, 0.0, 1.0),
    (-90.0, 0.0, 1.0),
    (180.0, 0.0, 1.0),
    (30.0, 45.0, 0.8),
    (-120.0, -30.0, 1.0),
    (0.0, 90.0, 1.0),
    (0.0, -90.0, 1.0),
    (135.0, 15.0, 0.5),
];

/// Print an f64 with enough digits to round-trip exactly through JSON.
fn num(value: f64) -> String {
    if value == value.trunc() && value.abs() < 1e15 {
        format!("{value:.1}")
    } else {
        format!("{value:?}")
    }
}

fn triple(v: [f64; 3]) -> String {
    format!("[{}, {}, {}]", num(v[0]), num(v[1]), num(v[2]))
}

fn main() {
    let mut out = String::new();
    out.push_str("{\n");
    out.push_str(
        "  \"_comment\": \"GENERATED — do not edit. \
         cargo run -p omniphony-geometry --example dump_golden_vectors > this file. \
         Asserted against src/coordinates.js by scripts/test-math.mjs.\",\n",
    );

    // to_spherical: ADM cartesian -> angles.
    out.push_str("  \"toSpherical\": [\n");
    let mut rows: Vec<String> = Vec::new();
    for &p in POSITIONS {
        let (az, el, dist) = geometry::to_spherical(p[0], p[1], p[2]);
        rows.push(format!(
            "    {{ \"adm\": {}, \"az\": {}, \"el\": {}, \"dist\": {} }}",
            triple(p),
            num(az),
            num(el),
            num(dist)
        ));
    }
    out.push_str(&rows.join(",\n"));
    out.push_str("\n  ],\n");

    // from_spherical: angles -> ADM cartesian.
    out.push_str("  \"fromSpherical\": [\n");
    rows.clear();
    for &(az, el, dist) in ANGLES {
        let (x, y, z) = geometry::from_spherical(az, el, dist);
        rows.push(format!(
            "    {{ \"az\": {}, \"el\": {}, \"dist\": {}, \"adm\": {} }}",
            num(az),
            num(el),
            num(dist),
            triple([x, y, z])
        ));
    }
    out.push_str(&rows.join(",\n"));
    out.push_str("\n  ],\n");

    // map_depth and its inverse, per room.
    out.push_str("  \"mapDepth\": [\n");
    rows.clear();
    for &(_, front, _, rear, _, blend) in ROOMS {
        for step in 0..=16 {
            let depth = -1.0 + 2.0 * (step as f64) / 16.0;
            let mapped = geometry::map_depth(depth, front, rear, blend);
            rows.push(format!(
                "    {{ \"depth\": {}, \"front\": {}, \"rear\": {}, \"blend\": {}, \
                 \"mapped\": {}, \"inverse\": {} }}",
                num(depth),
                num(front),
                num(rear),
                num(blend),
                num(mapped),
                num(geometry::inverse_map_depth(mapped, front, rear, blend))
            ));
        }
    }
    out.push_str(&rows.join(",\n"));
    out.push_str("\n  ],\n");

    // Angle helpers.
    out.push_str("  \"normalizeDeg\": [\n");
    rows.clear();
    for &a in &[
        -540.0, -359.0, -181.0, -180.0, -90.0, 0.0, 90.0, 180.0, 181.0, 359.0, 540.0,
    ] {
        rows.push(format!(
            "    {{ \"in\": {}, \"out\": {} }}",
            num(a),
            num(geometry::normalize_deg(a))
        ));
    }
    out.push_str(&rows.join(",\n"));
    out.push_str("\n  ],\n");

    out.push_str("  \"snapDeg\": [\n");
    rows.clear();
    for &(angle, step, threshold) in &[
        (44.0, 45.0, 2.0),
        (41.0, 45.0, 2.0),
        (-46.5, 45.0, 2.0),
        (0.5, 15.0, 1.0),
        (7.0, 15.0, 1.0),
        (179.0, 90.0, 2.0),
    ] {
        rows.push(format!(
            "    {{ \"angle\": {}, \"step\": {}, \"threshold\": {}, \"out\": {} }}",
            num(angle),
            num(step),
            num(threshold),
            num(geometry::snap_deg(angle, step, threshold))
        ));
    }
    out.push_str(&rows.join(",\n"));
    out.push_str("\n  ],\n");

    // Room scaling, both directions, per room.
    out.push_str("  \"roomScaledPosition\": [\n");
    rows.clear();
    for &(width, front, height, rear, lower, blend) in ROOMS {
        let ratio = [width, front, height];
        for &p in POSITIONS {
            let scaled = geometry::room_scaled_position(p, ratio, rear, lower, blend);
            let back = geometry::inverse_room_scaled_position(scaled, ratio, rear, lower, blend);
            rows.push(format!(
                "    {{ \"adm\": {}, \"ratio\": {}, \"rear\": {}, \"lower\": {}, \
                 \"blend\": {}, \"scaled\": {}, \"inverse\": {} }}",
                triple(p),
                triple(ratio),
                num(rear),
                num(lower),
                num(blend),
                triple(scaled),
                triple(back)
            ));
        }
    }
    out.push_str(&rows.join(",\n"));
    out.push_str("\n  ]\n}\n");

    print!("{out}");
}
