//! Null test between the two render hosts: the bundled demo rendered by the
//! `orender` CLI to a file and by the embedded [`Engine`] (what `liborender`
//! runs inside mpv) must come out sample for sample the same. A change to the
//! frame path that reaches one host only shows up here as a difference.
//!
//! Runs with the reference bridge, which the dev-dependency builds, and the
//! demo in the repository: no external media.

use orender_engine::Engine;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Far above what the 2.5 s demo takes even in a debug build.
const EXIT_DEADLINE: Duration = Duration::from_secs(60);

/// What the CLI reads a file in (`decoder_thread`): the engine is fed the same
/// chunks, so the bridge cuts both streams into the same frames.
const CLI_READ_CHUNK: usize = 64 * 1024;

/// The demo's rate, which both hosts render at.
const DEMO_RATE: u32 = 48_000;

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn demo_input() -> PathBuf {
    manifest_dir().join("assets/demo/spatial-demo.wav")
}

fn layout() -> PathBuf {
    manifest_dir().join("../layouts/7.1.4.yaml")
}

/// The reference bridge cdylib, which the dev-dependency on
/// `reference_bridge` builds into `deps/` beside the binary.
fn reference_bridge_path() -> PathBuf {
    let dir = Path::new(env!("CARGO_BIN_EXE_orender"))
        .parent()
        .expect("binary directory");
    let name = format!(
        "{}reference_bridge{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    [dir.join("deps").join(&name), dir.join(&name)]
        .into_iter()
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("reference bridge {name} not built in {}", dir.display()))
}

/// A fresh work directory `name`, holding `config` as `config/config.yaml`.
fn work_dir(name: &str, config: &str) -> PathBuf {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(work.join("config")).unwrap();
    std::fs::write(work.join("config/config.yaml"), config).unwrap();
    work
}

/// The demo rendered by the CLI to a raw `f32` file, as samples.
fn render_with_cli(work: &Path) -> Vec<f32> {
    let output = work.join("cli.f32");
    let log = work.join("cli-stderr.log");
    let mut child = Command::new(env!("CARGO_BIN_EXE_orender"))
        .arg("render")
        .arg(demo_input())
        .arg("--bridge-path")
        .arg(reference_bridge_path())
        .args(["--enable-vbap", "--speaker-layout"])
        .arg(layout())
        .args(["--output-backend", "file", "--output-file"])
        .arg(&output)
        .args(["--output-file-format", "raw-f32"])
        .args(["--no-osc", "--loglevel", "warn"])
        .env("OMNIPHONY_CONFIG_DIR", work.join("config"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .expect("spawn orender");
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait on orender") {
            break status;
        }
        if started.elapsed() > EXIT_DEADLINE {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "orender still running {EXIT_DEADLINE:?} after start\n{}",
                std::fs::read_to_string(&log).unwrap_or_default()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        status.success(),
        "orender exited with {status}\n{}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
    std::fs::read(&output)
        .unwrap()
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
        .collect()
}

/// The demo rendered by the embedded engine with the same config, layout and
/// bridge, fed as the CLI reads it. Also the engine's output width.
fn render_with_engine(work: &Path) -> (Vec<f32>, u32) {
    let mut engine = Engine::from_paths(
        Some(&work.join("config/config.yaml")),
        Some(&layout()),
        Some(&reference_bridge_path()),
        None,
        DEMO_RATE,
    )
    .expect("build the engine");
    let mut samples = Vec::new();
    let mut channels = None;
    let mut take = |blocks: Vec<orender_engine::RenderedAudio>| {
        for block in blocks {
            assert_eq!(*channels.get_or_insert(block.n_channels), block.n_channels);
            samples.extend_from_slice(&block.samples);
        }
    };
    for chunk in std::fs::read(demo_input()).unwrap().chunks(CLI_READ_CHUNK) {
        take(engine.process_raw(chunk).expect("engine process"));
    }
    loop {
        let blocks = engine.drain().expect("engine drain");
        if blocks.is_empty() {
            break;
        }
        take(blocks);
    }
    (samples, channels.expect("the engine rendered nothing"))
}

/// Render the demo through both hosts with `config` and require the same
/// samples, bit for bit. Returns them.
fn assert_hosts_agree(name: &str, config: &str) -> Vec<f32> {
    let work = work_dir(name, config);
    let cli = render_with_cli(&work);
    let (engine, channels) = render_with_engine(&work);
    let channels = channels as usize;
    assert!(
        cli.iter().any(|s| *s != 0.0),
        "the CLI rendered silence: the comparison would prove nothing"
    );
    assert_eq!(
        cli.len(),
        engine.len(),
        "the CLI rendered {} frames, the engine {} ({channels} channels)",
        cli.len() / channels,
        engine.len() / channels
    );
    let differing = cli
        .iter()
        .zip(&engine)
        .filter(|(a, b)| a.to_bits() != b.to_bits())
        .count();
    if let Some(first) = cli
        .iter()
        .zip(&engine)
        .position(|(a, b)| a.to_bits() != b.to_bits())
    {
        let max_diff = cli
            .iter()
            .zip(&engine)
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        panic!(
            "the hosts render differently: {differing} of {} samples differ, the first at \
             frame {} channel {} (CLI {} vs engine {}), largest difference {max_diff:e}",
            cli.len(),
            first / channels,
            first % channels,
            cli[first],
            engine[first],
        );
    }
    cli
}

/// Channel content on speakers, with the default channel render mode.
#[test]
fn cli_and_engine_render_the_demo_identically() {
    assert_hosts_agree("host_parity_speakers", "render: {}\n");
}

/// Channel content with both object-synthesis stages on: phantom extraction
/// pulls objects out of the bed, the generator lifts some to the height
/// layer, and both ride the object path into the render.
#[test]
fn cli_and_engine_synthesize_objects_identically() {
    let synthesized = assert_hosts_agree(
        "host_parity_synthetic_objects",
        "render:\n  synthetic_objects_enabled: true\n  phantom_extract_mode: broadband\n  \
         object_generator_id: pad\n",
    );
    let plain = render_with_cli(&work_dir("host_parity_plain", "render: {}\n"));
    assert!(
        synthesized != plain,
        "the stages synthesized nothing: this compares the plain render again"
    );
}

/// The binaural headphone render (the demo's own configuration).
#[test]
fn cli_and_engine_render_binaural_identically() {
    assert_hosts_agree(
        "host_parity_binaural",
        &std::fs::read_to_string(manifest_dir().join("assets/demo/demo.yaml")).unwrap(),
    );
}
