//! Canonical coordinate, angle and room-geometry math for Omniphony.
//!
//! Before this crate the same handful of formulas existed in five places: four
//! copies of the room depth warp inside the renderer workspace
//! (`renderer::speaker_layout`, `renderer::render_backend::room_transform`,
//! `renderer::render_backend::file_loaded_evaluator`,
//! `orender_engine::virtual_bed`), plus one in the Studio frontend — and two
//! independent cartesian/spherical pairs that did **not** agree (see
//! "Frames" below).
//!
//! # Frames
//!
//! **ADM space** is the wire, file and renderer frame. It is what a layout
//! YAML stores and what OSC carries:
//!
//! ```text
//! x: left(-1) -> right(+1)
//! y: back(-1) -> front(+1)
//! z: floor(-1) -> ceiling(+1)
//! ```
//!
//! Angles are degrees: azimuth `0` = front (`+y`), `+90` = right (`+x`);
//! elevation `0` = ear level, `+90` = straight up.
//!
//! **Scene space** is the Three.js frame the Studio frontend draws in. It is
//! the same room with the axes relabelled:
//!
//! ```text
//! scene = (adm.y, adm.z, adm.x)   // x = depth/front, y = up, z = right
//! ```
//!
//! The two frames describe *the same angles*, because the swizzle lines the
//! components up again:
//!
//! ```text
//! scene azimuth = atan2(scene.z, scene.x) = atan2(adm.x, adm.y) = adm azimuth
//! ```
//!
//! That equivalence is the trap this crate exists to close. Applying the
//! scene-ordered formula to ADM-ordered components — `atan2(z, x)` on ADM data,
//! without the swizzle — compiles, runs, and yields a *different frame*: it
//! reads a hard-right speaker as dead ahead and 45° up. The Studio backend did
//! exactly that on every layout it parsed. Convert frames explicitly with
//! [`adm_to_scene`](f64::adm_to_scene) / [`scene_to_adm`](f64::scene_to_adm);
//! never reorder components by hand.
//!
//! # Precision
//!
//! Every function exists in both [`f32`] (what the renderer's audio path uses)
//! and [`f64`] (what the Studio backend uses). The two are generated from one
//! macro so they cannot drift; the arithmetic is written to match the existing
//! renderer implementations term for term, so adopting this crate there is a
//! bit-for-bit no-op.
//!
//! # Hot paths
//!
//! Everything here is allocation-free and branch-light: these run per object
//! per frame in the renderer, and the embedded target has no cycles to spare.

#![forbid(unsafe_code)]

/// Smallest distance a speaker or object may sit at, in metres.
///
/// A zero-distance position has no defined direction, so the angle pair would
/// be arbitrary; clamping keeps every stored coordinate invertible.
pub const MIN_DISTANCE: f64 = 0.01;

/// Floor applied to room ratios before dividing by them.
pub const MIN_ROOM_RATIO: f64 = 0.01;

macro_rules! geometry_for {
    ($module:ident, $f:ty) => {
        /// Geometry in `
        #[doc = stringify!($f)]
        /// ` precision. See the crate docs for conventions.
        pub mod $module {
            /// Smallest representable distance, in metres.
            pub const MIN_DISTANCE: $f = super::MIN_DISTANCE as $f;
            /// Floor applied to room ratios before dividing by them.
            pub const MIN_ROOM_RATIO: $f = super::MIN_ROOM_RATIO as $f;

            // ---------------------------------------------------------------
            // Frames
            // ---------------------------------------------------------------

            /// ADM `(x, y, z)` -> Three.js scene `(depth, up, right)`.
            #[inline]
            pub fn adm_to_scene(position: [$f; 3]) -> [$f; 3] {
                [position[1], position[2], position[0]]
            }

            /// Three.js scene `(depth, up, right)` -> ADM `(x, y, z)`.
            #[inline]
            pub fn scene_to_adm(position: [$f; 3]) -> [$f; 3] {
                [position[2], position[0], position[1]]
            }

            // ---------------------------------------------------------------
            // Cartesian <-> spherical (ADM frame, degrees)
            // ---------------------------------------------------------------

            /// ADM cartesian -> `(azimuth_deg, elevation_deg, distance)`.
            ///
            /// Exact vertical directions are kept stable: straight up reads
            /// `+90`, straight down `-90`, and the origin `0` rather than
            /// whatever `atan2(0.0, 0.0)` happens to give.
            #[inline]
            pub fn to_spherical(x: $f, y: $f, z: $f) -> ($f, $f, $f) {
                let distance = (x * x + y * y + z * z).sqrt();
                let horizontal = (x * x + y * y).sqrt();

                let azimuth_deg = x.atan2(y).to_degrees();
                let elevation_deg = if horizontal < 1e-6 {
                    if z > 0.0 {
                        90.0
                    } else if z < 0.0 {
                        -90.0
                    } else {
                        0.0
                    }
                } else {
                    z.atan2(horizontal).to_degrees()
                };

                (azimuth_deg, elevation_deg, distance)
            }

            /// `(azimuth_deg, elevation_deg, distance)` -> ADM cartesian.
            #[inline]
            pub fn from_spherical(
                azimuth_deg: $f,
                elevation_deg: $f,
                distance: $f,
            ) -> ($f, $f, $f) {
                let az = azimuth_deg.to_radians();
                let el = elevation_deg.to_radians();
                let horizontal = distance * el.cos();
                (
                    horizontal * az.sin(),
                    horizontal * az.cos(),
                    distance * el.sin(),
                )
            }

            // ---------------------------------------------------------------
            // Angles
            // ---------------------------------------------------------------

            /// Wrap to `[-180, 180]` by repeated addition — matches
            /// `orender_engine::spatial::normalize_azimuth_deg`.
            #[inline]
            pub fn normalize_deg(angle: $f) -> $f {
                let mut a = angle;
                while a < -180.0 {
                    a += 360.0;
                }
                while a > 180.0 {
                    a -= 360.0;
                }
                a
            }

            /// Wrap to `(-180, 180]`, mapping the `-180` seam onto `+180` so a
            /// lookup table has one entry per direction, not two.
            #[inline]
            pub fn wrap_deg(value: $f) -> $f {
                let wrapped = (value + 180.0).rem_euclid(360.0) - 180.0;
                if wrapped == -180.0 { 180.0 } else { wrapped }
            }

            /// Shortest angular separation, in `[0, 180]`.
            #[inline]
            pub fn wrapped_distance_deg(a: $f, b: $f) -> $f {
                let delta = (a - b).rem_euclid(360.0);
                delta.min(360.0 - delta)
            }

            /// Snap to the nearest multiple of `step`, but only when already
            /// within `threshold` of it. Outside that window the angle is
            /// returned untouched, so snapping never fights a deliberate edit.
            #[inline]
            pub fn snap_deg(angle: $f, step: $f, threshold: $f) -> $f {
                if step <= 0.0 {
                    return angle;
                }
                let snapped = (angle / step).round() * step;
                if (angle - snapped).abs() <= threshold {
                    snapped
                } else {
                    angle
                }
            }

            // ---------------------------------------------------------------
            // Room geometry
            // ---------------------------------------------------------------

            /// Non-linear depth warp on the front/back axis.
            ///
            /// A cubic through `(0, 0)` and `(±1, ±ratio)` whose slope at the
            /// origin is `center_ratio`, so the room can be stretched towards
            /// the front and the rear by different amounts while the listener
            /// position stays put and the curve stays smooth across it.
            /// `center_blend` picks where that origin slope sits between the
            /// rear and front ratios.
            #[inline]
            pub fn map_depth(depth: $f, front_ratio: $f, rear_ratio: $f, center_blend: $f) -> $f {
                let d = depth.clamp(-1.0, 1.0);
                let blend = center_blend.clamp(0.0, 1.0);
                let center_ratio = rear_ratio + (front_ratio - rear_ratio) * blend;
                if d >= 0.0 {
                    let t = d;
                    let a = center_ratio - front_ratio;
                    let b = 2.0 * (front_ratio - center_ratio);
                    a * t * t * t + b * t * t + center_ratio * t
                } else {
                    let t = -d;
                    let a = center_ratio - rear_ratio;
                    let b = 2.0 * (rear_ratio - center_ratio);
                    -(a * t * t * t + b * t * t + center_ratio * t)
                }
            }

            /// Invert [`map_depth`] by bisection.
            ///
            /// 28 halvings of a unit interval, so the result is good to about
            /// `2^-29`. The warp is a monotone cubic with no closed-form
            /// inverse worth the branch count here; this matches the two
            /// existing implementations (`orender_engine::virtual_bed` and the
            /// Studio frontend) iteration for iteration, which is what lets
            /// them be replaced without moving a single stored coordinate.
            pub fn inverse_map_depth(
                mapped_depth: $f,
                front_ratio: $f,
                rear_ratio: $f,
                center_blend: $f,
            ) -> $f {
                let (target, mut lo, mut hi) = if mapped_depth >= 0.0 {
                    (
                        mapped_depth.clamp(0.0, front_ratio.max(0.0)),
                        0.0 as $f,
                        1.0 as $f,
                    )
                } else {
                    (
                        mapped_depth.clamp(-rear_ratio.max(0.0), 0.0),
                        -1.0 as $f,
                        0.0 as $f,
                    )
                };
                for _ in 0..28 {
                    let mid = (lo + hi) * 0.5;
                    if map_depth(mid, front_ratio, rear_ratio, center_blend) < target {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                (lo + hi) * 0.5
            }

            /// Scale a normalised ADM position into room-relative space: the
            /// coordinates gain models pan in, and that distance attenuation
            /// measures from.
            ///
            /// `room_ratio` is `[width, front, height]`; the rear and lower
            /// ratios cover the half-axes that are allowed to differ.
            #[inline]
            pub fn room_scaled_position(
                position: [$f; 3],
                room_ratio: [$f; 3],
                rear_ratio: $f,
                lower_ratio: $f,
                center_blend: $f,
            ) -> [$f; 3] {
                [
                    position[0] * room_ratio[0],
                    map_depth(position[1], room_ratio[1], rear_ratio, center_blend),
                    if position[2] >= 0.0 {
                        position[2] * room_ratio[2]
                    } else {
                        position[2] * lower_ratio
                    },
                ]
            }

            /// Inverse of [`room_scaled_position`], clamped back into the
            /// normalised `[-1, 1]` cube.
            #[inline]
            pub fn inverse_room_scaled_position(
                position: [$f; 3],
                room_ratio: [$f; 3],
                rear_ratio: $f,
                lower_ratio: $f,
                center_blend: $f,
            ) -> [$f; 3] {
                let width = room_ratio[0].max(MIN_ROOM_RATIO);
                let front = room_ratio[1].max(MIN_ROOM_RATIO);
                let height = room_ratio[2].max(MIN_ROOM_RATIO);
                let rear = rear_ratio.max(MIN_ROOM_RATIO);
                let lower = lower_ratio.max(MIN_ROOM_RATIO);

                [
                    (position[0] / width).clamp(-1.0, 1.0),
                    inverse_map_depth(position[1], front, rear, center_blend).clamp(-1.0, 1.0),
                    if position[2] >= 0.0 {
                        (position[2] / height).clamp(-1.0, 1.0)
                    } else {
                        (position[2] / lower).clamp(-1.0, 1.0)
                    },
                ]
            }

            // ---------------------------------------------------------------
            // Sampling grids
            // ---------------------------------------------------------------

            /// `count` positions spread evenly from `min` to `max`, inclusive.
            ///
            /// A count of one or zero collapses to `min` rather than dividing
            /// by zero.
            pub fn evenly_spaced_axis(count: usize, min: $f, max: $f) -> Vec<$f> {
                if count <= 1 {
                    return vec![min];
                }
                let step = (max - min) / (count.saturating_sub(1) as $f);
                (0..count).map(|index| min + step * index as $f).collect()
            }

            /// Node positions of the cartesian gain table's height axis.
            ///
            /// Not symmetric, which is the whole reason this is a function and
            /// not a step size. The positive half is a plain even spread over
            /// `[0, 1]`, but the space below the listener is optional and gets
            /// its own resolution: `z_neg_size` nodes covering `[-1, 0)`,
            /// stopping short of zero so the two halves do not both land on it.
            ///
            /// Both counts are **node counts**, not intervals. The live
            /// parameter is an interval count and is converted before it gets
            /// here (`live_params.rs`: `z_size.max(1) + 1`) — a distinction
            /// that has to survive every hop of the protocol, and the reason
            /// the Studio asks for these positions instead of rebuilding them.
            pub fn cartesian_z_axis(z_size: usize, z_neg_size: usize) -> Vec<$f> {
                let mut values = Vec::with_capacity(z_neg_size + z_size);
                if z_neg_size > 0 {
                    for index in 0..z_neg_size {
                        values.push(-1.0 + index as $f / z_neg_size as $f);
                    }
                }
                values.extend(evenly_spaced_axis(z_size.max(2), 0.0, 1.0));
                values
            }

            // ---------------------------------------------------------------
            // Coordinate hydration
            // ---------------------------------------------------------------

            /// Derive the polar representation of a speaker given its
            /// cartesian one, clamping the inputs to the normalised cube and
            /// the distance to [`MIN_DISTANCE`].
            #[inline]
            pub fn hydrate_from_cartesian(x: $f, y: $f, z: $f) -> ($f, $f, $f) {
                let x = x.clamp(-1.0, 1.0);
                let y = y.clamp(-1.0, 1.0);
                let z = z.clamp(-1.0, 1.0);
                let (az, el, dist) = to_spherical(x, y, z);
                (az, el, dist.max(MIN_DISTANCE))
            }

            /// Derive the cartesian representation of a speaker given its
            /// polar one, clamping the result to the normalised cube.
            #[inline]
            pub fn hydrate_from_spherical(
                azimuth_deg: $f,
                elevation_deg: $f,
                distance: $f,
            ) -> ($f, $f, $f) {
                let (x, y, z) =
                    from_spherical(azimuth_deg, elevation_deg, distance.max(MIN_DISTANCE));
                (x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0), z.clamp(-1.0, 1.0))
            }
        }
    };
}

geometry_for!(f32, ::core::primitive::f32);
geometry_for!(f64, ::core::primitive::f64);

#[cfg(test)]
mod tests {
    use super::f64::*;

    const EPS: f64 = 1e-9;

    fn close(a: f64, b: f64) {
        assert!((a - b).abs() < 1e-6, "expected {b}, got {a}");
    }

    // ── Frames ──────────────────────────────────────────────────────────────

    #[test]
    fn adm_cardinal_directions_read_as_expected() {
        // Front centre.
        let (az, el, _) = to_spherical(0.0, 1.0, 0.0);
        close(az, 0.0);
        close(el, 0.0);

        // Hard right.
        let (az, el, _) = to_spherical(1.0, 0.0, 0.0);
        close(az, 90.0);
        close(el, 0.0);

        // Hard left.
        let (az, _, _) = to_spherical(-1.0, 0.0, 0.0);
        close(az, -90.0);

        // Directly overhead: azimuth is arbitrary, elevation must be exact.
        let (_, el, _) = to_spherical(0.0, 0.0, 1.0);
        close(el, 90.0);
    }

    /// The regression this crate was written for: the Studio backend used the
    /// scene-space formula on ADM-ordered components. `FR` from `7.1.4.yaml`
    /// (x=1, y=1, z=0) is front-right at ear level, and must not read as
    /// straight-ahead-and-elevated.
    #[test]
    fn front_right_speaker_is_not_read_as_elevated_centre() {
        let (az, el, _) = to_spherical(1.0, 1.0, 0.0);
        close(az, 45.0);
        close(el, 0.0);

        // What the un-swizzled formula produced, for the record.
        let wrong_az = 0.0f64.atan2(1.0).to_degrees();
        let wrong_el = 1.0f64.atan2((1.0f64 + 0.0).sqrt()).to_degrees();
        close(wrong_az, 0.0);
        close(wrong_el, 45.0);
    }

    #[test]
    fn scene_swizzle_preserves_angles() {
        // Applying the scene-ordered azimuth formula to swizzled components
        // must agree with the ADM formula on the originals.
        for &(x, y, z) in &[
            (1.0, 1.0, 0.0),
            (-0.3, 0.8, 0.5),
            (0.0, -1.0, -0.25),
            (0.7, 0.0, 1.0),
        ] {
            let scene = adm_to_scene([x, y, z]);
            let scene_az = scene[2].atan2(scene[0]).to_degrees();
            let (adm_az, _, _) = to_spherical(x, y, z);
            close(scene_az, adm_az);

            let scene_el = scene[1]
                .atan2((scene[0] * scene[0] + scene[2] * scene[2]).sqrt())
                .to_degrees();
            let (_, adm_el, _) = to_spherical(x, y, z);
            close(scene_el, adm_el);
        }
    }

    #[test]
    fn frame_conversion_round_trips() {
        let adm = [0.25, -0.5, 0.75];
        assert_eq!(scene_to_adm(adm_to_scene(adm)), adm);
    }

    #[test]
    fn spherical_round_trips() {
        for &(az, el, dist) in &[
            (0.0, 0.0, 1.0),
            (45.0, 30.0, 0.8),
            (-120.0, -15.0, 1.0),
            (179.0, 89.0, 0.5),
        ] {
            let (x, y, z) = from_spherical(az, el, dist);
            let (az2, el2, dist2) = to_spherical(x, y, z);
            close(az2, az);
            close(el2, el);
            close(dist2, dist);
        }
    }

    // ── Angles ──────────────────────────────────────────────────────────────

    #[test]
    fn normalize_and_wrap_agree_away_from_the_seam() {
        for a in [-359.0, -181.0, -90.0, 0.0, 90.0, 181.0, 540.0] {
            close(normalize_deg(a), wrap_deg(a));
        }
    }

    #[test]
    fn wrap_folds_the_negative_seam_onto_positive() {
        close(wrap_deg(-180.0), 180.0);
        close(wrap_deg(180.0), 180.0);
        // normalize_deg keeps -180 as itself; that difference is deliberate.
        close(normalize_deg(-180.0), -180.0);
    }

    #[test]
    fn wrapped_distance_takes_the_short_way() {
        close(wrapped_distance_deg(170.0, -170.0), 20.0);
        close(wrapped_distance_deg(-170.0, 170.0), 20.0);
        close(wrapped_distance_deg(0.0, 180.0), 180.0);
        close(wrapped_distance_deg(10.0, 10.0), 0.0);
    }

    #[test]
    fn snap_only_inside_the_threshold() {
        close(snap_deg(44.0, 45.0, 2.0), 45.0);
        close(snap_deg(41.0, 45.0, 2.0), 41.0);
        close(snap_deg(41.0, 0.0, 2.0), 41.0);
    }

    // ── Room geometry ───────────────────────────────────────────────────────

    #[test]
    fn depth_warp_pins_its_endpoints() {
        let (front, rear, blend) = (2.0, 1.5, 0.5);
        close(map_depth(0.0, front, rear, blend), 0.0);
        close(map_depth(1.0, front, rear, blend), front);
        close(map_depth(-1.0, front, rear, blend), -rear);
    }

    #[test]
    fn depth_warp_is_monotone() {
        let (front, rear, blend) = (2.0, 1.2, 0.35);
        let mut previous = map_depth(-1.0, front, rear, blend);
        for i in 1..=200 {
            let d = -1.0 + 2.0 * (i as f64) / 200.0;
            let current = map_depth(d, front, rear, blend);
            assert!(current > previous, "not monotone at d={d}");
            previous = current;
        }
    }

    #[test]
    fn depth_warp_inverts() {
        for &(front, rear, blend) in &[
            (1.0, 1.0, 0.5),
            (2.0, 1.5, 0.5),
            (3.0, 0.8, 0.0),
            (1.2, 2.4, 1.0),
        ] {
            for i in 0..=40 {
                let d = -1.0 + 2.0 * (i as f64) / 40.0;
                let mapped = map_depth(d, front, rear, blend);
                let back = inverse_map_depth(mapped, front, rear, blend);
                assert!(
                    (back - d).abs() < 1e-6,
                    "depth {d} round-tripped to {back} (front={front}, rear={rear}, blend={blend})"
                );
            }
        }
    }

    #[test]
    fn room_scaling_inverts() {
        let ratio = [1.5, 2.0, 1.1];
        let (rear, lower, blend) = (1.3, 0.6, 0.5);
        for &position in &[
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [-1.0, -1.0, -1.0],
            [0.3, -0.7, 0.25],
        ] {
            let scaled = room_scaled_position(position, ratio, rear, lower, blend);
            let back = inverse_room_scaled_position(scaled, ratio, rear, lower, blend);
            for axis in 0..3 {
                assert!(
                    (back[axis] - position[axis]).abs() < 1e-6,
                    "axis {axis}: {:?} -> {:?}",
                    position,
                    back
                );
            }
        }
    }

    #[test]
    fn degenerate_ratios_do_not_divide_by_zero() {
        let back = inverse_room_scaled_position([0.5, 0.5, -0.5], [0.0, 0.0, 0.0], 0.0, 0.0, 0.5);
        assert!(back.iter().all(|v| v.is_finite()));
    }

    // ── Hydration ───────────────────────────────────────────────────────────

    #[test]
    fn hydration_round_trips_a_layout_speaker() {
        // FR from layouts/7.1.4.yaml.
        let (az, el, dist) = hydrate_from_cartesian(1.0, 1.0, 0.0);
        close(az, 45.0);
        close(el, 0.0);
        close(dist, 2.0f64.sqrt());

        let (x, y, z) = hydrate_from_spherical(az, el, dist);
        close(x, 1.0);
        close(y, 1.0);
        close(z, 0.0);
    }

    #[test]
    fn hydration_floors_the_distance() {
        let (_, _, dist) = hydrate_from_cartesian(0.0, 0.0, 0.0);
        assert!(dist >= MIN_DISTANCE - EPS);
    }

    #[test]
    fn hydration_clamps_out_of_range_cartesian() {
        let (az, _, _) = hydrate_from_cartesian(5.0, 0.0, 0.0);
        close(az, 90.0);
    }

    // ── Sampling grids ──────────────────────────────────────────────────────

    #[test]
    fn an_even_axis_spans_its_ends_inclusively() {
        assert_eq!(evenly_spaced_axis(2, -1.0, 1.0), vec![-1.0, 1.0]);
        assert_eq!(evenly_spaced_axis(3, -1.0, 1.0), vec![-1.0, 0.0, 1.0]);
        assert_eq!(
            evenly_spaced_axis(5, 0.0, 1.0),
            vec![0.0, 0.25, 0.5, 0.75, 1.0]
        );
    }

    #[test]
    fn a_degenerate_axis_collapses_to_its_start() {
        assert_eq!(evenly_spaced_axis(1, -1.0, 1.0), vec![-1.0]);
        assert_eq!(evenly_spaced_axis(0, -1.0, 1.0), vec![-1.0]);
    }

    /// With no space below the listener the height axis is just the upper half.
    #[test]
    fn the_height_axis_without_a_negative_half_starts_at_zero() {
        assert_eq!(cartesian_z_axis(3, 0), vec![0.0, 0.5, 1.0]);
    }

    /// The asymmetric case, and the reason this is a function rather than a
    /// step: the two halves have independent resolutions, and the negative one
    /// stops short of zero so they do not both claim it.
    #[test]
    fn the_height_axis_halves_are_independent_and_meet_once() {
        let axis = cartesian_z_axis(3, 2);
        assert_eq!(axis, vec![-1.0, -0.5, 0.0, 0.5, 1.0]);
        assert_eq!(axis.iter().filter(|v| **v == 0.0).count(), 1);
    }

    #[test]
    fn the_height_axis_is_ascending_for_any_split() {
        for z_size in 2..8usize {
            for z_neg in 0..8usize {
                let axis = cartesian_z_axis(z_size, z_neg);
                assert_eq!(axis.len(), z_neg + z_size.max(2));
                for pair in axis.windows(2) {
                    assert!(
                        pair[1] > pair[0],
                        "not ascending at z_size={z_size} z_neg={z_neg}: {axis:?}"
                    );
                }
                assert_eq!(*axis.first().unwrap(), if z_neg > 0 { -1.0 } else { 0.0 });
                assert_eq!(*axis.last().unwrap(), 1.0);
            }
        }
    }

    /// A single-node request is widened to two, because a table axis with one
    /// position cannot interpolate.
    #[test]
    fn the_height_axis_never_degenerates_to_one_node() {
        assert_eq!(cartesian_z_axis(1, 0), vec![0.0, 1.0]);
        assert_eq!(cartesian_z_axis(0, 0), vec![0.0, 1.0]);
    }

    // ── Precision parity ────────────────────────────────────────────────────

    #[test]
    fn f32_and_f64_agree_within_single_precision() {
        for &(x, y, z) in &[(1.0, 1.0, 0.0), (-0.3, 0.8, 0.5), (0.0, -1.0, -0.25)] {
            let (az64, el64, _) = to_spherical(x, y, z);
            let (az32, el32, _) = super::f32::to_spherical(x as f32, y as f32, z as f32);
            assert!((az32 as f64 - az64).abs() < 1e-4);
            assert!((el32 as f64 - el64).abs() < 1e-4);
        }

        let m64 = map_depth(0.4, 2.0, 1.5, 0.5);
        let m32 = super::f32::map_depth(0.4, 2.0, 1.5, 0.5);
        assert!((m32 as f64 - m64).abs() < 1e-5);
    }
}
