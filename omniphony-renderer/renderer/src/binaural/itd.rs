//! Interaural time difference (ITD) from source direction.
//!
//! Uses Woodworth's spherical-head approximation. The broadband ITD is applied
//! as a pure per-ear delay (via [`crate::delay_line::DelayLine`]) rather than
//! baked into the HRIR phase, so head-tracking can move it smoothly without
//! re-deriving filters.

/// Default effective head radius (m) — KEMAR-ish. Live-tunable via
/// `BinauralLiveParams::head_radius_m` for per-listener ITD fit.
pub const DEFAULT_HEAD_RADIUS_M: f32 = 0.0875;
/// Speed of sound (m/s).
const SPEED_OF_SOUND: f32 = 343.0;

/// Per-ear delays in seconds for a source at the given azimuth/elevation.
///
/// `azimuth_rad`: 0 = front, positive = source to the **right**.
/// `elevation_rad`: 0 = horizontal, +π/2 = up.
///
/// Returns `(left_delay_s, right_delay_s)`, both ≥ 0: the ear nearer the source
/// gets 0, the far (contralateral) ear gets the positive ITD. A source on the
/// right therefore delays the **left** ear.
///
/// The formula is Woodworth's spherical-head ITD in its three-dimensional
/// form (Larcher & Jot): `Δt = (r/c)·(λ + sin λ)` where `λ` is the **lateral
/// angle** between the source and the median plane, `sin λ = cos(el)·sin(az)`.
/// Everything the head does to the ITD depends on that one angle — a source
/// on the interaural axis has the full ITD, one in the median plane none, and
/// front/back mirrors share it — so the lateral angle is the right variable,
/// not the azimuth. Scaling the horizontal-plane value by `cos(el)` instead
/// (the previous form) coincides with this on the horizon but overshoots as
/// the source rises on the side: +16 % at (90°, 30°), +22 % at (90°, 45°),
/// which are exactly the directions of the height channels.
pub fn ear_delays_seconds(azimuth_rad: f32, elevation_rad: f32, head_radius_m: f32) -> (f32, f32) {
    // Signed sine of the lateral angle: +1 at the right ear, −1 at the left,
    // 0 anywhere in the median plane (front, back, overhead alike).
    ear_delays_from_lateral(elevation_rad.cos() * azimuth_rad.sin(), head_radius_m)
}

/// [`ear_delays_seconds`] for a direction given by the **sine of its lateral
/// angle** — the `x` component of the unit head-relative vector, which a
/// caller that already has the vector need not turn back into angles.
pub fn ear_delays_from_lateral(lateral_sine: f32, head_radius_m: f32) -> (f32, f32) {
    let s = lateral_sine.clamp(-1.0, 1.0);
    let lateral = s.abs().asin();
    let mag = (head_radius_m / SPEED_OF_SOUND) * (lateral + s.abs());
    if s >= 0.0 {
        // Source on the right → left ear is the far ear.
        (mag, 0.0)
    } else {
        (0.0, mag)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn front_source_has_zero_itd() {
        let (l, r) = ear_delays_seconds(0.0, 0.0, DEFAULT_HEAD_RADIUS_M);
        assert!(l.abs() < 1e-9 && r.abs() < 1e-9);
    }

    #[test]
    fn right_source_delays_left_ear() {
        let (l, r) = ear_delays_seconds(std::f32::consts::FRAC_PI_2, 0.0, DEFAULT_HEAD_RADIUS_M);
        assert!(l > r);
        assert!(r.abs() < 1e-9);
        // Max ITD for a 0.0875 m head ≈ 0.66 ms.
        assert!((l - 0.00066).abs() < 0.0002, "itd={l}");
    }

    #[test]
    fn left_source_delays_right_ear() {
        let (l, r) = ear_delays_seconds(-std::f32::consts::FRAC_PI_2, 0.0, DEFAULT_HEAD_RADIUS_M);
        assert!(r > l);
        assert!(l.abs() < 1e-9);
    }

    #[test]
    fn elevated_source_has_smaller_itd_than_horizontal() {
        let (l_horiz, _) =
            ear_delays_seconds(std::f32::consts::FRAC_PI_2, 0.0, DEFAULT_HEAD_RADIUS_M);
        let (l_high, _) = ear_delays_seconds(
            std::f32::consts::FRAC_PI_2,
            std::f32::consts::FRAC_PI_4,
            DEFAULT_HEAD_RADIUS_M,
        );
        assert!(l_high < l_horiz && l_high > 0.0);
    }

    /// The horizontal-plane value is unchanged by the lateral-angle form.
    #[test]
    fn horizontal_plane_matches_woodworth() {
        let r_c = DEFAULT_HEAD_RADIUS_M / SPEED_OF_SOUND;
        for az_deg in [15.0f32, 45.0, 75.0, 90.0] {
            let az = az_deg.to_radians();
            let (l, _) = ear_delays_seconds(az, 0.0, DEFAULT_HEAD_RADIUS_M);
            let expected = r_c * (az + az.sin());
            assert!(
                (l - expected).abs() < 1e-7,
                "az {az_deg}: {l} vs {expected}"
            );
        }
    }

    /// Off the horizon the ITD is a function of the lateral angle alone: a
    /// source at (az, el) must carry the same ITD as the horizontal source at
    /// the lateral angle `asin(cos el · sin az)`.
    #[test]
    fn elevated_source_matches_its_lateral_angle() {
        for (az_deg, el_deg) in [(90.0f32, 30.0f32), (90.0, 45.0), (60.0, 30.0), (30.0, 60.0)] {
            let (az, el) = (az_deg.to_radians(), el_deg.to_radians());
            let lateral = (el.cos() * az.sin()).asin();
            let (l_el, r_el) = ear_delays_seconds(az, el, DEFAULT_HEAD_RADIUS_M);
            let (l_h, r_h) = ear_delays_seconds(lateral, 0.0, DEFAULT_HEAD_RADIUS_M);
            assert!((l_el - l_h).abs() < 1e-7 && (r_el - r_h).abs() < 1e-7);
        }
    }

    /// Hand-computed Larcher–Jot values, r = 0.0875 m, c = 343 m/s. The old
    /// `cos(el)`-scaled form gave 0.568 ms and 0.464 ms for the first two.
    #[test]
    fn height_channel_directions_match_the_closed_form() {
        let cases = [
            (90.0f32, 30.0f32, 0.4881e-3f32),
            (90.0, 45.0, 0.3808e-3),
            (60.0, 30.0, 0.4077e-3),
            (90.0, 60.0, 0.2611e-3),
        ];
        for (az_deg, el_deg, expected) in cases {
            let (l, _) = ear_delays_seconds(
                az_deg.to_radians(),
                el_deg.to_radians(),
                DEFAULT_HEAD_RADIUS_M,
            );
            assert!(
                (l - expected).abs() < 1e-6,
                "({az_deg}°, {el_deg}°): {l:.4e} vs {expected:.4e}"
            );
        }
    }

    /// Front/back mirrors share the lateral angle, hence the ITD — and the
    /// median plane has none anywhere along it, overhead included.
    #[test]
    fn front_back_mirror_and_median_plane() {
        let (l_f, r_f) = ear_delays_seconds(30f32.to_radians(), 0.0, DEFAULT_HEAD_RADIUS_M);
        let (l_b, r_b) = ear_delays_seconds(150f32.to_radians(), 0.0, DEFAULT_HEAD_RADIUS_M);
        assert!((l_f - l_b).abs() < 1e-7 && (r_f - r_b).abs() < 1e-7);
        for (az_deg, el_deg) in [(0.0f32, 45.0f32), (180.0, 30.0), (0.0, 90.0), (90.0, 90.0)] {
            let (l, r) = ear_delays_seconds(
                az_deg.to_radians(),
                el_deg.to_radians(),
                DEFAULT_HEAD_RADIUS_M,
            );
            assert!(
                l.abs() < 1e-6 && r.abs() < 1e-6,
                "({az_deg}°, {el_deg}°): {l} {r}"
            );
        }
    }
}
