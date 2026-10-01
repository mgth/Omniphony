//! Where the listener is, for clients that show what is heard rather than what
//! was just rendered.
//!
//! The engine describes each block of audio — its object frame, its timestamp,
//! its meters — as it renders it, and the sound comes out later by everything
//! buffered behind the render: the output ring and the device (the CLI, which
//! measures it), or whatever an embedding host holds (Kodi banks seconds of it,
//! and only Kodi knows how much, so it reports it through `heard_us`).
//!
//! The engine holds nothing back. It says where each block starts
//! ([`PLAYOUT_BLOCK`](osc_contract::PLAYOUT_BLOCK)) and where the listener is
//! ([`PLAYOUT_HEARD`](osc_contract::PLAYOUT_HEARD)), and a client that wants to
//! follow the sound queues the messages itself; one that wants them early
//! ignores both. Until the first heard position there is nothing to compare a
//! block with, so no marker is sent and the stream is exactly what it was.

use rosc::{OscMessage, OscPacket, OscType};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use super::OscSender;
use runtime_control::osc_contract;

/// The heard position goes out at most this often. A client extrapolates
/// between two of them, so a faster report only costs packets.
const HEARD_INTERVAL_MS: u64 = 20;

/// No block has been marked: the next stream message marks its block, even
/// one at position 0.
const UNMARKED: u64 = u64::MAX;

pub(crate) struct PlayoutMarks {
    /// A heard position has been published: blocks are worth marking.
    active: AtomicBool,
    /// Start of the block being rendered — what the stream messages sent now
    /// describe.
    block: AtomicU64,
    /// The block the last marker named.
    marked: AtomicU64,
    /// When the last heard position went out, in ms since `epoch`, plus one so
    /// that 0 means never.
    heard_sent_ms: AtomicU64,
    epoch: Instant,
}

impl PlayoutMarks {
    pub(crate) fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            block: AtomicU64::new(0),
            marked: AtomicU64::new(UNMARKED),
            heard_sent_ms: AtomicU64::new(0),
            epoch: Instant::now(),
        }
    }

    /// The block a marker must name before the next stream message, if that
    /// message is the first of a new block.
    fn block_to_mark(&self) -> Option<u64> {
        if !self.active.load(Ordering::Relaxed) {
            return None;
        }
        let block = self.block.load(Ordering::Relaxed);
        (self.marked.swap(block, Ordering::Relaxed) != block).then_some(block)
    }

    /// Whether a heard position reported `now_ms` after `epoch` goes out.
    fn heard_due(&self, now_ms: u64) -> bool {
        self.active.store(true, Ordering::Relaxed);
        let last = self.heard_sent_ms.load(Ordering::Relaxed);
        if last != 0 && now_ms + 1 < last + HEARD_INTERVAL_MS {
            return false;
        }
        self.heard_sent_ms.store(now_ms + 1, Ordering::Relaxed);
        true
    }
}

impl OscSender {
    /// The stream messages sent from now on describe the block starting at
    /// sample `pos`. Cheap enough for every block: a store, and nothing is
    /// sent until a stream message actually goes out.
    pub fn render_at(&self, pos: u64) {
        self.playout.block.store(pos, Ordering::Relaxed);
    }

    /// The listener is hearing sample `pos` of the timeline [`render_at`]
    /// counts, which plays at `rate` samples a second. Published at most every
    /// [`HEARD_INTERVAL_MS`]; the first one turns the block markers on.
    ///
    /// [`render_at`]: Self::render_at
    pub fn send_heard(&self, pos: u64, rate: u32) {
        let now_ms = self.playout.epoch.elapsed().as_millis() as u64;
        if !self.playout.heard_due(now_ms) || !self.has_osc_clients() {
            return;
        }
        let packet = OscPacket::Message(OscMessage {
            addr: osc_contract::PLAYOUT_HEARD.to_string(),
            args: vec![
                OscType::Long(pos.min(i64::MAX as u64) as i64),
                OscType::Int(rate.min(i32::MAX as u32) as i32),
            ],
        });
        if let Ok(bytes) = rosc::encoder::encode(&packet) {
            self.send_raw_to_all(&bytes);
        }
    }

    /// The timeline starts again (a reset): the next block is marked even if
    /// it starts where the last marked one did.
    pub fn rewind_playout(&self) {
        self.playout.block.store(0, Ordering::Relaxed);
        self.playout.marked.store(UNMARKED, Ordering::Relaxed);
    }

    /// Ahead of a stream message: name its block, the first time.
    pub(super) fn mark_block(&self) {
        let Some(block) = self.playout.block_to_mark() else {
            return;
        };
        let packet = OscPacket::Message(OscMessage {
            addr: osc_contract::PLAYOUT_BLOCK.to_string(),
            args: vec![OscType::Long(block.min(i64::MAX as u64) as i64)],
        });
        if let Ok(bytes) = rosc::encoder::encode(&packet) {
            self.send_raw_to_all(&bytes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_marked_before_a_heard_position() {
        let p = PlayoutMarks::new();
        p.block.store(480, Ordering::Relaxed);
        assert_eq!(p.block_to_mark(), None);
        assert!(p.heard_due(0));
        assert_eq!(p.block_to_mark(), Some(480));
    }

    #[test]
    fn a_block_is_marked_once_however_many_messages_describe_it() {
        let p = PlayoutMarks::new();
        assert!(p.heard_due(0));
        assert_eq!(p.block_to_mark(), Some(0), "position 0 is a block too");
        assert_eq!(p.block_to_mark(), None);
        p.block.store(960, Ordering::Relaxed);
        assert_eq!(p.block_to_mark(), Some(960));
        assert_eq!(p.block_to_mark(), None);
    }

    #[test]
    fn heard_positions_are_thinned_to_the_interval() {
        let p = PlayoutMarks::new();
        assert!(p.heard_due(100));
        assert!(!p.heard_due(100 + HEARD_INTERVAL_MS - 1));
        assert!(p.heard_due(100 + HEARD_INTERVAL_MS));
        assert!(p.heard_due(1_000));
    }

    #[test]
    fn heard_at_time_zero_is_not_mistaken_for_never() {
        let p = PlayoutMarks::new();
        assert!(p.heard_due(0));
        assert!(!p.heard_due(1), "0 ms was a real send");
    }
}
