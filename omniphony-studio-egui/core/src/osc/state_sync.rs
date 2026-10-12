//! Whether the state this client holds is the engine's, by the state
//! generation (osc-contract `STATE_GENERATION`).
//!
//! The engine counts the control-plane state it publishes and sends the count
//! with it: each update carries the next one, each datagram of a snapshot the
//! current one with its index and the snapshot's datagram count, and the
//! heartbeat acknowledgement repeats it. A datagram lost on the way shows up
//! as a count that skips, a snapshot with a part missing, or an
//! acknowledgement ahead of what the client holds, and the client asks for the
//! snapshot again instead of staying stale until the next change.
//!
//! An engine older than contract revision 1 sends no generation: nothing is
//! ever held, so nothing is ever asked.

use std::time::{Duration, Instant};

/// How long a refresh is given to arrive before it is asked again: the reply
/// can be lost like anything else.
const REFRESH_RETRY: Duration = Duration::from_secs(1);

/// More datagrams than any snapshot splits into (each carries up to 65 kB): a
/// count past it is not a snapshot this client will wait for.
const MAX_SNAPSHOT_PARTS: u32 = 1024;

#[derive(Debug, Default)]
pub struct StateSync {
    /// The generation of the state the model holds: set by a snapshot, moved
    /// by each update that follows it in sequence.
    held: Option<i32>,
    /// A snapshot whose parts are still arriving.
    pending: Option<PendingSnapshot>,
    /// Something went missing since the last whole snapshot.
    stale: bool,
    last_request: Option<Instant>,
}

#[derive(Debug)]
struct PendingSnapshot {
    generation: i32,
    seen: Vec<bool>,
}

impl StateSync {
    /// A state update's generation (`full = 0`).
    pub fn on_update(&mut self, generation: i32) {
        // A snapshot still missing parts when the next publication arrives
        // has lost them.
        self.abandon_pending();
        // Before the first snapshot there is nothing to be behind of: the
        // listener asks for that one until it is complete.
        let Some(held) = self.held else {
            return;
        };
        if held.wrapping_add(1) != generation {
            self.stale = true;
        }
        self.held = Some(generation);
    }

    /// One datagram of a snapshot (`full = 1`): part `part` of `parts`.
    ///
    /// The snapshot makes the state current only once every part of it is
    /// in: losing the first and receiving the last is a partial snapshot, and
    /// its `snapshot_complete` says nothing about the parts before it.
    pub fn on_snapshot_part(&mut self, generation: i32, part: u32, parts: u32) {
        if parts == 0 || part >= parts || parts > MAX_SNAPSHOT_PARTS {
            return;
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.generation != generation || p.seen.len() != parts as usize)
        {
            self.abandon_pending();
        }
        let pending = self.pending.get_or_insert_with(|| PendingSnapshot {
            generation,
            seen: vec![false; parts as usize],
        });
        pending.seen[part as usize] = true;
        self.held = Some(generation);
        if pending.seen.iter().all(|&seen| seen) {
            self.pending = None;
            self.stale = false;
        } else if part + 1 == parts {
            // The last part, with an earlier one missing: datagrams from one
            // sender arrive in order, so the missing one is lost.
            self.pending = None;
            self.stale = true;
        }
    }

    /// The generation a heartbeat acknowledgement reports, when it has one.
    ///
    /// The engine reads it under the lock its publications hold, so the ack
    /// leaves after everything published before it: a snapshot still missing
    /// parts by then has lost them, and a count ahead of the held one is an
    /// update that never came.
    pub fn on_ack(&mut self, generation: i32) {
        self.abandon_pending();
        if self.held.is_some_and(|held| held != generation) {
            self.stale = true;
        }
    }

    /// Whether to ask for the snapshot now, recording that it was.
    pub fn refresh_due(&mut self, now: Instant) -> bool {
        if !self.stale
            || self
                .last_request
                .is_some_and(|last| now.duration_since(last) < REFRESH_RETRY)
        {
            return false;
        }
        self.last_request = Some(now);
        true
    }

    pub fn is_stale(&self) -> bool {
        self.stale
    }

    fn abandon_pending(&mut self) {
        if self.pending.take().is_some() {
            self.stale = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synced(generation: i32) -> StateSync {
        let mut sync = StateSync::default();
        sync.on_snapshot_part(generation, 0, 1);
        sync
    }

    #[test]
    fn updates_in_sequence_keep_the_state_current() {
        let mut sync = synced(7);
        sync.on_update(8);
        sync.on_update(9);
        sync.on_ack(9);
        assert!(!sync.is_stale());
        assert!(!sync.refresh_due(Instant::now()));
    }

    #[test]
    fn a_skipped_update_asks_for_the_snapshot_until_one_arrives() {
        let mut sync = synced(7);
        sync.on_update(9);
        assert!(sync.is_stale());
        let now = Instant::now();
        assert!(sync.refresh_due(now));
        assert!(!sync.refresh_due(now), "not twice while the reply is due");
        assert!(
            sync.refresh_due(now + REFRESH_RETRY),
            "asked again when lost"
        );
        sync.on_snapshot_part(10, 0, 1);
        assert!(!sync.refresh_due(now + REFRESH_RETRY * 3));
    }

    /// The last update of a burst has nothing after it to show the gap: the
    /// acknowledgement does.
    #[test]
    fn an_ack_ahead_of_the_held_state_marks_it_stale() {
        let mut sync = synced(7);
        sync.on_ack(8);
        assert!(sync.is_stale());
    }

    #[test]
    fn a_snapshot_is_current_once_every_part_is_in() {
        let mut sync = synced(3);
        sync.on_update(5);
        assert!(sync.is_stale());
        sync.on_snapshot_part(6, 0, 3);
        sync.on_snapshot_part(6, 1, 3);
        assert!(sync.is_stale(), "not current halfway");
        sync.on_snapshot_part(6, 2, 3);
        assert!(!sync.is_stale());
        sync.on_ack(6);
        assert!(!sync.is_stale());
    }

    /// The review's case: the first part of a two-part snapshot lost, the
    /// last one received, then the acknowledgement with that generation.
    #[test]
    fn a_snapshot_missing_its_first_part_is_not_taken_for_whole() {
        let mut sync = StateSync::default();
        sync.on_snapshot_part(4, 1, 2);
        sync.on_ack(4);
        assert!(sync.is_stale());
        assert!(sync.refresh_due(Instant::now()));
    }

    /// The last part lost: the acknowledgement that follows finds the
    /// snapshot still waiting for it.
    #[test]
    fn a_snapshot_missing_its_last_part_is_noticed_at_the_ack() {
        let mut sync = synced(1);
        sync.on_snapshot_part(2, 0, 2);
        assert!(
            !sync.refresh_due(Instant::now()),
            "nothing asked mid-snapshot"
        );
        sync.on_ack(2);
        assert!(sync.is_stale());
    }

    #[test]
    fn a_snapshot_cut_short_by_the_next_publication_is_stale() {
        let mut sync = synced(1);
        sync.on_snapshot_part(2, 0, 2);
        sync.on_update(3);
        assert!(sync.is_stale());
    }

    #[test]
    fn the_count_wraps() {
        let mut sync = synced(i32::MAX);
        sync.on_update(i32::MIN);
        assert!(!sync.is_stale());
    }

    /// An engine that sends no generation, and a client still waiting for
    /// its first snapshot, never ask.
    #[test]
    fn nothing_is_asked_before_a_snapshot_set_the_generation() {
        let mut sync = StateSync::default();
        sync.on_update(5);
        sync.on_ack(9);
        assert!(!sync.refresh_due(Instant::now()));
    }

    #[test]
    fn a_malformed_part_is_ignored() {
        let mut sync = synced(1);
        sync.on_snapshot_part(2, 3, 2);
        sync.on_snapshot_part(2, 0, 0);
        sync.on_snapshot_part(2, 0, MAX_SNAPSHOT_PARTS + 1);
        sync.on_ack(1);
        assert!(!sync.is_stale());
    }
}
