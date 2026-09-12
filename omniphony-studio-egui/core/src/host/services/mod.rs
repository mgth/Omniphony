//! What the Studio does between events, on a clock of its own.
//!
//! A UI draws when something happens; an application has to keep working when
//! nothing does. Meters fall, a test signal stops itself, a watchdog restarts
//! a renderer that went away. All of that used to run after the frame, so it
//! stopped when the frames did — with the renderer silent and the pointer
//! still, or with the window minimised, nothing advanced at all.
//!
//! Each service takes `now` rather than reading the clock, so its tests drive
//! time, and says when it wants to be looked at again. The clock sleeps until
//! then, or until something nudges it, so an idle Studio wakes for nothing.

pub mod interests;
pub mod meters;
pub mod overlay;
pub mod speaker_test;
pub mod watchdog;

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Instant;

use crate::host::commands::SharedState;
use crate::osc::Waker;

/// What a service did and when it wants to run again.
pub struct Tick {
    /// The model changed, so whoever draws it should.
    pub changed: bool,
    /// When this service is next due. `None` means it has nothing pending.
    pub next: Option<Instant>,
}

impl Tick {
    /// A pass that changed nothing and wants nothing.
    pub fn idle() -> Self {
        Self {
            changed: false,
            next: None,
        }
    }
}

/// The services, and the earliest deadline among them.
#[derive(Default)]
pub struct Services {
    pub meters: meters::Meters,
    pub speaker_test: speaker_test::SpeakerTest,
    pub watchdog: watchdog::Watchdog,
    pub gain_tables: interests::GainTables,
    pub idle_feed: interests::IdleFeed,
    pub diagnostics: interests::Diagnostics,
    pub overlay: overlay::MpvOverlay,
}

impl Services {
    /// Run everything that is due, and say when the next thing is.
    pub fn tick(&mut self, state: &SharedState, now: Instant) -> Tick {
        let mut changed = false;
        let mut next: Option<Instant> = None;
        for tick in [
            self.meters.tick(state, now),
            self.speaker_test.tick(state, now),
            self.watchdog.tick(state, now),
            self.gain_tables.tick(state, now),
            self.idle_feed.tick(state, now),
            self.diagnostics.tick(state, now),
            self.overlay.tick(state, now),
        ] {
            changed |= tick.changed;
            next = match (next, tick.next) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
        Tick { changed, next }
    }
}

/// A handle on the clock: nudge it when something it watches may have changed
/// (a packet applied, an interest declared) and it runs its services again.
#[derive(Clone)]
pub struct ServiceClock {
    nudge: Sender<()>,
}

impl ServiceClock {
    /// Make the handle before the thread: the listener is given it as part of
    /// its waker, and the thread starts once the host state exists.
    pub fn new() -> (Self, Receiver<()>) {
        let (nudge, rx) = mpsc::channel();
        (Self { nudge }, rx)
    }

    /// Ask for a pass. Cheap and never blocks; a dead clock is ignored,
    /// because nothing here is worth failing a UI action for.
    pub fn nudge(&self) {
        let _ = self.nudge.send(());
    }
}

/// Run the services until the handle is dropped. `waker` is called after a
/// pass that changed the model, so a falling meter is shown without anything
/// else having to happen.
pub fn spawn(
    state: Arc<SharedState>,
    waker: Waker,
    nudges: Receiver<()>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("studio-services".into())
        .spawn(move || {
            let mut services = Services::default();
            loop {
                let tick = services.tick(&state, Instant::now());
                if tick.changed {
                    waker();
                }
                let wait = tick
                    .next
                    .map(|at| at.saturating_duration_since(Instant::now()));
                let waited = match wait {
                    Some(wait) => nudges.recv_timeout(wait),
                    // Nothing pending: sleep until something happens.
                    None => nudges.recv().map_err(|_| RecvTimeoutError::Disconnected),
                };
                if matches!(waited, Err(RecvTimeoutError::Disconnected)) {
                    return;
                }
            }
        })
}
