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
//! ([`PLAYOUT_BLOCK`](runtime_control::osc_contract::PLAYOUT_BLOCK)) and where the listener is
//! ([`PLAYOUT_HEARD`](runtime_control::osc_contract::PLAYOUT_HEARD)), and a client that wants to
//! follow the sound queues the messages itself; one that wants them early
//! ignores both. Until the first heard position there is nothing to compare a
//! block with, so no marker is sent and the stream is exactly what it was.
//!
//! The render path only says which block it is on and where the listener is;
//! the markers themselves are decided and sent on the telemetry thread
//! ([`super::telemetry`]), with the stream messages they precede.

use super::OscSender;
use super::telemetry::{Block, Event};

impl OscSender {
    /// The stream messages sent from now on describe the block starting at
    /// sample `pos`. Cheap enough for every block: a store, and nothing is
    /// sent until a stream message actually goes out. Also where what a full
    /// queue held back is pushed again, so it goes out even when no other
    /// message follows it.
    pub fn render_at(&mut self, pos: u64) {
        self.telemetry.retry_held();
        self.telemetry.block.pos = pos;
    }

    /// The listener is hearing sample `pos` of the timeline [`render_at`]
    /// counts, which plays at `rate` samples a second. Published at most every
    /// [`HEARD_INTERVAL_MS`](super::telemetry::HEARD_INTERVAL_MS); the first
    /// one turns the block markers on.
    ///
    /// [`render_at`]: Self::render_at
    pub fn send_heard(&mut self, pos: u64, rate: u32) {
        if self.telemetry.heard.due_now() {
            self.telemetry.push(Event::Heard { pos, rate });
        }
    }

    /// The timeline starts again (a reset): the next block is marked even if
    /// it starts where the last marked one did.
    pub fn rewind_playout(&mut self) {
        self.telemetry.block = Block {
            restarts: self.telemetry.block.restarts.wrapping_add(1),
            pos: 0,
        };
    }
}
