//! `Engine::process_raw_within` — the path behind `orender_process`'s ">0 =
//! buffer too small, retry" return — must not lose or repeat audio.
//!
//! Skipped unless a real bridge and stream are given, like `parity.rs`. Use a
//! sample that decodes to actual audio (the parity fixture is silent):
//!
//! ```sh
//! ORENDER_BRIDGE=../../harletty-bridge/target/release/libharletty_bridge.so \
//! ORENDER_SAMPLE=/path/to/stream.thd \
//! cargo test -p orender_engine --test process_retry -- --nocapture
//! ```

mod common;

use common::{Blocks, PACKET, real_engine as setup};
use orender_engine::{Engine, RenderedAudio};

/// What one call handed back.
type Output = Blocks;

fn collect(engine: &mut Engine, chunks: Vec<RenderedAudio>) -> Output {
    let mut out = Output::new();
    common::collect(engine, chunks, &mut out);
    out
}

/// Every packet's output with a buffer that always fits.
fn reference() -> Option<Vec<Output>> {
    let (mut engine, data) = setup()?;
    Some(
        data.chunks(PACKET)
            .map(|p| {
                let chunks = engine
                    .process_raw_within(p, usize::MAX)
                    .expect("process")
                    .expect("an unbounded buffer always fits");
                collect(&mut engine, chunks)
            })
            .collect(),
    )
}

/// A host that starts with a small buffer and, on each "too small", doubles it
/// and retries the same packet gets exactly the audio of an unbounded buffer.
#[test]
fn retrying_the_same_packet_returns_its_audio_once() {
    let Some(expected) = reference() else { return };
    let (mut engine, data) = setup().unwrap();

    let mut capacity = 64usize;
    let mut retries = 0usize;
    for (i, p) in data.chunks(PACKET).enumerate() {
        let chunks = loop {
            match engine.process_raw_within(p, capacity).expect("process") {
                Some(chunks) => break chunks,
                None => {
                    capacity *= 2;
                    retries += 1;
                }
            }
        };
        assert_eq!(
            collect(&mut engine, chunks),
            expected[i],
            "packet {i}: output after retrying differs from an unbounded buffer"
        );
    }
    eprintln!("{retries} retries, final capacity {capacity} samples");
    assert!(
        retries > 0,
        "the buffer never came up short; the test proved nothing"
    );
}

/// A host that drops the packet on "too small" and moves on (mpv does) loses
/// that packet's audio but nothing else: every later packet renders exactly as
/// it would have.
#[test]
fn moving_on_after_a_short_buffer_keeps_the_stream_in_step() {
    let Some(expected) = reference() else { return };
    let (mut engine, data) = setup().unwrap();

    let mut capacity = 64usize;
    let mut dropped = 0usize;
    for (i, p) in data.chunks(PACKET).enumerate() {
        match engine.process_raw_within(p, capacity).expect("process") {
            Some(chunks) => assert_eq!(
                collect(&mut engine, chunks),
                expected[i],
                "packet {i}: output after a dropped packet differs"
            ),
            None => {
                capacity *= 2;
                dropped += 1;
            }
        }
    }
    eprintln!("{dropped} packets dropped, final capacity {capacity} samples");
    assert!(
        dropped > 0,
        "the buffer never came up short; the test proved nothing"
    );
}
