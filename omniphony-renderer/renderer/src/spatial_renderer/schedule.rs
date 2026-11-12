//! Object events that take effect on their own sample rather than on the
//! first sample of the block they arrive with.
//!
//! A bridge dates every event on its PCM ([`SpatialChannelEvent::sample_pos`]:
//! an offset from the first sample of the block it comes with). An event at
//! offset 0 is applied when it arrives, as every event used to be. An event
//! that falls later in its block, or in a later block — a TrueHD access unit is
//! 40 samples, and the metadata it carries is dated hundreds of samples ahead —
//! waits in its channel's [`EventSchedule`] on the renderer's sample clock, and
//! the ramp drivers start its move on its sample:
//!
//! - `RampMode::Sample` splits the object's pass over the block at each move
//!   ([`start_due_moves`] between two runs of the unchanged per-sample loop);
//! - the modes that evaluate the ramp once per block walk it to each move
//!   ([`walk_to`]) before evaluating it where they always did;
//! - `Interp` and `Off`, which ignore the ramp, take every move of the block
//!   at its start.
//!
//! The gain of a scheduled event is set at the start of the block holding its
//! sample: the gain slew is computed once per block, and the slew spreads any
//! step over [`GAIN_SLEW_SECS`](super::GAIN_SLEW_SECS) anyway. Its move and its
//! gain therefore land in the same block, as they did when both landed on the
//! arrival block.
//!
//! Nothing allocates: each channel holds at most [`SCHEDULE_CAPACITY`] events
//! in place, and a channel with none costs one compare per block.
//!
//! [`SpatialChannelEvent::sample_pos`]: super::SpatialChannelEvent::sample_pos

use crate::ramp_strategy::{ChannelRampState, RampContext, RampProgress, RampStrategy, RampTarget};

/// Events one channel holds for later samples. Object metadata states one
/// event per object per payload (TrueHD and E-AC-3 both carry one info block),
/// and a payload is dated at most a frame or two ahead of the PCM it comes
/// with, so a channel rarely holds more than two. When an event arrives on a
/// full schedule the earliest one is started at once
/// ([`EventSchedule::push`]).
pub(super) const SCHEDULE_CAPACITY: usize = 8;

/// One object event waiting for its sample.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct ScheduledEvent {
    /// The sample it takes effect on, on the renderer's clock.
    pub(super) at: u64,
    /// Its gain: set on the channel at the start of the block holding `at`,
    /// then taken (see the module docs).
    pub(super) gain_db: Option<f32>,
    pub(super) ramp_length: Option<u32>,
    pub(super) size: Option<[f32; 3]>,
    pub(super) position: Option<[f64; 3]>,
}

/// A channel's events waiting for their sample, in time order. Events due on
/// the same sample keep the order they arrived in.
#[derive(Clone, Debug, Default)]
pub(super) struct EventSchedule {
    events: [ScheduledEvent; SCHEDULE_CAPACITY],
    len: usize,
}

impl EventSchedule {
    #[inline]
    pub(super) fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The sample the next event is due on, `u64::MAX` when there is none.
    #[inline]
    pub(super) fn next_at(&self) -> u64 {
        if self.len == 0 {
            u64::MAX
        } else {
            self.events[0].at
        }
    }

    /// Hold `event` until its sample. On a full schedule, the earliest of the
    /// events held and `event` is handed back instead, for the caller to
    /// apply now.
    pub(super) fn push(&mut self, event: ScheduledEvent) -> Option<ScheduledEvent> {
        let mut early = None;
        if self.len == SCHEDULE_CAPACITY {
            if event.at < self.events[0].at {
                return Some(event);
            }
            early = Some(self.events[0]);
            self.events.copy_within(1..self.len, 0);
            self.len -= 1;
        }
        // After every event due on the same sample or before.
        let slot = self.events[..self.len]
            .iter()
            .position(|held| held.at > event.at)
            .unwrap_or(self.len);
        self.events.copy_within(slot..self.len, slot + 1);
        self.events[slot] = event;
        self.len += 1;
        early
    }

    /// The next event, removed, when it is due on or before `now`.
    #[inline]
    pub(super) fn pop_due(&mut self, now: u64) -> Option<ScheduledEvent> {
        if self.len == 0 || self.events[0].at > now {
            return None;
        }
        let event = self.events[0];
        self.events.copy_within(1..self.len, 0);
        self.len -= 1;
        Some(event)
    }

    /// The events due before `end`, in time order, to take their gains.
    pub(super) fn due_before_mut(&mut self, end: u64) -> impl Iterator<Item = &mut ScheduledEvent> {
        self.events[..self.len]
            .iter_mut()
            .take_while(move |event| event.at < end)
    }
}

/// Apply what an object event says about its ramp, now: its ramp length, then
/// a new ramp toward its position, from where the object is, when the
/// position or the size changes — or the new size alone, when it states no
/// position. `at` is the sample the ramp starts on, on the renderer's clock,
/// handed to the strategy.
#[inline]
pub(super) fn apply_move<S: RampStrategy + ?Sized>(
    ramp: &mut ChannelRampState,
    ramp_length: Option<u32>,
    size: Option<[f32; 3]>,
    position: Option<[f64; 3]>,
    at: u64,
    strategy: &S,
    ctx: &RampContext,
) {
    if let Some(ramp_length) = ramp_length {
        ramp.ramp_length = ramp_length as u64;
    }
    // `None` = unchanged.
    let new_target_size = size.unwrap_or(ramp.target_size);
    let size_changed = ramp.target_size != new_target_size;
    if let Some(target_position) = position {
        if ramp.target_position != target_position || size_changed {
            let ramp_length = ramp.ramp_length;
            strategy.update_target(
                ramp,
                RampTarget {
                    position: target_position,
                    size: new_target_size,
                    ramp_length,
                },
                Some(at),
                ctx,
            );
        }
    } else if size_changed {
        ramp.target_size = new_target_size;
        if ramp.remaining_ramp_units.is_none() {
            ramp.current_size = new_target_size;
        }
    }
}

/// Start every scheduled move due on or before `now`, in order. The ramp
/// must stand on sample `now` with the position of the sample before it
/// committed, so each new ramp starts from where the object is.
#[inline]
pub(super) fn start_due_moves<S: RampStrategy + ?Sized>(
    ramp: &mut ChannelRampState,
    schedule: &mut EventSchedule,
    now: u64,
    strategy: &S,
    ctx: &RampContext,
) {
    while let Some(event) = schedule.pop_due(now) {
        apply_move(
            ramp,
            event.ramp_length,
            event.size,
            event.position,
            now,
            strategy,
            ctx,
        );
    }
}

/// Evaluate `ramp` where it stands.
#[inline]
pub(super) fn evaluate<S: RampStrategy + ?Sized>(
    ramp: &mut ChannelRampState,
    strategy: &S,
    ctx: &RampContext,
) {
    let progress = ramp.current_progress().unwrap_or(RampProgress {
        completed_units: 0,
        total_units: 0,
    });
    strategy.evaluate(ramp, progress, ctx);
}

/// Advance `ramp` from block sample `from` to block sample `to`
/// (`from <= to`), for the drivers that evaluate the ramp less often than
/// every sample, starting each move scheduled on a sample of `from..=to` on
/// that sample: the ramp is evaluated and committed on the sample before it,
/// so the move starts from where the object is there. A move due before
/// `from` starts on `from`. Leaves the ramp on sample `to`, to be evaluated
/// there; with nothing scheduled it is `advance_ramp(to - from)`.
///
/// `block_start` is block sample 0 on the renderer's clock.
#[inline]
pub(super) fn walk_to<S: RampStrategy + ?Sized>(
    ramp: &mut ChannelRampState,
    schedule: &mut EventSchedule,
    block_start: u64,
    from: usize,
    to: usize,
    strategy: &S,
    ctx: &RampContext,
) {
    let mut now = from;
    let last = block_start + to as u64;
    while schedule.next_at() <= last {
        let due = (schedule.next_at().saturating_sub(block_start) as usize).max(now);
        if due > now {
            ramp.advance_ramp((due - 1 - now) as u64);
            evaluate(ramp, strategy, ctx);
            ramp.commit_output_position();
            ramp.advance_ramp(1);
            now = due;
        }
        start_due_moves(ramp, schedule, block_start + now as u64, strategy, ctx);
    }
    ramp.advance_ramp((to - now) as u64);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(at: u64) -> ScheduledEvent {
        ScheduledEvent {
            at,
            ..ScheduledEvent::default()
        }
    }

    fn times(schedule: &mut EventSchedule) -> Vec<u64> {
        let mut out = Vec::new();
        while let Some(event) = schedule.pop_due(u64::MAX) {
            out.push(event.at);
        }
        out
    }

    #[test]
    fn events_come_out_in_time_order_and_ties_in_arrival_order() {
        let mut schedule = EventSchedule::default();
        assert_eq!(schedule.next_at(), u64::MAX);
        for (t, gain) in [(30, 1.0), (10, 2.0), (30, 3.0), (20, 4.0), (10, 5.0)] {
            let event = ScheduledEvent {
                gain_db: Some(gain),
                ..at(t)
            };
            assert_eq!(schedule.push(event), None);
        }
        assert_eq!(schedule.next_at(), 10);
        assert_eq!(schedule.pop_due(9), None);
        let order: Vec<_> = std::iter::from_fn(|| schedule.pop_due(30))
            .map(|e| (e.at, e.gain_db.unwrap()))
            .collect();
        assert_eq!(
            order,
            [(10, 2.0), (10, 5.0), (20, 4.0), (30, 1.0), (30, 3.0)]
        );
        assert!(schedule.is_empty());
    }

    #[test]
    fn a_full_schedule_hands_the_earliest_event_back() {
        let mut schedule = EventSchedule::default();
        for t in 0..SCHEDULE_CAPACITY as u64 {
            assert_eq!(schedule.push(at(100 + t)), None);
        }
        // Later than all of them: the earliest held one comes back.
        assert_eq!(schedule.push(at(500)).map(|e| e.at), Some(100));
        // Earlier than all of them: it comes back itself.
        assert_eq!(schedule.push(at(50)).map(|e| e.at), Some(50));
        let mut want: Vec<u64> = (101..100 + SCHEDULE_CAPACITY as u64).collect();
        want.push(500);
        assert_eq!(times(&mut schedule), want);
    }

    #[test]
    fn the_gains_due_in_a_block_are_the_ones_before_its_end() {
        let mut schedule = EventSchedule::default();
        for t in [5, 40, 39, 80] {
            schedule.push(at(t));
        }
        let due: Vec<u64> = schedule.due_before_mut(40).map(|e| e.at).collect();
        assert_eq!(due, [5, 39]);
    }
}
