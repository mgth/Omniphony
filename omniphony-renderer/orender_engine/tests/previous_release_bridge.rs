//! A bridge built at the last release, loaded into this host: the outcome
//! `BRIDGE_API.md` ("Versioning") promises for it.
//!
//! - Same `bridge_api` minor as this host: it loads and decodes.
//! - Another minor: it is refused before anything of it runs, by an error
//!   that names both versions — not by an abi_stable layout error.
//!
//! CI builds the reference bridge at the last `v*` tag and points
//! `ORENDER_PREVIOUS_RELEASE_BRIDGE` at it (see the "previous release bridge"
//! step of `.github/workflows/ci.yml`). By hand:
//!
//! ```sh
//! ORENDER_PREVIOUS_RELEASE_BRIDGE=/path/to/libreference_bridge.so \
//! cargo test -p orender_engine --test previous_release_bridge -- --ignored
//! ```

use abi_stable::library::lib_header_from_path;
use abi_stable::std_types::RSlice;
use bridge_api::RInputTransport;
use orender_engine::bridge_loader::{LoadedBridge, host_bridge_api_version};
use std::path::{Path, PathBuf};

const BRIDGE_VAR: &str = "ORENDER_PREVIOUS_RELEASE_BRIDGE";

#[test]
#[ignore = "needs ORENDER_PREVIOUS_RELEASE_BRIDGE, the reference bridge built at the last release tag"]
fn the_last_release_bridge_loads_or_is_refused_by_version() {
    let path = PathBuf::from(
        std::env::var_os(BRIDGE_VAR)
            .unwrap_or_else(|| panic!("{BRIDGE_VAR} is not set: nothing to test")),
    );
    let bridge = lib_header_from_path(&path)
        .expect("the previous release's bridge has an abi_stable header")
        .version_strings()
        .parsed()
        .expect("its bridge_api version parses");
    let host = host_bridge_api_version();
    eprintln!("host bridge_api {host}, previous release bridge built against {bridge}");

    let loaded = LoadedBridge::load_with_params(&path);
    if (bridge.major, bridge.minor) == (host.major, host.minor) {
        let mut loaded = loaded.expect("a bridge of the host's bridge_api minor loads");
        decodes_the_demo(&mut loaded);
    } else {
        let err = format!(
            "{:#}",
            loaded
                .err()
                .expect("a bridge of another bridge_api minor is refused")
        );
        assert!(
            err.contains(&format!("bridge_api {bridge}"))
                && err.contains(&format!("{}.{}.x", host.major, host.minor)),
            "the refusal must name both versions: {err}"
        );
        eprintln!("refused as promised: {err}");
    }
}

/// The reference bridge presents a WAV file as a channel bed: feed it the
/// demo and expect frames and no error.
fn decodes_the_demo(loaded: &mut LoadedBridge) {
    let demo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets/demo/spatial-demo.wav");
    let data = std::fs::read(&demo).expect("read the demo WAV");
    assert!(loaded.configure("presentation", "best"));
    let mut frames = 0;
    for chunk in data.chunks(16 * 1024).take(8) {
        let result = loaded
            .bridge
            .push_packet(RSlice::from_slice(chunk), RInputTransport::Raw, 0);
        assert!(
            result.error_message.is_empty(),
            "decode error: {}",
            result.error_message
        );
        frames += result.frames.len();
    }
    assert!(frames > 0, "the previous release's bridge decoded nothing");
}
