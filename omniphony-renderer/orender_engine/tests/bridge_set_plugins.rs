//! Several real bridge plugins loaded together, routed by their probes
//! (`docs/multi-bridge.md`): two copies of the reference bridge, so that both
//! claim a WAV stream and the set has to pick one, whatever the reads.

mod common;

use bridge_api::{RDecodedFrame, RInputTransport};
use orender_engine::bridge_loader::{
    BRIDGE_STATUS_ERROR_MAX_BYTES, BRIDGE_STATUS_MAX_FAILURES, BridgeFailure, BridgeLibs,
    BridgeRequest, LoadedBridge, load_bridge_library, load_bridges, resolve_bridges,
};
use std::path::{Path, PathBuf};

/// `source` copied under `name` in this run's own directory: another file
/// to the dynamic loader, so a second plugin.
fn copy_as(source: &Path, dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(format!(
        "{}{name}{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    std::fs::copy(source, &path).expect("copy the reference bridge");
    path
}

fn demo() -> Vec<u8> {
    let demo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets/demo/spatial-demo.wav");
    let mut data = std::fs::read(demo).expect("read the demo WAV");
    data.truncate(256 * 1024);
    data
}

/// Every PCM sample `loaded` decodes from `stream`, pushed `chunk` bytes at
/// a time.
fn decode(loaded: &mut LoadedBridge, stream: &[u8], chunk: usize) -> Vec<i32> {
    let mut pcm = Vec::new();
    for piece in stream.chunks(chunk) {
        let result = loaded.bridge.push_packet(piece, RInputTransport::Raw, 0);
        assert!(result.error_message.is_empty(), "{}", result.error_message);
        pcm.extend(
            result
                .frames
                .iter()
                .flat_map(|f: &RDecodedFrame| f.pcm.iter().copied()),
        );
    }
    pcm
}

#[test]
fn a_stream_decodes_the_same_through_a_set_whatever_the_reads() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("bridge-set-plugins-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the test directory");
    let source = common::reference_bridge_path();
    let first = load_bridge_library(&copy_as(&source, &dir, "first_bridge")).unwrap();
    let second = load_bridge_library(&copy_as(&source, &dir, "second_bridge")).unwrap();

    let stream = demo();
    let mut alone = LoadedBridge::open(BridgeLibs::single(first)).unwrap();
    let expected = decode(&mut alone, &stream, 16 * 1024);
    assert!(!expected.is_empty(), "the demo decodes");

    // Bytes before the stream start belong to no stream and are dropped.
    let mut late = b"not a stream".to_vec();
    late.extend_from_slice(&stream);
    for (input, chunk) in [
        (&stream, 16 * 1024),
        (&stream, 7),
        (&late, 4096),
        (&late, 1),
    ] {
        let mut set = LoadedBridge::open(BridgeLibs::new(vec![first, second]).unwrap()).unwrap();
        assert_eq!(set.bridge.len(), 2);
        let decoded = decode(&mut set, input, chunk);
        assert!(
            decoded == expected,
            "{chunk}-byte reads of a {}-byte input decode {} samples, alone {}",
            input.len(),
            decoded.len(),
            expected.len()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Two real plugins and a path that names nothing: the host loads the two,
/// in the order asked, reports the third, and keeps it in what a Save writes.
#[test]
fn every_requested_bridge_loads_and_a_missing_one_is_reported() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("bridge-set-request-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the test directory");
    let source = common::reference_bridge_path();
    let a = copy_as(&source, &dir, "a_bridge");
    let b = copy_as(&source, &dir, "b_bridge");
    let missing = dir.join("missing_bridge.so");

    let request = resolve_bridges(&[b.clone(), missing.clone(), a.clone()], &[]).unwrap();
    assert_eq!(request.recorded, [b.clone(), missing.clone(), a.clone()]);
    let loaded = load_bridges(&request).unwrap();
    assert_eq!(loaded.loaded, [b.clone(), a.clone()]);
    let status = loaded.status();
    assert_eq!(status.len(), 3);
    assert!(status[0].error.is_none() && status[1].error.is_none());
    assert_eq!(status[2].path, missing.display().to_string());
    assert!(status[2].error.is_some());

    let set = LoadedBridge::open(loaded.libs).unwrap();
    assert_eq!(set.bridge.len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

/// abi_stable reports a layout mismatch in tens of kilobytes: the published
/// state summarises each error and lists a bounded number of failures, so it
/// still fits one datagram next to a bridge that works.
#[test]
fn the_published_failures_are_bounded() {
    let source = common::reference_bridge_path();
    let report = format!("first line\n{}", "Error: layout mismatch\n".repeat(2_000));
    let request = BridgeRequest {
        files: vec![source],
        failures: (0..BRIDGE_STATUS_MAX_FAILURES + 4)
            .map(|i| BridgeFailure {
                path: format!("/plugins/lib{i}_bridge.so").into(),
                error: report.clone(),
            })
            .collect(),
        recorded: Vec::new(),
    };
    let status = load_bridges(&request).unwrap().status();
    assert_eq!(status.len(), 1 + BRIDGE_STATUS_MAX_FAILURES);
    for entry in &status[1..] {
        let error = entry.error.as_deref().unwrap();
        assert!(
            error.len() <= BRIDGE_STATUS_ERROR_MAX_BYTES + 128,
            "{} bytes",
            error.len()
        );
    }
    let last = status.last().unwrap().error.as_deref().unwrap();
    assert!(last.contains("4 more bridges failed"), "{last}");
    let json = serde_json::to_string(&status).unwrap();
    assert!(json.len() < 16 * 1024, "{} bytes", json.len());
}
