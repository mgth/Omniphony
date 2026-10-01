//! Show each block of audio when it is heard, not when it was rendered.
//!
//! The renderer describes a block — its object frame, its timestamp, its
//! meters — as it renders it, and the sound comes out later by everything
//! buffered behind the render: the output ring and the device, or seconds of
//! it behind a host such as Kodi. Drawn as it arrives, the scene runs that far
//! ahead of what the listener hears.
//!
//! The renderer holds nothing back: it names the block each stream message is
//! about (`/omniphony/playout/block`) and says where the listener is
//! (`/omniphony/playout/heard`). This queue keeps the stream messages and hands
//! them on once the listener reaches their block, extrapolating between two
//! heard positions with the local clock. Everything else — state, replies to
//! what the user just did — goes straight through: an edit must show at once,
//! even though the sound of it follows.
//!
//! Without a heard position (an engine that does not publish one, or the
//! user's switch off) nothing is held and the stream is drawn as it arrives,
//! as before.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use rosc::{OscMessage, OscType};

use crate::osc_contract;

/// How far past the last heard position the clock may carry the listener.
/// The renderer reports every ~20 ms while the audio plays; when the reports
/// stop the audio has stopped (a pause), and so should the scene.
const MAX_EXTRAPOLATION: Duration = Duration::from_millis(100);

/// A heard position older than this, when a new block arrives, means the
/// renderer no longer reports one (its output went somewhere it cannot
/// measure): what is held goes out and the stream is drawn as it comes. A
/// pause does not trip it, because a paused renderer sends no new block.
const HEARD_STALE: Duration = Duration::from_secs(2);

/// Held messages beyond this go out early, oldest first. Ten seconds of the
/// busiest stream is a few tens of thousands of messages; past that the
/// listener is not catching up, and holding more would only grow.
const MAX_HELD: usize = 50_000;

#[derive(Clone, Copy, Debug)]
struct Heard {
    pos: u64,
    rate: u32,
    at: Instant,
}

/// The queue between the socket and the model. Owned by the listener thread.
pub(crate) struct Playout {
    /// The user's switch ("follow the sound").
    enabled: bool,
    /// The block the stream messages arriving now describe.
    block: Option<u64>,
    heard: Option<Heard>,
    held: VecDeque<(u64, OscMessage)>,
}

/// What the listener should do with a message.
pub(crate) enum Offer {
    /// Apply it now.
    Apply,
    /// Taken: it is a playout message, or it waits for its block.
    Taken,
}

impl Playout {
    pub(crate) fn new(enabled: bool) -> Self {
        Self {
            enabled,
            block: None,
            heard: None,
            held: VecDeque::new(),
        }
    }

    /// Turn following the sound on or off. Off releases everything held on
    /// the next [`release_due`](Self::release_due).
    pub(crate) fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
    }

    /// Another renderer, or the same one starting over: nothing held is about
    /// its timeline.
    pub(crate) fn reset(&mut self) {
        self.block = None;
        self.heard = None;
        self.held.clear();
    }

    /// Offer one incoming message. `m` is cloned only when it is held.
    pub(crate) fn offer(&mut self, m: &OscMessage, now: Instant) -> Offer {
        if m.addr == osc_contract::PLAYOUT_BLOCK {
            if let Some(pos) = arg_u64(&m.args, 0) {
                self.on_block(pos, now);
            }
            return Offer::Taken;
        }
        if m.addr == osc_contract::PLAYOUT_HEARD {
            if let (Some(pos), Some(rate)) = (arg_u64(&m.args, 0), arg_u64(&m.args, 1)) {
                self.heard = Some(Heard {
                    pos,
                    rate: rate.clamp(1, u64::from(u32::MAX)) as u32,
                    at: now,
                });
            }
            return Offer::Taken;
        }
        if !describes_a_block(&m.addr) {
            return Offer::Apply;
        }
        let (Some(block), Some(heard)) = (self.block, self.listener(now)) else {
            return Offer::Apply;
        };
        // Already heard, and nothing ahead of it to overtake.
        if self.held.is_empty() && block <= heard {
            return Offer::Apply;
        }
        self.held.push_back((block, m.clone()));
        Offer::Taken
    }

    /// Hand every held message whose block the listener has reached to
    /// `apply`, in arrival order — all of them when nothing is followed any
    /// more.
    pub(crate) fn release_due(&mut self, now: Instant, mut apply: impl FnMut(OscMessage)) {
        let listener = self.listener(now);
        while let Some((block, _)) = self.held.front() {
            let due = match listener {
                Some(heard) => *block <= heard || self.held.len() > MAX_HELD,
                None => true,
            };
            if !due {
                break;
            }
            if let Some((_, m)) = self.held.pop_front() {
                apply(m);
            }
        }
    }

    /// How long until the oldest held message is due, if one is held.
    pub(crate) fn next_due_in(&self, now: Instant) -> Option<Duration> {
        let (block, _) = self.held.front()?;
        let Some(heard) = self.heard else {
            return Some(Duration::ZERO);
        };
        let Some(listener) = self.listener(now) else {
            return Some(Duration::ZERO);
        };
        let ahead = block.saturating_sub(listener);
        Some(Duration::from_secs_f64(
            ahead as f64 / f64::from(heard.rate),
        ))
    }

    /// How far the display runs behind the render, for the latency readout:
    /// the newest block against the listener.
    pub(crate) fn delay(&self, now: Instant) -> Option<Duration> {
        let block = self.block?;
        let heard = self.heard?;
        let listener = self.listener(now)?;
        Some(Duration::from_secs_f64(
            block.saturating_sub(listener) as f64 / f64::from(heard.rate),
        ))
    }

    fn on_block(&mut self, pos: u64, now: Instant) {
        // Back to an earlier position: a seek or a reset. What is held was
        // about audio nobody will hear.
        if self.block.is_some_and(|previous| pos < previous) {
            self.held.clear();
        }
        // Blocks keep coming and the heard position does not: the renderer
        // stopped measuring its output. Stop waiting for it.
        if self
            .heard
            .is_some_and(|heard| now.saturating_duration_since(heard.at) > HEARD_STALE)
        {
            self.heard = None;
        }
        self.block = Some(pos);
    }

    /// Where the listener is now, when the stream is being followed.
    fn listener(&self, now: Instant) -> Option<u64> {
        if !self.enabled {
            return None;
        }
        let heard = self.heard?;
        let since = now
            .saturating_duration_since(heard.at)
            .min(MAX_EXTRAPOLATION);
        Some(heard.pos + (since.as_secs_f64() * f64::from(heard.rate)) as u64)
    }
}

/// The messages that describe a block of audio: what the renderer sends while
/// it renders one (`OscSender::send_to_all` / `send_to_metering_clients` on
/// its side), so what must not be shown before it is heard.
fn describes_a_block(addr: &str) -> bool {
    addr == osc_contract::SPATIAL_FRAME
        || addr == osc_contract::TIMESTAMP
        || addr == osc_contract::BED_CONFIG
        || addr == osc_contract::STATE_OBJECT_TEST_POSITION
        || addr.starts_with(osc_contract::OBJECT_STREAM_PREFIX)
        || addr.starts_with(osc_contract::METER_PREFIX)
}

fn arg_u64(args: &[OscType], i: usize) -> Option<u64> {
    match args.get(i)? {
        OscType::Long(v) => u64::try_from(*v).ok(),
        OscType::Int(v) => u64::try_from(*v).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u64 = 48_000;

    fn msg(addr: &str) -> OscMessage {
        OscMessage {
            addr: addr.into(),
            args: Vec::new(),
        }
    }

    fn block(pos: u64) -> OscMessage {
        OscMessage {
            addr: osc_contract::PLAYOUT_BLOCK.into(),
            args: vec![OscType::Long(pos as i64)],
        }
    }

    fn heard(pos: u64) -> OscMessage {
        OscMessage {
            addr: osc_contract::PLAYOUT_HEARD.into(),
            args: vec![OscType::Long(pos as i64), OscType::Int(RATE as i32)],
        }
    }

    fn frame() -> OscMessage {
        msg(osc_contract::SPATIAL_FRAME)
    }

    fn applies(p: &mut Playout, m: &OscMessage, now: Instant) -> bool {
        matches!(p.offer(m, now), Offer::Apply)
    }

    fn released(p: &mut Playout, now: Instant) -> usize {
        let mut n = 0;
        p.release_due(now, |_| n += 1);
        n
    }

    #[test]
    fn without_a_heard_position_the_stream_goes_straight_through() {
        let t = Instant::now();
        let mut p = Playout::new(true);
        assert!(!applies(&mut p, &block(96_000), t), "markers are consumed");
        assert!(applies(&mut p, &frame(), t));
        assert_eq!(p.next_due_in(t), None);
    }

    #[test]
    fn a_block_waits_for_the_listener() {
        let t = Instant::now();
        let mut p = Playout::new(true);
        assert!(!applies(&mut p, &heard(0), t));
        assert!(!applies(&mut p, &block(RATE), t));
        assert!(!applies(&mut p, &frame(), t), "a second ahead: held");
        assert!(!applies(&mut p, &msg("/omniphony/meter/object/3"), t));
        assert_eq!(released(&mut p, t), 0);
        let wait = p.next_due_in(t).unwrap();
        assert!((wait.as_secs_f64() - 1.0).abs() < 1e-3, "{wait:?}");

        p.offer(&heard(RATE - 1), t);
        assert_eq!(released(&mut p, t), 0);
        p.offer(&heard(RATE), t);
        assert_eq!(released(&mut p, t), 2, "frame and meters, in order");
    }

    #[test]
    fn state_and_replies_are_never_held() {
        let t = Instant::now();
        let mut p = Playout::new(true);
        p.offer(&heard(0), t);
        p.offer(&block(RATE), t);
        assert!(!applies(&mut p, &frame(), t));
        assert!(applies(&mut p, &msg(osc_contract::STATE_LATENCY), t));
        assert!(applies(&mut p, &msg(osc_contract::HEARTBEAT_ACK), t));
    }

    #[test]
    fn nothing_overtakes_what_is_held() {
        let t = Instant::now();
        let mut p = Playout::new(true);
        p.offer(&heard(0), t);
        p.offer(&block(RATE), t);
        assert!(!applies(&mut p, &frame(), t));
        // The listener reaches the block before the queue is next drained: a
        // message arriving now is due too, but must not go before the one
        // already waiting.
        p.offer(&heard(RATE), t);
        assert!(!applies(&mut p, &frame(), t));
        assert_eq!(released(&mut p, t), 2);
        assert!(applies(&mut p, &frame(), t), "queue empty, block heard");
    }

    #[test]
    fn the_clock_carries_the_listener_between_reports_but_not_through_a_pause() {
        let t = Instant::now();
        let mut p = Playout::new(true);
        p.offer(&heard(0), t);
        p.offer(&block(RATE / 50), t); // 20 ms ahead
        p.offer(&frame(), t);
        assert_eq!(released(&mut p, t + Duration::from_millis(19)), 0);
        assert_eq!(released(&mut p, t + Duration::from_millis(21)), 1);

        p.offer(&block(RATE), t); // a second ahead, and the reports stop
        p.offer(&frame(), t);
        assert_eq!(released(&mut p, t + Duration::from_secs(5)), 0);
    }

    #[test]
    fn a_seek_drops_what_was_held() {
        let t = Instant::now();
        let mut p = Playout::new(true);
        p.offer(&heard(10 * RATE), t);
        p.offer(&block(11 * RATE), t);
        p.offer(&frame(), t);
        p.offer(&block(0), t);
        p.offer(&heard(0), t);
        assert_eq!(released(&mut p, t), 0, "the old timeline's frame is gone");
        assert_eq!(p.next_due_in(t), None);
    }

    #[test]
    fn switching_it_off_releases_everything() {
        let t = Instant::now();
        let mut p = Playout::new(true);
        p.offer(&heard(0), t);
        p.offer(&block(RATE), t);
        p.offer(&frame(), t);
        p.set_enabled(false);
        assert_eq!(released(&mut p, t), 1);
        assert!(applies(&mut p, &frame(), t));
    }

    #[test]
    fn a_renderer_that_stops_reporting_is_no_longer_waited_for() {
        let t = Instant::now();
        let mut p = Playout::new(true);
        p.offer(&heard(0), t);
        p.offer(&block(RATE), t);
        p.offer(&frame(), t);
        let later = t + HEARD_STALE + Duration::from_millis(1);
        p.offer(&block(2 * RATE), later);
        assert_eq!(released(&mut p, later), 1);
        assert!(applies(&mut p, &frame(), later));
    }

    #[test]
    fn the_delay_reads_the_newest_block_against_the_listener() {
        let t = Instant::now();
        let mut p = Playout::new(true);
        assert_eq!(p.delay(t), None);
        p.offer(&heard(RATE), t);
        p.offer(&block(3 * RATE), t);
        let d = p.delay(t).unwrap();
        assert!((d.as_secs_f64() - 2.0).abs() < 1e-3, "{d:?}");
    }
}
