//! `orender render <file> --output-backend file`: an offline render, driven
//! through the built binary with the reference bridge and the bundled demo.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Far above what the 2.5 s demo takes even in a debug build; only a run that
/// does not end on its own gets near it.
const EXIT_DEADLINE: Duration = Duration::from_secs(60);

/// Speakers in `layouts/7.1.4.yaml`, i.e. the rendered channel count.
const LAYOUT_CHANNELS: u64 = 12;

fn manifest_dir() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// The reference bridge cdylib, which the dev-dependency on
/// `reference_bridge` builds into `deps/` beside the binary (a workspace build
/// also copies it next to the binary itself).
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

/// Audio frames in a canonical PCM WAV: the `data` chunk over the block align.
fn wav_frame_count(wav: &[u8]) -> u64 {
    let block_align = u16::from_le_bytes([wav[32], wav[33]]) as u64;
    let mut pos = 12;
    while pos + 8 <= wav.len() {
        let id = &wav[pos..pos + 4];
        let len = u32::from_le_bytes(wav[pos + 4..pos + 8].try_into().unwrap()) as usize;
        if id == b"data" {
            return len as u64 / block_align;
        }
        pos += 8 + len + (len & 1);
    }
    panic!("no data chunk");
}

/// A non-continuous file render ends by itself once the input is exhausted,
/// successfully, with every input frame rendered. The CLI used to keep a frame
/// sender of its own beside the decoder thread's, so the channel never closed
/// at the end of the file and the process idled until it was killed.
#[test]
fn file_render_exits_at_end_of_input_with_full_output() {
    let input = manifest_dir().join("assets/demo/spatial-demo.wav");
    let layout = manifest_dir().join("../layouts/7.1.4.yaml");
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("file_render_exits_at_eof");
    let _ = std::fs::remove_dir_all(&work);
    // An empty config directory keeps the run off any per-user config.
    let config_dir = work.join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let output = work.join("out.f32");
    let log = work.join("stderr.log");

    let mut child = Command::new(env!("CARGO_BIN_EXE_orender"))
        .arg("render")
        .arg(&input)
        .arg("--bridge-path")
        .arg(reference_bridge_path())
        .args(["--enable-vbap", "--speaker-layout"])
        .arg(&layout)
        .args(["--output-backend", "file", "--output-file"])
        .arg(&output)
        .args(["--output-file-format", "raw-f32"])
        .args(["--no-osc", "--loglevel", "warn"])
        .env("OMNIPHONY_CONFIG_DIR", &config_dir)
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
                "orender still running {EXIT_DEADLINE:?} after start: the render did not end \
                 at the end of its input\n{}",
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

    let frames = wav_frame_count(&std::fs::read(&input).unwrap());
    let written = std::fs::metadata(&output).unwrap().len();
    assert_eq!(
        written,
        frames * LAYOUT_CHANNELS * 4,
        "rendered output is not the whole input ({frames} frames x {LAYOUT_CHANNELS} channels)"
    );
}
