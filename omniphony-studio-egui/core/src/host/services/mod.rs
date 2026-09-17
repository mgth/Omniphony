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
pub mod operations;
pub mod overlay;
pub mod speaker_test;
pub mod updates;
pub mod virtual_bed;
pub mod watchdog;

use crate::host::runtime::Worker;
use std::sync::Arc;
use std::sync::Mutex;
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

/// Cloneable wake handle. Thread::unpark coalesces bursts into one token;
/// it has no unbounded notification queue and does not own the running state.
#[derive(Clone, Default)]
pub struct ServiceClock {
    thread: Arc<Mutex<Option<std::thread::Thread>>>,
}
impl ServiceClock {
    pub fn nudge(&self) {
        if let Some(thread) = &*self.thread.lock().unwrap() {
            thread.unpark();
        }
    }
}

pub struct ServiceRuntime {
    clock: Worker,
    watchdog: Worker,
}
impl ServiceRuntime {
    /// Stop periodic producers, then terminate audio operations immediately.
    /// A slow watchdog join must not extend a test tone's safety deadline.
    pub fn stop_audio_operations(&mut self, state: &SharedState) {
        self.watchdog.request_stop();
        self.clock.shutdown();
        speaker_test::stop(state);
        auto_tune::revert(state);
    }

    pub fn shutdown(&mut self) {
        self.clock.request_stop();
        self.watchdog.request_stop();
        self.clock.shutdown();
        self.watchdog.shutdown();
    }
}

/// The periodic watchdog has its own worker: DNS, configuration and service
/// manager calls must never delay the speaker-test safety clock.
pub fn spawn(
    state: Arc<SharedState>,
    waker: Waker,
    clock: ServiceClock,
) -> std::io::Result<ServiceRuntime> {
    let service_state = state.clone();
    let repaint = waker.clone();
    let clock = Worker::spawn("studio-services", move |stop| {
        *clock.thread.lock().unwrap() = Some(std::thread::current());
        let mut services = Services::default();
        while !stop.cancelled() {
            let tick = services.tick(&service_state, Instant::now());
            if tick.changed {
                repaint();
            }
            stop.wait(
                tick.next
                    .map(|at| at.saturating_duration_since(Instant::now())),
            );
        }
        *clock.thread.lock().unwrap() = None;
    })?;
    let watchdog = Worker::spawn("studio-watchdog", move |stop| {
        let mut watchdog = watchdog::Watchdog::default();
        while !stop.cancelled() {
            let tick = watchdog.tick(&state, Instant::now(), &stop);
            if tick.changed {
                waker();
            }
            stop.wait(
                tick.next
                    .map(|at| at.saturating_duration_since(Instant::now())),
            );
        }
    })?;
    Ok(ServiceRuntime { clock, watchdog })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    #[test]
    fn shutdown_sends_stop_before_joining_a_blocked_watchdog() {
        let mut state = crate::host::commands::tests::state();
        let (tx, commands) = std::sync::mpsc::channel();
        state.osc_tx = tx;
        let (release, wait) = std::sync::mpsc::channel::<()>();
        let mut runtime = ServiceRuntime {
            clock: Worker::spawn("test-clock", |stop| {
                while !stop.cancelled() {
                    stop.wait(None);
                }
            })
            .unwrap(),
            watchdog: Worker::spawn("test-watchdog", move |_| {
                let _ = wait.recv();
            })
            .unwrap(),
        };
        speaker_test::start(&state, 2, -8.0, "test_only".into(), "burst");
        commands.try_iter().for_each(drop);
        runtime.stop_audio_operations(&state);
        assert!(
            matches!(commands.try_recv().unwrap(), crate::osc::Control::Send { args, .. } if matches!(args.first(), Some(rosc::OscType::Int(-1))))
        );
        assert!(state.read().speaker_test.running.is_none());
        release.send(()).unwrap();
        runtime.shutdown();
    }

    /// A burst has to end on time with the clock parked on a distant deadline.
    ///
    /// That is the ordinary case, not a contrived one: with a renderer
    /// connected and quiet the watchdog is the only service still due, seven
    /// seconds out, and the loop is asleep until then. This ran for 6.8s of a
    /// 2s window before `speaker_test::start` woke the clock.
    #[test]
    fn a_burst_ends_on_time_while_the_clock_is_parked() {
        // `clock` is held to the end: dropping the sender would end the loop.
        let clock = ServiceClock::default();
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

        let mut runtime = spawn(state.clone(), Arc::new(|| {}), clock.clone()).unwrap();
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
        runtime.shutdown();
        assert_eq!(Arc::strong_count(&state), 1);
    }
}
