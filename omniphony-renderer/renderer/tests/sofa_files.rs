//! The SOFA readers on real files: the small sets vendored under
//! `tests/sofa/` (see its README for where they come from), the corrupt
//! files that crashed libmysofa's reader, and deterministic truncations and
//! byte flips of the valid ones. A SOFA file is a user's download: whatever
//! it holds is loaded or refused with a reason naming it, never a panic or a
//! non-finite response.
#![cfg(feature = "sofa")]

use std::path::{Path, PathBuf};

use renderer::binaural::brir::{BrirLoadOptions, BrirSet};
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
        let peak = set.peak();
        assert!(peak.is_finite() && peak > 0.0, "{name}: peak {peak}");
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

/// Loaded or refused, never a panic, never a non-finite response; a refusal
/// names the file.
fn load_or_refuse(path: &Path, what: &str) -> (bool, bool) {
    let p = path_str(path);
    let name = path.file_name().unwrap().to_string_lossy().into_owned();
    let hrir = match hrir_set_from_sofa(p, RATE, true) {
        Ok(set) => {
            assert!(set.peak().is_finite(), "{what}: non-finite HRIR set");
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

/// Truncations at every 2 % and seeded byte flips of each valid file.
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
        for case in 0..100 {
            let mut damaged = bytes.clone();
            let what = if case < 50 {
                damaged.truncate(bytes.len() * case / 50);
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
