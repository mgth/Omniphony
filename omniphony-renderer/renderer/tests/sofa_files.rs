//! The SOFA readers on real files: the small sets vendored under
//! `tests/sofa/` (see its README for where they come from), the corrupt
//! files that crashed libmysofa's reader, and deterministic truncations and
//! byte flips of the valid ones. A SOFA file is a user's download: whatever
//! it holds is loaded or refused with a reason naming it, never a panic or a
//! non-finite response.
#![cfg(feature = "sofa")]

use std::path::{Path, PathBuf};

use renderer::binaural::brir::{BrirLoadOptions, BrirSet};
use renderer::binaural::hrir::{HRIR_LEN, HrirPair, HrirSet};
use renderer::binaural::measured::hrir_set_from_sofa;

const RATE: u32 = 48_000;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/sofa")
        .join(name)
}

fn path_str(path: &Path) -> &str {
    path.to_str().expect("UTF-8 fixture path")
}

fn brir(path: &Path) -> anyhow::Result<BrirSet> {
    BrirSet::from_sofa(path_str(path), RATE, &BrirLoadOptions::default())
}

fn finite_brir(set: &BrirSet) -> bool {
    (0..set.emitters().len()).all(|e| {
        (0..set.orientations().len()).all(|o| {
            let p = set.pair(e, o);
            p.left.iter().chain(&p.right).all(|s| s.is_finite())
        })
    })
}

/// Every kernel the set renders, both ears, at each node of its 5° grid
/// (azimuth all round, elevation -40° to 90°). `peak` cannot tell: it folds
/// with `f32::max`, which skips a NaN.
fn finite_hrir(set: &HrirSet) -> bool {
    let mut pair = HrirPair {
        left: [0.0; HRIR_LEN],
        right: [0.0; HRIR_LEN],
    };
    (-8..=18).all(|el| {
        (0..72).all(|az| {
            set.at(az as f32 * 5.0, el as f32 * 5.0, &mut pair);
            let taps = set.len();
            pair.left[..taps]
                .iter()
                .chain(&pair.right[..taps])
                .all(|s| s.is_finite())
        })
    })
}

/// Free-field HRIR sets as measured and as written by two different tools
/// (the Matlab API, and netCDF4 through SOFAsonix, whose object headers
/// sofar 0.3.0 could not read: issue #185).
#[test]
fn free_field_hrir_files_load_as_a_direction_dependent_set() {
    for name in [
        "Pulse.sofa",
        "tester.sofa",
        "tester2.sofa",
        "sofasonix_netcdf4.sofa",
    ] {
        let path = fixture(name);
        let set = hrir_set_from_sofa(path_str(&path), RATE, true)
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(!set.is_empty(), "{name}");
        assert!(finite_hrir(&set), "{name}: non-finite HRIR");
        assert!(set.peak() > 0.0, "{name}: silent");
        assert!(!set.is_direction_invariant(), "{name}");
    }
}

/// A multi-speaker room response, stored chunked as recent writers do: its
/// three speakers load as three emitters seen from one orientation. The same
/// file is not a free-field set, and the HRIR reader says so.
#[test]
fn a_multispeaker_brir_file_loads_its_speakers() {
    let path = fixture("chunked_multispeaker_brir.sofa");
    let set = brir(&path).expect("loads");
    assert_eq!(set.conventions(), "MultiSpeakerBRIR");
    assert_eq!(set.emitters().len(), 3);
    assert_eq!(set.orientations().len(), 1);
    assert_eq!(set.room_corners(), None, "no room corners in the base file");
    assert!(set.max_taps() > 0);
    assert!(finite_brir(&set));
    let energy = |e: usize| {
        let p = set.pair(e, 0);
        p.left.iter().chain(&p.right).map(|s| s * s).sum::<f32>()
    };
    assert!((0..3).all(|e| energy(e) > 0.0), "every speaker sounds");

    let err = hrir_set_from_sofa(path_str(&path), RATE, true)
        .err()
        .expect("not a free-field set");
    assert!(
        err.to_string().contains("chunked_multispeaker_brir.sofa"),
        "{err}"
    );
}

fn assert_room_corners(set: &BrirSet, expected: [[f32; 3]; 2], name: &str) {
    let actual = set
        .room_corners()
        .unwrap_or_else(|| panic!("{name}: no corners"));
    for (actual, expected) in actual.iter().flatten().zip(expected.iter().flatten()) {
        assert!(
            (actual - expected).abs() < 1e-5,
            "{name}: expected {expected}, got {actual}"
        );
    }
    assert_eq!(set.room_type(), Some("shoebox"), "{name}");
    assert_eq!(set.emitters().len(), 3, "{name}");
    assert!(set.max_taps() > 0, "{name}");
    assert!(finite_brir(set), "{name}");
}

/// Exercise the file loader, including the HDF datasets and their metadata:
/// both encodings describe the same box in SOFA coordinates. In the renderer
/// frame +x is right and +y is front, so the far corner is (-4, 6, 2.5).
#[test]
fn room_corners_load_from_cartesian_and_spherical_files() {
    for name in [
        "room_corners_cartesian.sofa",
        "room_corners_spherical.sofa",
        "room_corners_own_metadata.sofa",
    ] {
        let set = brir(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_room_corners(&set, [[0.0, 0.0, 0.0], [-4.0, 6.0, 2.5]], name);
    }
}

/// The listener stored in the file is subtracted before changing frames;
/// testing only a listener at the origin would miss a lost translation.
#[test]
fn room_corners_load_relative_to_the_files_listener() {
    let name = "room_corners_offset_listener.sofa";
    let set = brir(&fixture(name)).expect("offset-listener room loads");
    assert_room_corners(&set, [[2.0, -3.0, -1.2], [-2.0, 3.0, 1.3]], name);
}

/// Unknown units suppress the room box, without preventing its responses
/// and speakers from loading for the loudspeaker-box fallback.
#[test]
fn room_corners_with_unsupported_units_do_not_discard_the_brir() {
    let set = brir(&fixture("room_corners_unsupported_unit.sofa")).expect("room loads");
    let base = brir(&fixture("chunked_multispeaker_brir.sofa")).expect("base loads");
    assert_eq!(set.room_corners(), None);
    assert_eq!(set.room_type(), Some("shoebox"));
    assert_eq!(set.emitters(), base.emitters());
    assert_eq!(set.orientations(), base.orientations());
    assert_eq!(set.max_taps(), base.max_taps());
    for e in 0..base.emitters().len() {
        for o in 0..base.orientations().len() {
            assert_eq!(set.pair(e, o).left, base.pair(e, o).left);
            assert_eq!(set.pair(e, o).right, base.pair(e, o).right);
        }
    }
}

/// Loaded or refused, never a panic, never a non-finite response; a refusal
/// names the file.
fn load_or_refuse(path: &Path, what: &str) -> (bool, bool) {
    let p = path_str(path);
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let hrir = match hrir_set_from_sofa(p, RATE, true) {
        Ok(set) => {
            assert!(finite_hrir(&set), "{what}: non-finite HRIR set");
            true
        }
        Err(e) => {
            assert!(e.to_string().contains(&name), "{what}: {e}");
            false
        }
    };
    let room = match brir(path) {
        Ok(set) => {
            assert!(finite_brir(&set), "{what}: non-finite room response");
            true
        }
        Err(e) => {
            assert!(e.to_string().contains(&name), "{what}: {e}");
            false
        }
    };
    (hrir, room)
}

/// The fuzzing corpus libmysofa's crashes were filed with. Most are refused;
/// two still describe a readable set and load.
#[test]
fn files_that_crashed_libmysofa_are_loaded_or_refused() {
    let dir = fixture("malformed");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "sofa"))
        .collect();
    files.sort();
    assert!(files.len() >= 19, "the corpus is there: {}", files.len());
    let mut refused = 0;
    for file in &files {
        let (hrir, room) = load_or_refuse(file, &file.display().to_string());
        refused += usize::from(!hrir && !room);
    }
    assert!(refused >= 15, "{refused} of {} refused", files.len());
}

/// Truncations at every 5 % and seeded byte flips of each valid file. Few
/// cases: the parser's own fuzzing lives in sofar (`tests/malformed.rs`);
/// these check what this crate does with what the parser lets through, and
/// every copy that still loads costs a whole HRIR set build.
#[test]
fn damaged_copies_of_valid_files_are_loaded_or_refused() {
    let dir = std::env::temp_dir().join(format!("orender-sofa-damage-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    for source in [
        "Pulse.sofa",
        "sofasonix_netcdf4.sofa",
        "chunked_multispeaker_brir.sofa",
    ] {
        let bytes = std::fs::read(fixture(source)).unwrap();
        for case in 0..30 {
            let mut damaged = bytes.clone();
            let what = if case < 20 {
                damaged.truncate(bytes.len() * case / 20);
                format!("{source} cut at {} bytes", damaged.len())
            } else {
                for _ in 0..=case % 8 {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    let at = (seed % damaged.len() as u64) as usize;
                    damaged[at] = (seed >> 32) as u8;
                }
                format!("{source} with flipped bytes, case {case}")
            };
            let path = dir.join(format!("damaged-{case}.sofa"));
            std::fs::write(&path, &damaged).unwrap();
            load_or_refuse(&path, &what);
        }
    }
    std::fs::remove_dir_all(&dir).unwrap();
}
