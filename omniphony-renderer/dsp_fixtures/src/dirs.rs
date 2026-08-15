//! Direction sets for sphere sweeps.
//!
//! Azimuth is degrees in `[-180, 180]` with 0 = front and +90 = right;
//! elevation is degrees in `[-90, 90]` with 0 = horizontal. This matches
//! `renderer::speaker_layout::Speaker`.

/// `n` approximately-uniform directions over the whole sphere, via a Fibonacci
/// lattice.
///
/// Preferred over a lat/long grid: a grid oversamples the poles badly, which
/// for a VBAP sweep means most of the test budget is spent re-measuring the
/// same two triplets.
pub fn fibonacci_sphere(n: usize) -> Vec<(f32, f32)> {
    assert!(n > 0, "fibonacci_sphere needs n > 0");
    // Golden angle: π(3 − √5).
    let golden_angle = std::f64::consts::PI * (3.0 - 5.0_f64.sqrt());
    (0..n)
        .map(|i| {
            // Half-step offsets give equal area per point and avoid landing
            // exactly on the poles.
            let z = 1.0 - 2.0 * (i as f64 + 0.5) / n as f64;
            let elevation = z.clamp(-1.0, 1.0).asin().to_degrees();
            let mut azimuth = (golden_angle * i as f64).to_degrees() % 360.0;
            if azimuth > 180.0 {
                azimuth -= 360.0;
            }
            (azimuth as f32, elevation as f32)
        })
        .collect()
}

/// Directions around the horizontal plane, `step_deg` apart, excluding the
/// duplicate endpoint at +180°.
pub fn horizontal_ring(step_deg: f32) -> Vec<(f32, f32)> {
    assert!(step_deg > 0.0, "step must be positive");
    let count = (360.0 / step_deg).round() as usize;
    (0..count)
        .map(|i| {
            let mut az = i as f32 * step_deg;
            if az > 180.0 {
                az -= 360.0;
            }
            (az, 0.0)
        })
        .collect()
}

/// Directions along one meridian at fixed azimuth, from nadir to zenith
/// inclusive.
pub fn meridian(azimuth_deg: f32, step_deg: f32) -> Vec<(f32, f32)> {
    assert!(step_deg > 0.0, "step must be positive");
    let steps = (180.0 / step_deg).round() as usize;
    (0..=steps)
        .map(|i| (azimuth_deg, -90.0 + i as f32 * step_deg))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fibonacci_returns_the_requested_count() {
        assert_eq!(fibonacci_sphere(512).len(), 512);
        assert_eq!(fibonacci_sphere(1).len(), 1);
    }

    #[test]
    fn fibonacci_directions_are_in_range() {
        for (az, el) in fibonacci_sphere(2048) {
            assert!((-180.0..=180.0).contains(&az), "azimuth {az} out of range");
            assert!((-90.0..=90.0).contains(&el), "elevation {el} out of range");
        }
    }

    #[test]
    fn fibonacci_covers_both_hemispheres() {
        let dirs = fibonacci_sphere(512);
        assert!(
            dirs.iter().any(|(_, el)| *el > 60.0),
            "no near-zenith point"
        );
        assert!(
            dirs.iter().any(|(_, el)| *el < -60.0),
            "no near-nadir point"
        );
    }

    #[test]
    fn fibonacci_has_no_duplicate_directions() {
        let dirs = fibonacci_sphere(512);
        for i in 0..dirs.len() {
            for j in (i + 1)..dirs.len() {
                let (a, b) = (dirs[i], dirs[j]);
                assert!(
                    (a.0 - b.0).abs() > 1e-4 || (a.1 - b.1).abs() > 1e-4,
                    "duplicate direction at {i} and {j}: {a:?}"
                );
            }
        }
    }

    #[test]
    fn horizontal_ring_is_flat_and_closed() {
        let ring = horizontal_ring(10.0);
        assert_eq!(ring.len(), 36, "360/10 points, endpoint excluded");
        assert!(ring.iter().all(|(_, el)| el.abs() < 1e-6));
    }

    #[test]
    fn meridian_spans_pole_to_pole() {
        let m = meridian(30.0, 15.0);
        assert!(m.iter().all(|(az, _)| (az - 30.0).abs() < 1e-6));
        assert!((m.first().expect("non-empty").1 - -90.0).abs() < 1e-6);
        assert!((m.last().expect("non-empty").1 - 90.0).abs() < 1e-6);
    }
}
