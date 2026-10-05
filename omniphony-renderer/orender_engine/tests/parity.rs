//! End-to-end functional harness: render a real stream through the engine and
//! its bridge, and check the output is sane (channel layout, frames produced,
//! finite, non-silent) and that the OSC wiring broadcasts it.
//!
//! This exercises the exact path the FFI uses (`Engine::from_paths` →
//! `process_raw`). It runs the reference bridge on the bundled demo unless
//! another bridge and stream are given (see `common`). The object-stream checks
//! need such a stream and are ignored otherwise:
//!
//! ```sh
//! ORENDER_BRIDGE=../../harletty-bridge/target/release/libharletty_bridge.so \
//! ORENDER_SAMPLE=../../reference-sources/libstarmine_ad/crates/libstarmine_ad/tests/data/truehd_atmos_prefix_32k.mlp \
//! cargo test -p orender_engine --test parity -- --include-ignored --nocapture
//! ```
//!
//! Not written yet: a bit-exact comparison of this path against the `orender`
//! CLI's file output (`tests/file_render.rs` at the workspace root).

mod common;

use orender_engine::{Engine, OscOptions};
use std::net::UdpSocket;
use std::time::Duration;

/// A socket for the engine's OSC broadcasts, and the options pointing them at
/// it: an ephemeral port, never a live instance's.
fn osc_client() -> (UdpSocket, OscOptions) {
    let client = UdpSocket::bind("127.0.0.1:0").expect("bind client socket");
    client
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let options = OscOptions {
        host: "127.0.0.1".to_string(),
        port_out: client.local_addr().unwrap().port(),
        port_in: 0,
        metering: false,
    };
    (client, options)
}

/// Whether any datagram already queued on `client` mentions `address`.
fn received(client: &UdpSocket, address: &str) -> bool {
    let mut buf = [0u8; 16384];
    while let Ok((n, _)) = client.recv_from(&mut buf) {
        if buf[..n]
            .windows(address.len())
            .any(|w| w == address.as_bytes())
        {
            return true;
        }
    }
    false
}

#[test]
fn renders_a_real_stream() {
    let (mut engine, data) = common::real_engine();
    let (_client, options) = osc_client();
    engine
        .enable_osc(options)
        .expect("enable_osc should start the OSC listener");

    let channels = engine.channel_count();

    // Output channel-layout export: the 7.1.4 preset must map cleanly to labels
    // (no Unknown), one per speaker, in render order. This is what the FFI's
    // orender_channel_layout() hands mpv to build its chmap.
    use bridge_api::RChannelLabel::*;
    let layout = engine.channel_layout();
    assert_eq!(channels, 12, "7.1.4 preset must yield 12 speakers");
    assert_eq!(layout.len(), channels as usize, "one label per speaker");
    assert_eq!(
        layout,
        vec![L, R, C, LFE, Lb, Rb, Ls, Rs, Tfl, Tfr, Tbl, Tbr],
        "7.1.4 preset channel labels (render order)"
    );

    let mut total_frames = 0usize;
    let mut peak = 0.0f32;
    for packet in data.chunks(common::PACKET) {
        let chunks = engine.process_raw(packet).expect("process_raw");
        for c in &chunks {
            assert_eq!(c.n_channels, channels, "channel count must stay stable");
            assert_eq!(
                c.samples.len(),
                c.n_frames * c.n_channels as usize,
                "sample count must equal frames * channels"
            );
            for &s in &c.samples {
                assert!(s.is_finite(), "rendered sample must be finite");
                peak = peak.max(s.abs());
            }
            total_frames += c.n_frames;
        }
        engine.recycle(chunks);
    }

    eprintln!("parity harness: channels={channels} frames={total_frames} peak={peak:.6}");
    assert!(total_frames > 0, "expected at least one rendered frame");
    // The stream must decode to sound: a silent fixture (e.g. a metadata-only
    // prefix) would let a render that drops everything pass.
    assert!(peak > 0.0, "the render is silent");
}

/// An object stream reports its objects and broadcasts their positions.
#[test]
#[ignore = "needs ORENDER_BRIDGE and ORENDER_SAMPLE naming an object stream (see the module doc)"]
fn an_object_stream_reports_and_broadcasts_its_objects() {
    assert!(
        std::env::var_os("ORENDER_SAMPLE").is_some(),
        "set ORENDER_BRIDGE and ORENDER_SAMPLE to an object stream"
    );
    let (mut engine, data): (Engine, Vec<u8>) = common::real_engine();
    let (client, options) = osc_client();
    engine.enable_osc(options).expect("enable_osc");

    assert!(engine.has_objects(), "the stream must report objects");
    // Object frames are sent synchronously during process_raw to the permanent
    // target (our client), so they're buffered by the time we read.
    for packet in data.chunks(common::PACKET) {
        let chunks = engine.process_raw(packet).expect("process_raw");
        engine.recycle(chunks);
    }
    eprintln!(
        "parity harness: object_count={} dialnorm_db={:?}",
        engine.object_count(),
        engine.dialnorm_db()
    );
    assert!(
        engine.object_count() > 0,
        "an object stream must report a positive object count"
    );
    assert!(
        received(&client, runtime_control::osc_contract::SPATIAL_FRAME),
        "expected at least one spatial frame OSC broadcast"
    );
}
