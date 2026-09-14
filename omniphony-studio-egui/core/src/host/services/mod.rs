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

pub mod auto_tune;
pub mod interests;
pub mod jobs;
pub mod meters;
pub mod object_test;
pub mod overlay;
pub mod speaker_test;
pub mod updates;
pub mod virtual_bed;
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
    pub object_test: object_test::ObjectTestSource,
    pub virtual_bed: virtual_bed::VirtualBed,
    pub auto_tune: auto_tune::AutoTuneRunner,
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
            self.object_test.tick(state, now),
            self.virtual_bed.tick(state, now),
            self.auto_tune.tick(state, now),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    /// A burst has to end on time with the clock parked on a distant deadline.
    ///
    /// That is the ordinary case, not a contrived one: with a renderer
    /// connected and quiet the watchdog is the only service still due, seven
    /// seconds out, and the loop is asleep until then. This ran for 6.8s of a
    /// 2s window before `speaker_test::start` woke the clock.
    #[test]
    fn a_burst_ends_on_time_while_the_clock_is_parked() {
        // `clock` is held to the end: dropping the sender would end the loop.
        let (clock, nudges) = ServiceClock::new();
        // Wired as the app wires it: what the host announces reaches the clock.
        let state = Arc::new(crate::host::commands::tests::state_with_waker({
            let clock = clock.clone();
            Arc::new(move || clock.nudge())
        }));
        // Connected and just heard from, which is what pushes the watchdog's
        // next visit out to its full window.
        state.stats.registered.store(true, Ordering::Relaxed);
        state.stats.packets.store(1, Ordering::Relaxed);
        state.stats.last_packet_ms.store(
            state.stats.start.elapsed().as_millis() as u64,
            Ordering::Relaxed,
        );

        spawn(state.clone(), Arc::new(|| {}), nudges).unwrap();
        // Let the loop take its pass and settle into the long sleep.
        std::thread::sleep(Duration::from_millis(200));

        let started = Instant::now();
        speaker_test::start(&state, 3, -8.0, "test_only".to_owned(), "burst");
        // Generous, because a loaded machine is allowed to be late; the
        // failure this guards against overshoots by seconds, not milliseconds.
        let slack = Duration::from_millis(1500);
        while state.inner.lock().unwrap().speaker_test.running.is_some() {
            assert!(
                started.elapsed() < speaker_test::BURST + slack,
                "burst outlived its {:?} window",
                speaker_test::BURST
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            started.elapsed() >= speaker_test::BURST,
            "burst was cut short of its {:?} window",
            speaker_test::BURST
        );
        drop(clock);
    }
}
