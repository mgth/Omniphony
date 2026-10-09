//! The local-renderer auto-start watchdog.
//!
//! It is what makes "open Studio and it works" true on a machine where the
//! renderer is a separate process: when the link has been down long enough,
//! the configured host is this machine, and nothing else is already holding
//! the port, it starts one. Everything else in it is a rule against doing that
//! when it would be wrong or futile.
//!
//! It ran after the frame until now, which is the one moment it cannot count
//! on: a renderer that never came up sends nothing, so nothing repaints, so
//! the watchdog that would have started it never ran either.

use std::time::{Duration, Instant};

use super::Tick;
use crate::host::commands::{SharedState, WatchdogControl, app, orender};
use crate::i18n::t;

/// `WATCHDOG_INTERVAL`: how often the rules below are re-checked.
const TICK: Duration = Duration::from_secs(1);
/// `WATCHDOG_DISCONNECT_DEBOUNCE`: how long the link must be down first. A
/// renderer restarting on its own must be allowed to come back.
const DEBOUNCE: Duration = Duration::from_secs(6);
/// `WATCHDOG_GOODBYE_GRACE`: a renderer that said goodbye is not coming back,
/// so its port is free almost at once and the debounce is short-circuited.
const GOODBYE_GRACE: Duration = Duration::from_millis(500);
/// `WATCHDOG_FAST_FAIL_WINDOW`: a child that dies this soon after its spawn
/// did not start — it failed.
const FAST_FAIL: Duration = Duration::from_secs(5);
/// `WATCHDOG_COOLDOWN` and `WATCHDOG_MAX_ATTEMPTS`: back off, then give up
/// until something re-arms. Three failures in a row is a broken installation,
/// not bad luck, and a spawn loop would bury the reason in the log.
const COOLDOWN: Duration = Duration::from_secs(5);
const MAX_ATTEMPTS: u8 = 3;
/// How long a spawned renderer counts as starting up for the button. Past it,
/// a child that still has not answered is not "starting" any more, it is
/// stuck, and the button must not stay locked on it.
const STARTING_WINDOW: Duration = Duration::from_secs(20);
/// How often the button is redrawn while it animates: smooth enough for a
/// bar a couple of hundred pixels wide filling in seconds, and nothing at all
/// once it stops.
const ANIMATION_FRAME: Duration = Duration::from_millis(40);

#[derive(Default)]
pub struct Watchdog {
    /// When the next pass is due: a second after the last one, or sooner
    /// when a wait ends before that, so a start comes when its countdown
    /// fills and not up to a second later.
    next_at: Option<Instant>,
    disconnected_since: Option<Instant>,
    /// The last pass's machine probes refused a start during this outage: a
    /// service-managed renderer runs, or something holds the port. Remembered
    /// so a countdown is not promised on the strength of probes that are only
    /// asked once the wait is over.
    refused: bool,
    /// The renderer's goodbye the last pass saw, so a new one is acted on at
    /// once rather than at the next pass.
    goodbye: Option<Instant>,
}

/// What a tick decided.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// Nothing to do, or nothing it should do.
    Wait,
    /// Every rule lets a start through, but a wait is not over: the start
    /// comes in `left`, at the end of a wait `span` long in all.
    Pending { left: Duration, span: Duration },
    /// A service-managed renderer is someone else's responsibility.
    Managed,
    /// Something else holds the renderer's port — an embedded renderer the
    /// link lost, most likely. Starting a second one would fight it.
    PortHeld,
    /// Start a renderer.
    Start,
}

/// What a tick decides on, read once, all of it relative to the tick's own
/// `now`. The two facts that ask the machine (a service unit, a port bind)
/// are closures: asked only once every cheaper rule and every wait has let
/// the start through, and in that order.
struct Facts<S: FnOnce() -> bool, P: FnOnce() -> bool> {
    /// How long the link has been down.
    down_for: Duration,
    /// How long ago the renderer said goodbye, if it did.
    goodbye_for: Option<Duration>,
    auto_start: bool,
    /// Whether the target is this machine; `None` when there is no target.
    target_is_loopback: Option<bool>,
    suppressed: bool,
    attempts: u8,
    /// What is left of the cooldown after a failed start; zero when none.
    cooldown_left: Duration,
    /// A child we started is still running: still starting up.
    child_running: bool,
    /// The probes of an earlier pass of this outage refused a start.
    refused_before: bool,
    service_running: S,
    port_free: P,
}

/// The rules, in order. Each is a reason not to start a renderer now; the
/// waits come last among the cheap ones, so that the same rules also say
/// whether a renderer *will* start once they are over.
fn verdict<S: FnOnce() -> bool, P: FnOnce() -> bool>(facts: Facts<S, P>) -> Verdict {
    if !facts.auto_start || facts.target_is_loopback != Some(true) {
        return Verdict::Wait;
    }
    if facts.suppressed || facts.attempts >= MAX_ATTEMPTS {
        return Verdict::Wait;
    }
    if facts.child_running {
        return Verdict::Wait;
    }
    if let Some((left, span)) = wait_left(facts.down_for, facts.goodbye_for, facts.cooldown_left) {
        // The probes belong to the moment of the start: binding the port
        // while a renderer restarts on its own could be what stops it coming
        // back. Until then, what the last ones said stands.
        return if facts.refused_before {
            Verdict::Wait
        } else {
            Verdict::Pending { left, span }
        };
    }
    if (facts.service_running)() {
        return Verdict::Managed;
    }
    if !(facts.port_free)() {
        return Verdict::PortHeld;
    }
    Verdict::Start
}

/// What is left of the waits before a start, and how long that wait is in
/// all; `None` once they are over. Two waits: the link's — the debounce, or
/// the shorter grace after a goodbye, whichever ends first — and the cooldown
/// after a failed start. The one that ends last is the one that counts.
fn wait_left(
    down_for: Duration,
    goodbye_for: Option<Duration>,
    cooldown_left: Duration,
) -> Option<(Duration, Duration)> {
    let debounce = (DEBOUNCE.saturating_sub(down_for), DEBOUNCE);
    let link = match goodbye_for {
        Some(since) => {
            let grace = (GOODBYE_GRACE.saturating_sub(since), GOODBYE_GRACE);
            if grace.0 < debounce.0 {
                grace
            } else {
                debounce
            }
        }
        None => debounce,
    };
    let wait = if cooldown_left > link.0 {
        (cooldown_left, COOLDOWN)
    } else {
        link
    };
    (!wait.0.is_zero()).then_some(wait)
}

/// When the watchdog will start a renderer: the moment the wait began and
/// the moment it ends. Published by its tick in `WatchdogControl::countdown`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Countdown {
    from: Instant,
    due: Instant,
}

impl Countdown {
    fn fraction(self, now: Instant) -> f32 {
        let span = self.due.saturating_duration_since(self.from);
        if span.is_zero() {
            return 1.0;
        }
        (now.saturating_duration_since(self.from).as_secs_f32() / span.as_secs_f32())
            .clamp(0.0, 1.0)
    }
}

/// What the "Start the audio engine" button shows about a start it does not
/// have to be pressed for. Toolkit-free: the panel only draws it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum EngineStartProgress {
    /// Nothing will start on its own: the plain button.
    None,
    /// The watchdog starts the engine when `fraction` reaches 1.
    Countdown { fraction: f32 },
    /// An engine was started and has not answered yet. Starting another one
    /// now would only fight it.
    Starting,
}

impl EngineStartProgress {
    /// The state of the button now.
    ///
    /// Cheap enough for every frame the banner is drawn: two locks and one
    /// `try_wait`. The rules themselves are not run here — the machine probes
    /// among them are not per-frame questions — but read from what the
    /// watchdog's last pass published.
    pub fn of(state: &SharedState) -> Self {
        if state.stats.connection_state() == crate::osc::ConnectionState::Connected {
            return Self::None;
        }
        let child_running = orender::launched_renderer_running(state);
        let wd = state.watchdog.lock().unwrap();
        Self::at(&wd, child_running, Instant::now())
    }

    /// The state of the button at `now`, with the link down.
    fn at(wd: &WatchdogControl, child_running: bool, now: Instant) -> Self {
        let starting = child_running
            && wd.awaiting_answer
            && wd
                .last_spawn_at
                .is_some_and(|at| now.saturating_duration_since(at) < STARTING_WINDOW);
        if starting {
            return Self::Starting;
        }
        match wd.countdown {
            Some(countdown) => Self::Countdown {
                fraction: countdown.fraction(now),
            },
            None => Self::None,
        }
    }

    /// When the button wants drawing again: soon while it animates, never
    /// while it does not. What starts an animation is the watchdog's pass
    /// that published it, which wakes the UI itself.
    pub fn repaint_after(self) -> Option<Duration> {
        match self {
            Self::None => None,
            Self::Countdown { .. } | Self::Starting => Some(ANIMATION_FRAME),
        }
    }
}

impl Watchdog {
    pub fn tick(
        &mut self,
        state: &SharedState,
        now: Instant,
        stop: &crate::host::runtime::StopToken,
    ) -> Tick {
        let goodbye = state.stats.goodbye.at();
        let heard = goodbye.is_some() && goodbye != self.goodbye;
        self.goodbye = goodbye;
        if !heard && self.next_at.is_some_and(|at| now < at) {
            return Tick {
                changed: false,
                next: self.next_at,
            };
        }
        let mut next = now + TICK;
        self.next_at = Some(next);
        let changed = reap_renderer_child(state, now);
        // Connected: nothing to do until the link could go stale, which is the
        // next moment this could have anything to say.
        if state.stats.connection_state() == crate::osc::ConnectionState::Connected {
            self.disconnected_since = None;
            self.refused = false;
            let unpublished = {
                let mut wd = state.watchdog.lock().unwrap();
                wd.last_failure = None;
                wd.awaiting_answer = false;
                wd.countdown.take().is_some()
            };
            return Tick {
                changed: changed || unpublished,
                next: Some(next),
            };
        }
        let since = *self.disconnected_since.get_or_insert(now);
        // Re-read the configuration at check time, so a panel edit applies
        // without a restart.
        let cfg = state.config.snapshot();
        let target = *state.stats.target.lock().unwrap();
        let goodbye_for = goodbye.map(|at| now.saturating_duration_since(at));
        let (suppressed, attempts, cooldown_left) = {
            let wd = state.watchdog.lock().unwrap();
            (
                wd.suppressed,
                wd.attempts,
                wd.cooldown_until
                    .map_or(Duration::ZERO, |at| at.saturating_duration_since(now)),
            )
        };
        let child_running = orender::launched_renderer_running(state);
        let facts = Facts {
            down_for: now.saturating_duration_since(since),
            goodbye_for,
            auto_start: cfg.auto_start_renderer,
            target_is_loopback: target.map(|t| t.ip().is_loopback()),
            suppressed,
            attempts,
            cooldown_left,
            child_running,
            refused_before: self.refused,
            service_running: orender::orender_service_running,
            port_free: || {
                target.is_some_and(|t| std::net::UdpSocket::bind(("0.0.0.0", t.port())).is_ok())
            },
        };
        let verdict = verdict(facts);
        let countdown = match verdict {
            Verdict::Pending { left, span } => {
                let due = now + left;
                // Wake for the end of the wait, not a second past it.
                next = next.min(due);
                Some(Countdown {
                    from: due.checked_sub(span).unwrap_or(now),
                    due,
                })
            }
            _ => None,
        };
        self.next_at = Some(next);
        let published = {
            let mut wd = state.watchdog.lock().unwrap();
            let published = wd.countdown != countdown;
            wd.countdown = countdown;
            published
        };
        let due = Tick {
            changed: changed || published,
            next: Some(next),
        };
        match verdict {
            Verdict::Wait | Verdict::Pending { .. } => return due,
            Verdict::Managed => {
                self.refused = true;
                return due;
            }
            Verdict::PortHeld => {
                self.refused = true;
                // Its port was not freed after all: back to the debounce.
                if let Some(at) = goodbye {
                    state.stats.goodbye.forget_if(at);
                }
                return due;
            }
            Verdict::Start => self.refused = false,
        }
        if stop.cancelled() {
            return Tick::idle();
        }
        match orender::autostart_orender(&state.paths, state) {
            Ok(info) => {
                let command = info
                    .get("command")
                    .and_then(|c| c.as_str())
                    .unwrap_or_default()
                    .to_owned();
                app::push_log(
                    state,
                    "info",
                    "orender",
                    format!("{} {command}", t("log.orenderAutostartLaunched")),
                );
            }
            Err(error) => {
                let attempts = {
                    let mut wd = state.watchdog.lock().unwrap();
                    wd.attempts += 1;
                    wd.cooldown_until = Some(now + COOLDOWN);
                    wd.last_failure = Some(error.clone());
                    wd.attempts
                };
                app::push_log(state, "error", "orender", format!("orender: {error}"));
                // Only the streak is worth an alarm: one failed spawn on a busy
                // machine is not a broken installation.
                if attempts >= MAX_ATTEMPTS {
                    app::push_log(
                        state,
                        "error",
                        "orender",
                        t("log.orenderAutostartFailed").to_owned(),
                    );
                }
                // The cooldown starts now. Come straight back, so the next
                // pass publishes it as the button's countdown — or nothing,
                // after the last attempt — rather than a second from now.
                self.next_at = None;
                next = now;
            }
        }
        Tick {
            changed: true,
            next: Some(next),
        }
    }
}

/// Reap the tracked child whatever the connection state, so a renderer that
/// died is noticed even while another one answers. Says whether anything was
/// written to the log.
fn reap_renderer_child(state: &SharedState, now: Instant) -> bool {
    let exited = {
        let mut slot = state.renderer_child.lock().unwrap();
        match slot.as_mut().map(|child| child.try_wait()) {
            Some(Ok(Some(status))) => {
                *slot = None;
                Some(status)
            }
            _ => None,
        }
    };
    let Some(status) = exited else { return false };
    let fast_fail = {
        let mut wd = state.watchdog.lock().unwrap();
        // Whatever it was, it is not starting any more.
        wd.awaiting_answer = false;
        wd.last_spawn_at
            .is_some_and(|at| now.saturating_duration_since(at) < FAST_FAIL)
    };
    if !fast_fail {
        // Yielded to an mpv-embedded renderer, or stopped on purpose.
        state.watchdog.lock().unwrap().attempts = 0;
        app::push_log(
            state,
            "info",
            "orender",
            format!("orender exited: {status}"),
        );
        return true;
    }
    let message = format!(
        "orender exited within {}s of starting: {status}",
        FAST_FAIL.as_secs()
    );
    let attempts = {
        let mut wd = state.watchdog.lock().unwrap();
        wd.attempts += 1;
        wd.cooldown_until = Some(now + COOLDOWN);
        wd.last_failure = Some(message.clone());
        wd.attempts
    };
    app::push_log(state, "warn", "orender", message);
    if attempts >= MAX_ATTEMPTS {
        app::push_log(
            state,
            "error",
            "orender",
            t("log.orenderAutostartFailed").to_owned(),
        );
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    /// Everything clear: the link down past the debounce, auto-start on, the
    /// target this machine, no streak, nothing running, the port free.
    fn clear() -> Facts<fn() -> bool, fn() -> bool> {
        Facts {
            down_for: DEBOUNCE,
            goodbye_for: None,
            auto_start: true,
            target_is_loopback: Some(true),
            suppressed: false,
            attempts: 0,
            cooldown_left: Duration::ZERO,
            child_running: false,
            refused_before: false,
            service_running: || false,
            port_free: || true,
        }
    }

    /// A probe a rule earlier in the order must have made unnecessary.
    fn not_asked() -> bool {
        panic!("a machine probe was asked although a cheaper rule decided")
    }

    /// `facts` with both machine probes forbidden.
    fn unprobed(facts: Facts<fn() -> bool, fn() -> bool>) -> Facts<fn() -> bool, fn() -> bool> {
        Facts {
            service_running: not_asked as fn() -> bool,
            port_free: not_asked as fn() -> bool,
            ..facts
        }
    }

    #[test]
    fn everything_clear_starts_a_renderer() {
        assert_eq!(verdict(clear()), Verdict::Start);
    }

    #[test]
    fn a_renderer_restarting_on_its_own_gets_the_debounce() {
        let waiting = unprobed(Facts {
            down_for: DEBOUNCE - Duration::from_millis(1),
            ..clear()
        });
        assert_eq!(
            verdict(waiting),
            Verdict::Pending {
                left: Duration::from_millis(1),
                span: DEBOUNCE
            }
        );
    }

    #[test]
    fn the_debounce_counts_down_without_asking_the_machine() {
        for down in [0, 1, 2, 5] {
            let down_for = Duration::from_secs(down);
            assert_eq!(
                verdict(unprobed(Facts {
                    down_for,
                    ..clear()
                })),
                Verdict::Pending {
                    left: DEBOUNCE - down_for,
                    span: DEBOUNCE
                },
                "{down}s down"
            );
        }
    }

    #[test]
    fn a_goodbye_cuts_the_debounce_short_after_its_grace() {
        let early = Duration::from_secs(1);
        assert_eq!(
            verdict(Facts {
                down_for: early,
                goodbye_for: Some(GOODBYE_GRACE),
                ..clear()
            }),
            Verdict::Start
        );
        assert_eq!(
            verdict(unprobed(Facts {
                down_for: early,
                goodbye_for: Some(GOODBYE_GRACE - Duration::from_millis(1)),
                ..clear()
            })),
            Verdict::Pending {
                left: Duration::from_millis(1),
                span: GOODBYE_GRACE
            }
        );
        // A goodbye heard late does not hold back a debounce that ends first.
        assert_eq!(
            verdict(unprobed(Facts {
                down_for: DEBOUNCE - Duration::from_millis(100),
                goodbye_for: Some(Duration::ZERO),
                ..clear()
            })),
            Verdict::Pending {
                left: Duration::from_millis(100),
                span: DEBOUNCE
            }
        );
    }

    #[test]
    fn a_cooldown_is_a_wait_of_its_own() {
        // Past the debounce, the cooldown is what is left.
        assert_eq!(
            verdict(unprobed(Facts {
                attempts: 1,
                cooldown_left: Duration::from_secs(3),
                ..clear()
            })),
            Verdict::Pending {
                left: Duration::from_secs(3),
                span: COOLDOWN
            }
        );
        // Inside a debounce that ends later, the debounce is.
        assert_eq!(
            verdict(unprobed(Facts {
                down_for: Duration::from_secs(1),
                cooldown_left: Duration::from_secs(2),
                ..clear()
            })),
            Verdict::Pending {
                left: DEBOUNCE - Duration::from_secs(1),
                span: DEBOUNCE
            }
        );
    }

    #[test]
    fn each_reason_not_to_start_holds_on_its_own() {
        type Probes = Facts<fn() -> bool, fn() -> bool>;
        let cases: [(&str, fn(&mut Probes)); 6] = [
            ("auto-start off", |f| f.auto_start = false),
            ("remote target", |f| f.target_is_loopback = Some(false)),
            ("no target", |f| f.target_is_loopback = None),
            ("suppressed", |f| f.suppressed = true),
            ("three failures", |f| f.attempts = MAX_ATTEMPTS),
            ("child starting", |f| f.child_running = true),
        ];
        for (reason, rule_out) in cases {
            // Due now, and early in the debounce: neither starts nor counts
            // down.
            for down_for in [DEBOUNCE, Duration::from_secs(1)] {
                let mut facts = unprobed(Facts {
                    down_for,
                    ..clear()
                });
                rule_out(&mut facts);
                assert_eq!(verdict(facts), Verdict::Wait, "{reason}, {down_for:?}");
            }
        }
        assert_eq!(
            verdict(Facts {
                attempts: MAX_ATTEMPTS - 1,
                ..clear()
            }),
            Verdict::Start,
            "two failures still allow a third try"
        );
    }

    #[test]
    fn a_service_or_a_port_holder_is_left_alone() {
        let service = Facts {
            service_running: (|| true) as fn() -> bool,
            port_free: not_asked as fn() -> bool,
            ..clear()
        };
        assert_eq!(verdict(service), Verdict::Managed);
        let held = Facts {
            port_free: (|| false) as fn() -> bool,
            ..clear()
        };
        assert_eq!(verdict(held), Verdict::PortHeld);
    }

    #[test]
    fn a_refusal_learned_earlier_promises_no_countdown() {
        let refused = unprobed(Facts {
            attempts: 1,
            cooldown_left: Duration::from_secs(2),
            refused_before: true,
            ..clear()
        });
        assert_eq!(verdict(refused), Verdict::Wait);
        // Once the wait is over the probes are asked again: the port may have
        // been freed since.
        assert_eq!(
            verdict(Facts {
                refused_before: true,
                ..clear()
            }),
            Verdict::Start
        );
    }

    fn control(countdown: Option<Countdown>) -> WatchdogControl {
        WatchdogControl {
            countdown,
            ..Default::default()
        }
    }

    #[test]
    fn the_countdown_fills_from_its_start_to_its_due_time() {
        let from = Instant::now();
        let wd = control(Some(Countdown {
            from,
            due: from + DEBOUNCE,
        }));
        let at =
            |secs: f32| EngineStartProgress::at(&wd, false, from + Duration::from_secs_f32(secs));
        assert_eq!(at(0.0), EngineStartProgress::Countdown { fraction: 0.0 });
        assert_eq!(at(3.0), EngineStartProgress::Countdown { fraction: 0.5 });
        assert_eq!(at(6.0), EngineStartProgress::Countdown { fraction: 1.0 });
        // Between the due time and the pass that starts the engine, it stays full.
        assert_eq!(at(6.5), EngineStartProgress::Countdown { fraction: 1.0 });
        assert_eq!(
            EngineStartProgress::at(&control(None), false, from),
            EngineStartProgress::None
        );
    }

    #[test]
    fn a_spawned_child_is_starting_until_it_answers_dies_or_overstays() {
        let spawned = Instant::now();
        let countdown = Some(Countdown {
            from: spawned,
            due: spawned + DEBOUNCE,
        });
        let wd = WatchdogControl {
            last_spawn_at: Some(spawned),
            awaiting_answer: true,
            countdown,
            ..Default::default()
        };
        let soon = spawned + Duration::from_secs(2);
        // Started, even over a countdown not yet withdrawn: a click during
        // the countdown launches at once, before the next pass.
        assert_eq!(
            EngineStartProgress::at(&wd, true, soon),
            EngineStartProgress::Starting
        );
        // The child exited: not starting any more.
        assert_ne!(
            EngineStartProgress::at(&wd, false, soon),
            EngineStartProgress::Starting
        );
        // It answered once since: a running child is not "starting".
        let answered = WatchdogControl {
            awaiting_answer: false,
            ..control(None)
        };
        assert_eq!(
            EngineStartProgress::at(&answered, true, soon),
            EngineStartProgress::None
        );
        // Silent for too long: stuck, and the button is released.
        let stuck = WatchdogControl {
            last_spawn_at: Some(spawned),
            awaiting_answer: true,
            ..Default::default()
        };
        assert_eq!(
            EngineStartProgress::at(&stuck, true, spawned + STARTING_WINDOW),
            EngineStartProgress::None
        );
    }

    #[test]
    fn only_an_animating_button_asks_to_be_redrawn() {
        assert_eq!(EngineStartProgress::None.repaint_after(), None);
        assert_eq!(
            EngineStartProgress::Countdown { fraction: 0.3 }.repaint_after(),
            Some(ANIMATION_FRAME)
        );
        assert_eq!(
            EngineStartProgress::Starting.repaint_after(),
            Some(ANIMATION_FRAME)
        );
    }

    /// A host whose link is down towards a renderer on this machine, with
    /// auto-start on: what a first run looks like.
    fn local_target_down() -> SharedState {
        let state = crate::host::commands::tests::state();
        *state.stats.target.lock().unwrap() = Some("127.0.0.1:9".parse().unwrap());
        assert!(state.config.snapshot().auto_start_renderer);
        state
    }

    /// What the button would show at `now`, from what the tick published.
    fn shown(state: &SharedState, now: Instant) -> EngineStartProgress {
        let wd = state.watchdog.lock().unwrap();
        EngineStartProgress::at(&wd, false, now)
    }

    /// A tick at `t0 + secs`, with a stop token that keeps any start from
    /// spawning should a test ever let one through.
    fn tick_at(watchdog: &mut Watchdog, state: &SharedState, t0: Instant, secs: u64) -> Tick {
        watchdog.tick(
            state,
            t0 + Duration::from_secs(secs),
            &crate::host::runtime::StopToken::cancelled_for_test(),
        )
    }

    #[test]
    fn a_tick_publishes_the_countdown_and_wakes_the_ui_only_when_it_changes() {
        let state = local_target_down();
        let mut watchdog = Watchdog::default();
        let t0 = Instant::now();
        let first = tick_at(&mut watchdog, &state, t0, 0);
        assert!(first.changed, "the countdown appearing must wake the UI");
        assert_eq!(
            shown(&state, t0 + Duration::from_secs(3)),
            EngineStartProgress::Countdown { fraction: 0.5 }
        );
        // The same countdown again: nothing new to draw, the button's own
        // repaint requests carry the animation.
        for secs in 1..=4 {
            let tick = tick_at(&mut watchdog, &state, t0, secs);
            assert!(!tick.changed, "{secs}s");
            assert_eq!(tick.next, Some(t0 + Duration::from_secs(secs + 1)));
        }
        // The last pass before the start wakes for its due time exactly.
        let last = watchdog.tick(
            &state,
            t0 + Duration::from_millis(5_500),
            &crate::host::runtime::StopToken::cancelled_for_test(),
        );
        assert_eq!(last.next, Some(t0 + DEBOUNCE));
        // A renderer answers: the countdown goes, and the UI hears of it.
        state.stats.registered.store(true, Ordering::Relaxed);
        let connected = watchdog.tick(
            &state,
            t0 + Duration::from_millis(5_600),
            &crate::host::runtime::StopToken::cancelled_for_test(),
        );
        // Throttled: the pass is not due before the start's due time.
        assert!(!connected.changed);
        let connected = tick_at(&mut watchdog, &state, t0, 6);
        assert!(connected.changed);
        assert_eq!(
            shown(&state, t0 + Duration::from_secs(6)),
            EngineStartProgress::None
        );
        assert_eq!(EngineStartProgress::of(&state), EngineStartProgress::None);
        assert!(!tick_at(&mut watchdog, &state, t0, 7).changed);
    }

    #[test]
    fn no_countdown_where_the_watchdog_would_not_start() {
        let cases: [(&str, fn(&SharedState)); 5] = [
            ("auto-start off", |state| {
                state
                    .config
                    .update(|config| config.auto_start_renderer = false)
                    .unwrap()
            }),
            ("remote target", |state| {
                *state.stats.target.lock().unwrap() = Some("192.0.2.1:9".parse().unwrap())
            }),
            ("suppressed", |state| {
                state.watchdog.lock().unwrap().suppressed = true
            }),
            ("max attempts", |state| {
                state.watchdog.lock().unwrap().attempts = MAX_ATTEMPTS
            }),
            ("refused before", |_| {}),
        ];
        for (reason, setup) in cases {
            let state = local_target_down();
            setup(&state);
            let mut watchdog = Watchdog {
                refused: reason == "refused before",
                ..Default::default()
            };
            let t0 = Instant::now();
            let tick = tick_at(&mut watchdog, &state, t0, 0);
            assert!(!tick.changed, "{reason}");
            assert_eq!(shown(&state, t0), EngineStartProgress::None, "{reason}");
        }
    }

    #[test]
    fn a_cooldown_between_attempts_counts_down_to_the_next_one() {
        let state = local_target_down();
        let mut watchdog = Watchdog::default();
        let t0 = Instant::now();
        // The link has been down past the debounce; a start failed 2 s ago.
        watchdog.disconnected_since = Some(t0);
        {
            let mut wd = state.watchdog.lock().unwrap();
            wd.attempts = 1;
            wd.cooldown_until = Some(t0 + Duration::from_secs(11));
        }
        let tick = tick_at(&mut watchdog, &state, t0, 8);
        assert!(tick.changed);
        assert_eq!(
            shown(&state, t0 + Duration::from_secs(8)),
            EngineStartProgress::Countdown { fraction: 0.4 }
        );
    }

    /// A pass at `t0 + millis`.
    fn tick_at_ms(watchdog: &mut Watchdog, state: &SharedState, t0: Instant, millis: u64) -> Tick {
        watchdog.tick(
            state,
            t0 + Duration::from_millis(millis),
            &crate::host::runtime::StopToken::cancelled_for_test(),
        )
    }

    /// A renderer on this machine answers, and the watchdog's pass that saw
    /// it is not due again for a second.
    fn connected_then_passed(t0: Instant) -> (SharedState, Watchdog) {
        let state = local_target_down();
        state.stats.registered.store(true, Ordering::Relaxed);
        let mut watchdog = Watchdog::default();
        let tick = tick_at(&mut watchdog, &state, t0, 0);
        assert_eq!(tick.next, Some(t0 + TICK));
        (state, watchdog)
    }

    #[test]
    fn a_goodbye_restarts_the_renderer_after_the_grace_not_the_debounce() {
        let t0 = Instant::now();
        let (state, mut watchdog) = connected_then_passed(t0);
        // The renderer says goodbye 200 ms into the pass's second, which is
        // what the listener does with `STATE_SHUTDOWN`.
        let goodbye = t0 + Duration::from_millis(200);
        state.stats.registered.store(false, Ordering::Relaxed);
        state.stats.goodbye.heard(goodbye);
        // Woken for it, the pass is not throttled: the grace counts down from
        // the goodbye, and the next pass is due when it ends — the one that
        // starts the renderer.
        let heard = tick_at_ms(&mut watchdog, &state, t0, 200);
        assert!(heard.changed, "the countdown appearing must wake the UI");
        assert_eq!(heard.next, Some(goodbye + GOODBYE_GRACE));
        assert_eq!(
            state.watchdog.lock().unwrap().countdown,
            Some(Countdown {
                from: goodbye,
                due: goodbye + GOODBYE_GRACE
            })
        );
        assert_eq!(
            shown(&state, goodbye + GOODBYE_GRACE / 2),
            EngineStartProgress::Countdown { fraction: 0.5 }
        );
        // The same goodbye does not keep lifting the throttle.
        let again = tick_at_ms(&mut watchdog, &state, t0, 300);
        assert!(!again.changed);
        assert_eq!(again.next, Some(goodbye + GOODBYE_GRACE));
    }

    #[test]
    fn without_a_goodbye_a_lost_link_waits_out_the_debounce() {
        let t0 = Instant::now();
        let (state, mut watchdog) = connected_then_passed(t0);
        state.stats.registered.store(false, Ordering::Relaxed);
        // Nothing heard: the pass keeps its cadence.
        let early = tick_at_ms(&mut watchdog, &state, t0, 200);
        assert!(!early.changed);
        assert_eq!(early.next, Some(t0 + TICK));
        // The link is seen down at the next pass, and the debounce counts
        // from there.
        let seen = t0 + TICK;
        assert!(tick_at(&mut watchdog, &state, t0, 1).changed);
        let debounce = Some(Countdown {
            from: seen,
            due: seen + DEBOUNCE,
        });
        assert_eq!(state.watchdog.lock().unwrap().countdown, debounce);
        for secs in 2..=6 {
            let tick = tick_at(&mut watchdog, &state, t0, secs);
            assert!(!tick.changed, "{secs}s");
            assert_eq!(state.watchdog.lock().unwrap().countdown, debounce);
        }
    }

    #[test]
    fn a_launch_shows_starting_until_the_link_comes_up() {
        let state = local_target_down();
        state.watchdog.lock().unwrap().awaiting_answer = true;
        state.watchdog.lock().unwrap().last_spawn_at = Some(Instant::now());
        // No child here, so `of` cannot see one running; `at` stands in for it.
        let wd = state.watchdog.lock().unwrap();
        assert_eq!(
            EngineStartProgress::at(&wd, true, Instant::now()),
            EngineStartProgress::Starting
        );
        drop(wd);
        let mut watchdog = Watchdog::default();
        state.stats.registered.store(true, Ordering::Relaxed);
        tick_at(&mut watchdog, &state, Instant::now(), 0);
        assert!(!state.watchdog.lock().unwrap().awaiting_answer);
    }
}
