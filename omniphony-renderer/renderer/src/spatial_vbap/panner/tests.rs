use super::*;
use crate::spatial_vbap::spherical_to_adm;

#[cfg(feature = "saf_vbap")]
fn load_yaml_layout(name: &str) -> crate::speaker_layout::SpeakerLayout {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("layouts")
        .join(name);
    crate::speaker_layout::SpeakerLayout::from_file(path).unwrap()
}

#[cfg(feature = "saf_vbap")]
#[test]
fn test_vbap_panner_creation() {
    // Use a real preset with height speakers so 3D triangulation is valid.
    let layout = load_yaml_layout("7.1.4.yaml");
    let speakers = layout.spatializable_positions().0;

    let panner = VbapPanner::new(&speakers, 5, 5, 0.0);
    assert!(panner.is_ok());

    let panner = panner.unwrap();
    assert_eq!(panner.num_speakers(), speakers.len());
    assert!(panner.num_triangles() > 0);
}

#[cfg(feature = "saf_vbap")]
#[test]
fn test_vbap_gain_computation() {
    let layout = load_yaml_layout("7.1.4.yaml");
    let speakers = layout.spatializable_positions().0;

    let panner = VbapPanner::new(&speakers, 5, 5, 0.0).unwrap();

    // Test front center position (should activate L+R)
    let gains = panner.get_gains(0.0, 0.0);
    assert_eq!(gains.len(), speakers.len());

    // Power normalization: sum(gains²) should be approximately 1
    let power: f32 = gains.iter().map(|g| g * g).sum();
    assert!(
        (power - 1.0).abs() < 0.1,
        "Power normalization failed: {}",
        power
    );
}

#[cfg(feature = "saf_vbap")]
#[test]
fn test_vbap_error_cases() {
    // Too few speakers
    let speakers = vec![[0.0, 0.0], [30.0, 0.0]];
    assert!(VbapPanner::new(&speakers, 5, 5, 0.0).is_err());

    // Invalid resolution
    let speakers = vec![[0.0, 0.0], [-30.0, 0.0], [30.0, 0.0]];
    assert!(VbapPanner::new(&speakers, 0, 5, 0.0).is_err());
    assert!(VbapPanner::new(&speakers, 15, 5, 0.0).is_err());
}

#[test]
fn test_adm_to_spherical_cardinal_directions() {
    // Front center
    let (az, el, _dist) = adm_to_spherical(0.0, 1.0, 0.0);
    assert!((az - 0.0).abs() < 1.0, "Front center azimuth: {}", az);
    assert!((el - 0.0).abs() < 1.0, "Front center elevation: {}", el);

    // Left (X = -1)
    let (az, el, _dist) = adm_to_spherical(-1.0, 0.0, 0.0);
    assert!((az + 90.0).abs() < 1.0, "Left azimuth: {}", az);
    assert!((el - 0.0).abs() < 1.0, "Left elevation: {}", el);

    // Right (X = +1)
    let (az, el, _dist) = adm_to_spherical(1.0, 0.0, 0.0);
    assert!((az - 90.0).abs() < 1.0, "Right azimuth: {}", az);
    assert!((el - 0.0).abs() < 1.0, "Right elevation: {}", el);

    // Rear center (Y = -1)
    let (az, el, _dist) = adm_to_spherical(0.0, -1.0, 0.0);
    assert!(
        (az.abs() - 180.0).abs() < 1.0,
        "Rear center azimuth: {}",
        az
    );
    assert!((el - 0.0).abs() < 1.0, "Rear center elevation: {}", el);

    // Overhead (Z = 1)
    let (_az, el, _dist) = adm_to_spherical(0.0, 0.0, 1.0);
    assert!((el - 90.0).abs() < 1.0, "Overhead elevation: {}", el);
}

#[test]
fn test_adm_to_spherical_diagonal_positions() {
    // Front left (X = -0.707, Y = 0.707)
    let (az, el, _dist) = adm_to_spherical(-0.707, 0.707, 0.0);
    assert!((az + 45.0).abs() < 2.0, "Front left azimuth: {}", az);
    assert!((el - 0.0).abs() < 1.0, "Front left elevation: {}", el);

    // Front right (X = 0.707, Y = 0.707)
    let (az, el, _dist) = adm_to_spherical(0.707, 0.707, 0.0);
    assert!((az - 45.0).abs() < 2.0, "Front right azimuth: {}", az);
    assert!((el - 0.0).abs() < 1.0, "Front right elevation: {}", el);

    // Rear left (X = -0.707, Y = -0.707)
    let (az, el, _dist) = adm_to_spherical(-0.707, -0.707, 0.0);
    assert!((az + 135.0).abs() < 2.0, "Rear left azimuth: {}", az);
    assert!((el - 0.0).abs() < 1.0, "Rear left elevation: {}", el);

    // Rear right (X = 0.707, Y = -0.707)
    let (az, el, _dist) = adm_to_spherical(0.707, -0.707, 0.0);
    assert!((az - 135.0).abs() < 2.0, "Rear right azimuth: {}", az);
    assert!((el - 0.0).abs() < 1.0, "Rear right elevation: {}", el);
}

#[test]
fn test_adm_to_spherical_elevated_positions() {
    // Front elevated 45° (typical height speaker)
    // At 45° elevation, horizontal component = cos(45°) ≈ 0.707
    // Vertical component = sin(45°) ≈ 0.707
    let (az, el, _dist) = adm_to_spherical(0.0, 0.707, 0.707);
    assert!((az - 0.0).abs() < 2.0, "Front elevated azimuth: {}", az);
    assert!((el - 45.0).abs() < 2.0, "Front elevated elevation: {}", el);

    // Front left elevated
    let (az, el, _dist) = adm_to_spherical(-0.5, 0.5, 0.707);
    assert!((az + 45.0).abs() < 5.0, "FL elevated azimuth: {}", az);
    assert!(el > 35.0 && el < 55.0, "FL elevated elevation: {}", el);

    // Overhead front
    let (_az, el, _dist) = adm_to_spherical(0.0, 0.1, 0.995);
    assert!((el - 84.0).abs() < 5.0, "Near overhead elevation: {}", el);
}

#[test]
fn test_spherical_to_adm_roundtrip() {
    // Test several positions for roundtrip accuracy
    let test_positions = vec![
        (0.0, 0.0),    // Front center
        (-30.0, 0.0),  // Front left
        (30.0, 0.0),   // Front right
        (-90.0, 0.0),  // Left
        (90.0, 0.0),   // Right
        (180.0, 0.0),  // Rear
        (0.0, 45.0),   // Front elevated
        (-45.0, 30.0), // Front left elevated
    ];

    for (az_orig, el_orig) in test_positions {
        // Convert to ADM
        let (x, y, z) = spherical_to_adm(az_orig, el_orig, 1.0);

        // Convert back to spherical
        let (az_back, el_back, _dist) = adm_to_spherical(x, y, z);

        // Check roundtrip accuracy (within 1°)
        let az_diff = (az_orig - az_back).abs();
        // Handle wraparound at ±180°
        let az_diff = if az_diff > 180.0 {
            360.0 - az_diff
        } else {
            az_diff
        };

        assert!(
            az_diff < 1.0,
            "Azimuth roundtrip failed: {} -> ({},{},{}) -> {}, diff={}",
            az_orig,
            x,
            y,
            z,
            az_back,
            az_diff
        );
        assert!(
            (el_orig - el_back).abs() < 1.0,
            "Elevation roundtrip failed: {} -> ({},{},{}) -> {}",
            el_orig,
            x,
            y,
            z,
            el_back
        );
    }
}

#[test]
fn test_adm_coordinates_normalization() {
    // Test that ADM coordinates are properly normalized
    // Typical spatial audio object at (x=0.3, y=0.5, z=0.2)

    let (az, el, _dist) = adm_to_spherical(0.3, 0.5, 0.2);

    // Should be front-right elevated
    assert!(
        az > 0.0 && az < 90.0,
        "Azimuth should be front-right: {}",
        az
    );
    assert!(
        el > 0.0 && el < 45.0,
        "Elevation should be moderate: {}",
        el
    );

    // Back to ADM
    let (x, y, z) = spherical_to_adm(az, el, 1.0);

    // Check that we get unit sphere coordinates (distance = 1.0)
    let distance = (x * x + y * y + z * z).sqrt();
    assert!(
        (distance - 1.0).abs() < 0.01,
        "Should be on unit sphere: {}",
        distance
    );
}

#[test]
fn test_edge_cases() {
    // Origin (all zeros)
    let (_az, el, _dist) = adm_to_spherical(0.0, 0.0, 0.0);
    assert!((el - 0.0).abs() < 1.0, "Origin elevation: {}", el);

    // Very small values (near origin)
    let (_az, el, _dist) = adm_to_spherical(1e-8, 1e-8, 1e-8);
    assert!(
        (el - 90.0).abs() < 0.1,
        "Near origin elevation follows vertical stabilization threshold: {}",
        el
    );

    // Maximum elevation (zenith)
    let (_, el, _dist) = adm_to_spherical(0.0, 0.0, 1.0);
    assert!((el - 90.0).abs() < 0.1, "Zenith elevation: {}", el);

    // Minimum elevation (nadir)
    let (_, el, _dist) = adm_to_spherical(0.0, 0.0, -1.0);
    assert!((el + 90.0).abs() < 0.1, "Nadir elevation: {}", el);

    // Negative Z (below horizon) - should work
    let (_, el, _dist) = adm_to_spherical(0.0, 1.0, -0.5);
    assert!(
        el < 0.0,
        "Below horizon elevation should be negative: {}",
        el
    );
}

#[test]
fn test_yaml_speaker_order() {
    let yaml_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("layouts/8.0.yaml");
    if !yaml_path.exists() {
        println!("Skipping: layouts/8.0.yaml not found");
        return;
    }
    let layout = crate::speaker_layout::SpeakerLayout::from_file(&yaml_path).unwrap();
    println!("YAML speaker order ({} speakers):", layout.speakers.len());
    for (i, s) in layout.speakers.iter().enumerate() {
        println!(
            "  [{}] {} (az={}, el={}, spat={})",
            i, s.name, s.azimuth, s.elevation, s.spatialize
        );
    }
}
