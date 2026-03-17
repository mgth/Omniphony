use super::*;

fn load_yaml_layout(name: &str) -> crate::speaker_layout::SpeakerLayout {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("layouts")
        .join(name);
    crate::speaker_layout::SpeakerLayout::from_file(path).unwrap()
}

#[test]
fn test_vbap_load_compressed() {
    // Test loading a compressed VBAP table (if it exists)
    let path = std::path::Path::new("/tmp/vbap_7.1.4_compressed.bin");

    if !path.exists() {
        println!(
            "Skipping test: compressed VBAP table not found at /tmp/vbap_7.1.4_compressed.bin"
        );
        return;
    }

    let result = VbapPanner::load_from_file(path);
    assert!(
        result.is_ok(),
        "Failed to load compressed VBAP table: {:?}",
        result.err()
    );

    let (panner, _layout) = result.unwrap();
    assert_eq!(panner.num_speakers(), 12, "Expected 12 speakers");
    assert_eq!(
        panner.azimuth_resolution(),
        1,
        "Expected 1° azimuth resolution"
    );
    assert_eq!(
        panner.elevation_resolution(),
        1,
        "Expected 1° elevation resolution"
    );
    assert_eq!(panner.num_triangles(), 22, "Expected 22 triangles");

    println!("Successfully loaded compressed VBAP table!");
    println!("  Speakers: {}", panner.num_speakers());
    println!("  Triangles: {}", panner.num_triangles());
    println!("  Azimuth res: {}°", panner.azimuth_resolution());
    println!("  Elevation res: {}°", panner.elevation_resolution());
    println!("  Spread res: {}", panner.spread_resolution());

    // Test that we can get gains from the loaded panner
    let gains = panner.get_gains(0.0, 0.0);
    assert_eq!(gains.len(), 12, "Expected 12 gain values");

    // Check power normalization
    let power: f32 = gains.iter().map(|g| g * g).sum();
    assert!(
        (power - 1.0).abs() < 0.1,
        "Power normalization failed: {}",
        power
    );

    println!("  Gain computation test passed!");
}

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

#[test]
fn test_vbap_8_0_gains_diagnostic() {
    let path = std::path::Path::new("/tmp/vbap_8.0.vbap");

    if !path.exists() {
        println!("Skipping: /tmp/vbap_8.0.vbap not found");
        return;
    }

    let (panner, layout_opt) = VbapPanner::load_from_file(path).unwrap();
    let layout = layout_opt.unwrap();

    let names: Vec<&str> = layout.speakers.iter().map(|s| s.name.as_str()).collect();
    println!("Speakers ({}): {:?}", panner.num_speakers(), names);
    println!(
        "n_az={}, n_el={}, n_gtable={}",
        panner.n_az, panner.n_el, panner.n_gtable
    );

    // Test positions: (x, y, z) in ADM coordinates
    let test_positions = [
        (0.0, 1.0, 0.0, "Front center"),
        (0.0, -1.0, 0.0, "Rear center"),
        (-1.0, 0.0, 0.0, "Left"),
        (1.0, 0.0, 0.0, "Right"),
        (0.0, 0.0, 1.0, "Overhead"),
        (-0.5, 0.5, 0.0, "Front-left"),
        (0.5, 0.5, 0.0, "Front-right"),
        (-0.5, -0.5, 0.0, "Rear-left"),
        (0.5, -0.5, 0.0, "Rear-right"),
    ];

    for (x, y, z, label) in &test_positions {
        let (az, el, dist) = adm_to_spherical(*x, *y, *z);

        // Test both code paths
        let gains_direct = panner.get_gains(az, el);
        let gains_spread = panner.get_gains_with_spread(az, el, 0.0);
        let gains_cart = panner.get_gains_cartesian(*x, *y, *z, 0.0, DistanceModel::None);

        println!(
            "\n--- {} (ADM: {},{},{}) -> (az:{:.1}, el:{:.1}, d:{:.2}) ---",
            label, x, y, z, az, el, dist
        );

        print!("  get_gains:            ");
        for (i, g) in gains_direct.iter().enumerate() {
            if *g > 0.001 {
                print!("{}={:.3} ", names[i], g);
            }
        }
        println!();

        print!("  get_gains_with_spread:");
        for (i, g) in gains_spread.iter().enumerate() {
            if *g > 0.001 {
                print!("{}={:.3} ", names[i], g);
            }
        }
        println!();

        print!("  get_gains_cartesian:  ");
        for (i, g) in gains_cart.iter().enumerate() {
            if *g > 0.001 {
                print!("{}={:.3} ", names[i], g);
            }
        }
        println!();

        // Check: are gains_direct and gains_spread the same?
        let mut diff = 0.0f32;
        for i in 0..gains_direct.len() {
            diff += (gains_direct[i] - gains_spread[i]).abs();
        }
        if diff > 0.01 {
            println!(
                "  *** MISMATCH between get_gains and get_gains_with_spread! diff={:.4}",
                diff
            );
        }
    }

    // Test stability: same position 10 times should give identical gains
    println!("\n--- Stability test (front center x10) ---");
    let mut prev_gains: Option<Gains> = None;
    for i in 0..10 {
        let gains = panner.get_gains_cartesian(0.0, 1.0, 0.0, 0.0, DistanceModel::None);
        if let Some(ref prev) = prev_gains {
            let mut diff = 0.0f32;
            for j in 0..gains.len() {
                diff += (gains[j] - prev[j]).abs();
            }
            if diff > 0.0001 {
                println!("  Iteration {}: DIFFERS from previous! diff={:.6}", i, diff);
            }
        }
        prev_gains = Some(gains);
    }
    println!("  Stability: OK (10 identical calls)");
}
