//! Whether the state this client holds is the engine's, by the state
//! generation (osc-contract `STATE_GENERATION`).
//!
//! The engine counts the control-plane state it broadcasts and sends the count
//! with it: each single update carries the next one, a snapshot closes on the
//! current one, and the heartbeat acknowledgement repeats it. A datagram lost
//! on the way shows up as a count that skips, or as an acknowledgement ahead
//! of what the client holds, and the client asks for the snapshot again
//! instead of staying stale until the next change.
//!
//! An engine older than contract revision 1 sends no generation: nothing is
//! ever held, so nothing is ever asked.

use std::time::{Duration, Instant};

/// How long a refresh is given to arrive before it is asked again: the reply
/// can be lost like anything else.
const REFRESH_RETRY: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
pub struct StateSync {
    /// The generation of the state the model holds: set by a snapshot, moved
    /// by each update that follows it in sequence.
    held: Option<i32>,
    /// Something went missing since the last snapshot.
    stale: bool,
    last_request: Option<Instant>,
}

impl StateSync {
    /// A `/state/generation` received with the state it versions.
    pub fn on_generation(&mut self, generation: i32, full: bool) {
        if full {
            self.held = Some(generation);
            self.stale = false;
            return;
        }
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

    /// The generation a heartbeat acknowledgement reports, when it has one.
    ///
    /// It can run ahead of an update still on its way from another engine
    /// thread; that costs one snapshot, which is the price of noticing the
    /// last update of a burst going missing.
    pub fn on_ack(&mut self, generation: i32) {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synced(generation: i32) -> StateSync {
        let mut sync = StateSync::default();
        sync.on_generation(generation, true);
        sync
    }

    #[test]
    fn updates_in_sequence_keep_the_state_current() {
        let mut sync = synced(7);
        sync.on_generation(8, false);
        sync.on_generation(9, false);
        sync.on_ack(9);
        assert!(!sync.is_stale());
        assert!(!sync.refresh_due(Instant::now()));
    }

    #[test]
    fn a_skipped_update_asks_for_the_snapshot_until_one_arrives() {
        let mut sync = synced(7);
        sync.on_generation(9, false);
        assert!(sync.is_stale());
        let now = Instant::now();
        assert!(sync.refresh_due(now));
        assert!(!sync.refresh_due(now), "not twice while the reply is due");
        assert!(
            sync.refresh_due(now + REFRESH_RETRY),
            "asked again when lost"
        );
        sync.on_generation(10, true);
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
    fn the_count_wraps() {
        let mut sync = synced(i32::MAX);
        sync.on_generation(i32::MIN, false);
        assert!(!sync.is_stale());
    }

    /// An engine that sends no generation, and a client still waiting for
    /// its first snapshot, never ask.
    #[test]
    fn nothing_is_asked_before_a_snapshot_set_the_generation() {
        let mut sync = StateSync::default();
        sync.on_generation(5, false);
        sync.on_ack(9);
        assert!(!sync.refresh_due(Instant::now()));
    }
}
