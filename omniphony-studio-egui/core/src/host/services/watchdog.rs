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
use crate::host::commands::{SharedState, app, orender};
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
#[derive(Default)]
pub struct Watchdog {
    last_tick: Option<Instant>,
    disconnected_since: Option<Instant>,
}

/// What a tick decided.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// Nothing to do yet, or nothing it should do.
    Wait,
    /// Something else holds the renderer's port — an embedded renderer the
    /// link lost, most likely. Starting a second one would fight it.
    PortHeld,
    /// Start a renderer.
    Start,
}

/// What a tick decides on, read once, all of it relative to the tick's own
/// `now`. The two facts that ask the machine (a service unit, a port bind)
/// are closures: asked only once every cheaper rule has let the start
/// through, and in that order.
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
    cooling_down: bool,
    /// A child we started is still running: still starting up.
    child_running: bool,
    service_running: S,
    port_free: P,
}

/// The rules, in order. Each is a reason not to start a renderer now.
fn verdict<S: FnOnce() -> bool, P: FnOnce() -> bool>(facts: Facts<S, P>) -> Verdict {
    let goodbye_ready = facts.goodbye_for.is_some_and(|d| d >= GOODBYE_GRACE);
    if !goodbye_ready && facts.down_for < DEBOUNCE {
        return Verdict::Wait;
    }
    if !facts.auto_start || facts.target_is_loopback != Some(true) {
        return Verdict::Wait;
    }
    if facts.suppressed || facts.attempts >= MAX_ATTEMPTS || facts.cooling_down {
        return Verdict::Wait;
    }
    if facts.child_running {
        return Verdict::Wait;
    }
    // A service-managed renderer is someone else's responsibility.
    if (facts.service_running)() {
        return Verdict::Wait;
    }
    if !(facts.port_free)() {
        return Verdict::PortHeld;
    }
    Verdict::Start
}

impl Watchdog {
    pub fn tick(
        &mut self,
        state: &SharedState,
        now: Instant,
        stop: &crate::host::runtime::StopToken,
    ) -> Tick {
        if self
            .last_tick
            .is_some_and(|at| now.duration_since(at) < TICK)
        {
            return Tick {
                changed: false,
                next: self.last_tick.map(|at| at + TICK),
            };
        }
        self.last_tick = Some(now);
        let changed = reap_renderer_child(state, now);
        // Connected: nothing to do until the link could go stale, which is the
        // next moment this could have anything to say.
        if state.stats.connection_state() == crate::osc::ConnectionState::Connected {
            self.disconnected_since = None;
            {
                let mut wd = state.watchdog.lock().unwrap();
                wd.check_requested_at = None;
                wd.last_failure = None;
            }
            return Tick {
                changed,
                next: Some(now + TICK),
            };
        }
        let due = Tick {
            changed,
            next: Some(now + TICK),
        };
        let since = *self.disconnected_since.get_or_insert(now);
        // Re-read the configuration at check time, so a panel edit applies
        // without a restart.
        let cfg = state.config.snapshot();
        let target = *state.stats.target.lock().unwrap();
        let (goodbye_for, suppressed, attempts, cooling_down) = {
            let wd = state.watchdog.lock().unwrap();
            (
                wd.check_requested_at
                    .map(|at| now.saturating_duration_since(at)),
                wd.suppressed,
                wd.attempts,
                wd.cooldown_until.is_some_and(|at| at > now),
            )
        };
        let child_running = state
            .renderer_child
            .lock()
            .unwrap()
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)));
        let facts = Facts {
            down_for: now.saturating_duration_since(since),
            goodbye_for,
            auto_start: cfg.auto_start_renderer,
            target_is_loopback: target.map(|t| t.ip().is_loopback()),
            suppressed,
            attempts,
            cooling_down,
            child_running,
            service_running: orender::orender_service_running,
            port_free: || {
                target.is_some_and(|t| std::net::UdpSocket::bind(("0.0.0.0", t.port())).is_ok())
            },
        };
        match verdict(facts) {
            Verdict::Wait => return due,
            Verdict::PortHeld => {
                state.watchdog.lock().unwrap().check_requested_at = None;
                return due;
            }
            Verdict::Start => {}
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
            }
        }
        Tick {
            changed: true,
            next: Some(now + TICK),
        }
    }
}

/// How long the renderer has been quiet while still counting as connected, or
/// `None` when the link is down.
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
        let wd = state.watchdog.lock().unwrap();
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
            cooling_down: false,
            child_running: false,
            service_running: || false,
            port_free: || true,
        }
    }

    /// A probe a rule earlier in the order must have made unnecessary.
    fn not_asked() -> bool {
        panic!("a machine probe was asked although a cheaper rule decided")
    }

    #[test]
    fn everything_clear_starts_a_renderer() {
        assert_eq!(verdict(clear()), Verdict::Start);
    }

    #[test]
    fn a_renderer_restarting_on_its_own_gets_the_debounce() {
        let waiting = Facts {
            down_for: DEBOUNCE - Duration::from_millis(1),
            service_running: not_asked as fn() -> bool,
            port_free: not_asked as fn() -> bool,
            ..clear()
        };
        assert_eq!(verdict(waiting), Verdict::Wait);
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
            verdict(Facts {
                down_for: early,
                goodbye_for: Some(GOODBYE_GRACE - Duration::from_millis(1)),
                ..clear()
            }),
            Verdict::Wait
        );
    }

    #[test]
    fn each_reason_not_to_start_holds_on_its_own() {
        let cases: [(&str, Facts<fn() -> bool, fn() -> bool>); 7] = [
            (
                "auto-start off",
                Facts {
                    auto_start: false,
                    ..clear()
                },
            ),
            (
                "remote target",
                Facts {
                    target_is_loopback: Some(false),
                    ..clear()
                },
            ),
            (
                "no target",
                Facts {
                    target_is_loopback: None,
                    ..clear()
                },
            ),
            (
                "suppressed",
                Facts {
                    suppressed: true,
                    ..clear()
                },
            ),
            (
                "three failures",
                Facts {
                    attempts: MAX_ATTEMPTS,
                    ..clear()
                },
            ),
            (
                "cooling down",
                Facts {
                    cooling_down: true,
                    ..clear()
                },
            ),
            (
                "child starting",
                Facts {
                    child_running: true,
                    ..clear()
                },
            ),
        ];
        for (reason, facts) in cases {
            let facts = Facts {
                service_running: not_asked as fn() -> bool,
                port_free: not_asked as fn() -> bool,
                ..facts
            };
            assert_eq!(verdict(facts), Verdict::Wait, "{reason}");
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
        assert_eq!(verdict(service), Verdict::Wait);
        let held = Facts {
            port_free: (|| false) as fn() -> bool,
            ..clear()
        };
        assert_eq!(verdict(held), Verdict::PortHeld);
    }
}
