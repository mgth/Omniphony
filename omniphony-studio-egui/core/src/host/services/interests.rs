//! What the view is showing, kept alive by the core.
//!
//! Three subscriptions work the same way: the renderer only sends while it is
//! asked to, and forgets when a client goes quiet. The view says what it wants
//! to see — the gain tables under its volumes, a warm input chain while a test
//! pane is open, the diagnostics behind its plot — and the core does the
//! asking, the re-asking and the releasing.
//!
//! It used to be the view doing all three on every frame, which meant a folded
//! panel stopped renewing what it had asked for, and a window that stopped
//! drawing stopped renewing everything.

use std::time::{Duration, Instant};

use super::Tick;
use crate::host::commands::{SharedState, diag, gain};

/// `GAINTABLE_REPAIR`: a subscription is restated this often, so a renderer
/// that restarted starts sending again without the view touching anything.
const GAINTABLE_REPAIR: Duration = Duration::from_secs(5);
/// `IDLE_FEED_REARM`: the arm expires renderer-side, so it is renewed.
const IDLE_FEED_REARM: Duration = Duration::from_secs(120);
/// `DIAG_KEEPALIVE`: the diagnostics publication is restated at this rate, and
/// the rate goes with it — a renderer that restarted came back on its default.
const DIAG_KEEPALIVE: Duration = Duration::from_secs(1);

/// Who is asking for the idle feed. Two things do, so it is a set rather than
/// a flag: closing one must not take the feed away from the other.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FeedClient {
    SpeakerTest,
    ObjectTest,
}

/// What the view has declared. It is the view's business, so it lives with the
/// view's other state on the model, and the services below read it.
#[derive(Default, Debug, Clone)]
pub struct Interests {
    /// The gain tables under what the volumes are drawing.
    pub gain_tables: Vec<i64>,
    /// Who wants the input chain kept warm.
    pub idle_feed: Vec<FeedClient>,
    /// Whether the diagnostics plot is on screen, and at what rate it wants
    /// the telemetry.
    pub diagnostics: Option<f32>,
}

/// The gain tables the volumes are drawing.
pub fn set_gain_tables_wanted(state: &SharedState, targets: &[i64]) {
    let mut live = state.inner.lock().unwrap();
    if live.interests.gain_tables != targets {
        live.interests.gain_tables = targets.to_vec();
    }
}

/// Ask for, or release, the warm input chain.
pub fn set_idle_feed_wanted(state: &SharedState, client: FeedClient, wanted: bool) {
    let mut live = state.inner.lock().unwrap();
    let held = live.interests.idle_feed.contains(&client);
    match (wanted, held) {
        (true, false) => live.interests.idle_feed.push(client),
        (false, true) => live.interests.idle_feed.retain(|c| *c != client),
        _ => {}
    }
}

/// The diagnostics plot is on screen, at this publish rate, or is not.
pub fn set_diagnostics_wanted(state: &SharedState, rate_hz: Option<f32>) {
    state.inner.lock().unwrap().interests.diagnostics = rate_hz;
}

/// The gain-table subscription, negotiated by version and repaired on a
/// heartbeat.
#[derive(Default)]
pub struct GainTables {
    subscribed: Vec<i64>,
    last_subscribe: Option<Instant>,
}

impl GainTables {
    pub fn tick(&mut self, state: &SharedState, now: Instant) -> Tick {
        // Nothing to subscribe to without a renderer to ask.
        if state.stats.target.lock().unwrap().is_none() {
            return Tick::idle();
        }
        let wanted = state.inner.lock().unwrap().interests.gain_tables.clone();
        if wanted.is_empty() {
            if self.subscribed.is_empty() {
                return Tick::idle();
            }
            diag::unsubscribe_speaker_gaintable(state);
            self.subscribed.clear();
            self.last_subscribe = None;
            return Tick::idle();
        }
        let due = self
            .last_subscribe
            .is_none_or(|at| now.duration_since(at) >= GAINTABLE_REPAIR);
        if wanted != self.subscribed || due {
            let versions: Vec<(i64, i32)> = {
                let live = state.inner.lock().unwrap();
                wanted
                    .iter()
                    .map(|t| {
                        (
                            *t,
                            live.gain_tables
                                .get(t)
                                .map(|g| g.version() as i32)
                                .unwrap_or(0)
                                .max(0),
                        )
                    })
                    .collect()
            };
            for (target, have_version) in versions {
                diag::subscribe_speaker_gaintable(state, have_version, target as i32);
            }
            self.subscribed = wanted;
            self.last_subscribe = Some(now);
        }
        Tick {
            changed: false,
            next: self.last_subscribe.map(|at| at + GAINTABLE_REPAIR),
        }
    }
}

/// The warm input chain, armed while something wants it.
#[derive(Default)]
pub struct IdleFeed {
    armed_at: Option<Instant>,
}

impl IdleFeed {
    pub fn tick(&mut self, state: &SharedState, now: Instant) -> Tick {
        let wanted = !state.inner.lock().unwrap().interests.idle_feed.is_empty();
        match (wanted, self.armed_at) {
            (true, None) => {
                gain::control_speaker_test_idle_feed(state, true);
                self.armed_at = Some(now);
            }
            (true, Some(at)) if now.duration_since(at) >= IDLE_FEED_REARM => {
                gain::control_speaker_test_idle_feed(state, true);
                self.armed_at = Some(now);
            }
            (false, Some(_)) => {
                gain::control_speaker_test_idle_feed(state, false);
                self.armed_at = None;
            }
            _ => {}
        }
        Tick {
            changed: false,
            next: self.armed_at.map(|at| at + IDLE_FEED_REARM),
        }
    }
}

/// The diagnostics publication, held open while the plot is on screen.
#[derive(Default)]
pub struct Diagnostics {
    keepalive_at: Option<Instant>,
}

impl Diagnostics {
    pub fn tick(&mut self, state: &SharedState, now: Instant) -> Tick {
        let rate = state.inner.lock().unwrap().interests.diagnostics;
        let Some(rate) = rate else {
            if self.keepalive_at.take().is_some() {
                diag::control_diag_publication_enabled(state, 0);
            }
            return Tick::idle();
        };
        if self
            .keepalive_at
            .is_none_or(|at| now.duration_since(at) >= DIAG_KEEPALIVE)
        {
            diag::control_diag_publication_enabled(state, 1);
            diag::control_diag_rate_hz(state, rate);
            self.keepalive_at = Some(now);
        }
        Tick {
            changed: false,
            next: self.keepalive_at.map(|at| at + DIAG_KEEPALIVE),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_idle_feed_is_held_by_whoever_still_wants_it() {
        let state = crate::host::commands::tests::state();
        let mut feed = IdleFeed::default();
        set_idle_feed_wanted(&state, FeedClient::SpeakerTest, true);
        set_idle_feed_wanted(&state, FeedClient::ObjectTest, true);
        let now = Instant::now();
        feed.tick(&state, now);
        assert!(feed.armed_at.is_some());

        // One of the two letting go leaves it armed for the other.
        set_idle_feed_wanted(&state, FeedClient::SpeakerTest, false);
        feed.tick(&state, now);
        assert!(feed.armed_at.is_some());

        set_idle_feed_wanted(&state, FeedClient::ObjectTest, false);
        feed.tick(&state, now);
        assert!(feed.armed_at.is_none());
    }

    #[test]
    fn the_diagnostics_publication_is_released_when_the_plot_goes_away() {
        let state = crate::host::commands::tests::state();
        let mut diagnostics = Diagnostics::default();
        set_diagnostics_wanted(&state, Some(30.0));
        let tick = diagnostics.tick(&state, Instant::now());
        assert!(tick.next.is_some());
        set_diagnostics_wanted(&state, None);
        let tick = diagnostics.tick(&state, Instant::now());
        assert!(tick.next.is_none() && diagnostics.keepalive_at.is_none());
    }
}
