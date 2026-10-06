//! Helpers shared by the tests that run a real bridge on a real stream.
//!
//! By default that is the reference bridge (built by `cargo test` through the
//! dev-dependency) on the bundled demo WAV, so these tests run everywhere, CI
//! included. `ORENDER_BRIDGE` and `ORENDER_SAMPLE`, set together, substitute
//! another bridge and stream, e.g. a format bridge on an object stream:
//!
//! ```sh
//! ORENDER_BRIDGE=../../harletty-bridge/target/release/libharletty_bridge.so \
//! ORENDER_SAMPLE=/path/to/stream.thd \
//! cargo test -p orender_engine --tests -- --nocapture
//! ```

#![allow(dead_code)] // each test target uses a different subset

use orender_engine::{Engine, RenderedAudio};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// Raw bytes per call: several access units, so one call returns many blocks.
pub const PACKET: usize = 4096;

/// Blocks as they came out, in order: each one's position and samples.
pub type Blocks = Vec<(u64, Vec<f32>)>;

/// The bridge and stream to run: `ORENDER_BRIDGE` / `ORENDER_SAMPLE` when both
/// are set, else the reference bridge on the bundled demo.
pub fn source() -> (PathBuf, PathBuf) {
    match (
        std::env::var_os("ORENDER_BRIDGE"),
        std::env::var_os("ORENDER_SAMPLE"),
    ) {
        (Some(bridge), Some(sample)) => (bridge.into(), sample.into()),
        (None, None) => (
            reference_bridge_path(),
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets/demo/spatial-demo.wav"),
        ),
        _ => panic!("set ORENDER_BRIDGE and ORENDER_SAMPLE together, or neither"),
    }
}

/// An engine on [`source`]'s bridge, with the bytes of its stream.
///
/// The engine gets a config path in a directory of its own, so it neither
/// reads nor writes the user's `config.yaml` or overlay prefs.
pub fn real_engine() -> (Engine, Vec<u8>) {
    let (bridge, sample) = source();
    let data =
        std::fs::read(&sample).unwrap_or_else(|e| panic!("read sample {}: {e}", sample.display()));
    let config = private_config_dir().join("config.yaml");
    let engine =
        Engine::from_paths(Some(&config), None, Some(&bridge), None, 48_000).expect("build engine");
    (engine, data)
}

/// Append `chunks` to `into`, hand their buffers back to the engine, and
/// return how many frames they held.
pub fn collect(engine: &mut Engine, chunks: Vec<RenderedAudio>, into: &mut Blocks) -> usize {
    let frames = chunks.iter().map(|c| c.n_frames).sum();
    into.extend(chunks.iter().map(|c| (c.sample_pos, c.samples.clone())));
    engine.recycle(chunks);
    frames
}

/// A fresh, empty directory under cargo's per-target scratch space: one per
/// call, as the tests of a target run in parallel in one process.
fn private_config_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "engine-config-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the test's config directory");
    dir
}

/// The reference bridge cdylib, which the dev-dependency on `reference_bridge`
/// builds for this run, searched from the test binary upwards. Cargo puts a
/// dependency's cdylib in `deps/` beside the binaries; the newer build-dir
/// layout (cargo nightly) puts it in `build/reference_bridge/<hash>/out/`
/// under the profile directory instead. The most recent of several wins.
fn reference_bridge_path() -> PathBuf {
    let exe = std::env::current_exe().expect("test binary path");
    let name = format!(
        "{}reference_bridge{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    let modified = |p: &Path| p.metadata().and_then(|m| m.modified()).ok();
    for dir in exe.ancestors().skip(1).take(5) {
        let mut found: Vec<PathBuf> = vec![dir.join(&name), dir.join("deps").join(&name)];
        if let Ok(entries) = std::fs::read_dir(dir.join("build").join("reference_bridge")) {
            found.extend(entries.flatten().map(|e| e.path().join("out").join(&name)));
        }
        if let Some(path) = found
            .into_iter()
            .filter(|p| p.is_file())
            .max_by_key(|p| modified(p))
        {
            return path;
        }
    }
    panic!("reference bridge {name} not built near {}", exe.display())
}
