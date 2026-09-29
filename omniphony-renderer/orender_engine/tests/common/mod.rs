//! Helpers shared by the tests that need a real bridge and stream: each is
//! skipped unless `ORENDER_BRIDGE` and `ORENDER_SAMPLE` are set.

use orender_engine::{Engine, RenderedAudio};
use std::path::Path;

/// Raw bytes per call: several access units, so one call returns many blocks.
pub const PACKET: usize = 4096;

/// Blocks as they came out, in order: each one's position and samples.
pub type Blocks = Vec<(u64, Vec<f32>)>;

/// An engine on the bridge `ORENDER_BRIDGE`, with the bytes of the stream
/// `ORENDER_SAMPLE`; `None`, with a note, when either is unset.
pub fn real_engine() -> Option<(Engine, Vec<u8>)> {
    let (Ok(bridge), Ok(sample)) = (
        std::env::var("ORENDER_BRIDGE"),
        std::env::var("ORENDER_SAMPLE"),
    ) else {
        eprintln!("skipping: set ORENDER_BRIDGE and ORENDER_SAMPLE");
        return None;
    };
    let data = std::fs::read(&sample).expect("read sample file");
    let engine = Engine::from_paths(None, None, Some(Path::new(&bridge)), None, 48_000)
        .expect("build engine");
    Some((engine, data))
}

/// Append `chunks` to `into`, hand their buffers back to the engine, and
/// return how many frames they held.
pub fn collect(engine: &mut Engine, chunks: Vec<RenderedAudio>, into: &mut Blocks) -> usize {
    let frames = chunks.iter().map(|c| c.n_frames).sum();
    into.extend(chunks.iter().map(|c| (c.sample_pos, c.samples.clone())));
    engine.recycle(chunks);
    frames
}
