//! `Engine::set_decode_thread` — decoding on a thread of its own changes when a
//! packet's audio comes out, never what comes out.
//!
//! Skipped unless a real bridge and stream are given, like `process_retry.rs`.
//! Use a sample that decodes to actual audio (the parity fixture is silent):
//!
//! ```sh
//! ORENDER_BRIDGE=../../harletty-bridge/target/release/libharletty_bridge.so \
//! ORENDER_SAMPLE=/path/to/stream.thd \
//! cargo test --release -p orender_engine --test decode_thread -- --nocapture
//! ```

mod common;

use common::{Blocks as Stream, PACKET, collect};
use orender_engine::Engine;

fn setup(thread: bool) -> Option<(Engine, Vec<u8>)> {
    let (mut engine, data) = common::real_engine()?;
    engine.set_decode_thread(thread).expect("set_decode_thread");
    assert_eq!(engine.decode_thread(), thread);
    Some((engine, data))
}

/// Feed `packets` with a buffer that always fits, then drain until nothing is
/// left. Returns the frames the drain gave back.
fn render(engine: &mut Engine, packets: &[&[u8]], into: &mut Stream) -> usize {
    for p in packets {
        let chunks = engine
            .process_raw_within(p, usize::MAX)
            .expect("process")
            .expect("an unbounded buffer always fits");
        collect(engine, chunks, into);
    }
    let mut drained = 0;
    loop {
        let tail = engine.drain().expect("drain");
        match collect(engine, tail, into) {
            0 => return drained,
            frames => drained += frames,
        }
    }
}

/// The whole stream, drained, with the thread off.
fn reference() -> Option<Stream> {
    let (mut engine, data) = setup(false)?;
    let packets: Vec<&[u8]> = data.chunks(PACKET).collect();
    let mut out = Stream::new();
    let drained = render(&mut engine, &packets, &mut out);
    assert_eq!(
        drained, 0,
        "with the thread off there is nothing left to drain"
    );
    Some(out)
}

#[test]
fn the_thread_changes_when_audio_comes_out_not_what() {
    let Some(expected) = reference() else { return };
    let (mut engine, data) = setup(true).unwrap();
    let packets: Vec<&[u8]> = data.chunks(PACKET).collect();
    let mut out = Stream::new();
    let drained = render(&mut engine, &packets, &mut out);
    eprintln!(
        "{} blocks, {drained} frames came out of the drain",
        out.len()
    );
    assert_eq!(out.len(), expected.len(), "block count differs");
    assert!(
        out == expected,
        "the threaded stream differs from the inline one"
    );
}

/// A host that doubles its buffer and retries the same packet on "too small",
/// and the same for the drain, gets exactly the stream of an unbounded buffer.
#[test]
fn retries_on_a_short_buffer_lose_nothing_with_the_thread() {
    let Some(expected) = reference() else { return };
    let (mut engine, data) = setup(true).unwrap();

    let mut out = Stream::new();
    let mut capacity = 64usize;
    let mut retries = 0usize;
    for p in data.chunks(PACKET) {
        let chunks = loop {
            match engine.process_raw_within(p, capacity).expect("process") {
                Some(chunks) => break chunks,
                None => {
                    capacity *= 2;
                    retries += 1;
                }
            }
        };
        collect(&mut engine, chunks, &mut out);
    }
    let mut capacity = 1usize;
    loop {
        let tail = match engine.drain_with_capacity(capacity).expect("drain") {
            Some(chunks) => chunks,
            None => {
                capacity *= 2;
                retries += 1;
                continue;
            }
        };
        if collect(&mut engine, tail, &mut out) == 0 {
            break;
        }
    }
    eprintln!("{retries} retries");
    assert!(
        retries > 0,
        "the buffer never came up short; the test proved nothing"
    );
    assert!(out == expected, "retrying lost or repeated audio");
}

/// A reset drops what the thread was still decoding — it belongs to the old
/// position — and the stream after it is the same as with the thread off.
#[test]
fn a_reset_discards_what_was_in_flight() {
    let run = |thread: bool| -> Option<(Stream, Stream)> {
        let (mut engine, data) = setup(thread)?;
        let packets: Vec<&[u8]> = data.chunks(PACKET).collect();
        let (first, second) = packets.split_at(packets.len() / 2);
        let mut before = Stream::new();
        for p in first {
            let chunks = engine.process_raw_within(p, usize::MAX).unwrap().unwrap();
            collect(&mut engine, chunks, &mut before);
        }
        engine.reset();
        let mut after = Stream::new();
        render(&mut engine, second, &mut after);
        Some((before, after))
    };
    let Some((inline_before, inline_after)) = run(false) else {
        return;
    };
    let (threaded_before, threaded_after) = run(true).unwrap();
    assert!(
        inline_before.starts_with(&threaded_before),
        "before the reset the thread may hold packets back, never change them"
    );
    assert!(
        threaded_after == inline_after,
        "the stream after a reset differs with the thread on"
    );
}

/// The thread can be switched at the points the option documents: before the
/// first packet, and after a reset.
#[test]
fn switching_between_streams() {
    let run = |switch_to: Option<bool>| -> Option<Stream> {
        let (mut engine, data) = setup(true)?;
        let packets: Vec<&[u8]> = data.chunks(PACKET).take(200).collect();
        let mut first = Stream::new();
        render(&mut engine, &packets, &mut first);
        engine.reset();
        if let Some(on) = switch_to {
            engine.set_decode_thread(on).expect("switch after a reset");
            assert_eq!(engine.decode_thread(), on);
        }
        let mut second = Stream::new();
        render(&mut engine, &packets, &mut second);
        Some(second)
    };
    let Some(kept_on) = run(None) else { return };
    assert!(
        run(Some(false)).unwrap() == kept_on,
        "turning the thread off after a reset changed the audio"
    );
}
