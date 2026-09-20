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
        let changed = reap_renderer_child(state);
        // Connected: nothing to do until the link could go stale, which is the
        // next moment this could have anything to say.
        if state.stats.connection_state() == crate::osc::ConnectionState::Connected {
            self.disconnected_since = None;
            state.watchdog.lock().unwrap().check_requested_at = None;
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
        let goodbye_ready = state
            .watchdog
            .lock()
            .unwrap()
            .check_requested_at
            .is_some_and(|at| at.elapsed() >= GOODBYE_GRACE);
        if !goodbye_ready && since.elapsed() < DEBOUNCE {
            return due;
        }
        // Re-read the configuration at check time, so a panel edit applies
        // without a restart.
        let cfg = state.config.snapshot();
        let Some(target) = *state.stats.target.lock().unwrap() else {
            return due;
        };
        if !cfg.auto_start_renderer || !target.ip().is_loopback() {
            return due;
        }
        {
            let wd = state.watchdog.lock().unwrap();
            if wd.suppressed || wd.attempts >= MAX_ATTEMPTS {
                return due;
            }
            if wd.cooldown_until.is_some_and(|at| at > now) {
                return due;
            }
        }
        // A tracked child that is still running is still starting up.
        if state
            .renderer_child
            .lock()
            .unwrap()
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
        {
            return due;
        }
        // A service-managed renderer is someone else's responsibility.
        if orender::orender_service_running() {
            return due;
        }
        // Something already holds the port — an mpv-embedded renderer we lost
        // contact with, most likely. Starting a second one would fight it.
        if std::net::UdpSocket::bind(("0.0.0.0", target.port())).is_err() {
            state.watchdog.lock().unwrap().check_requested_at = None;
            return due;
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
fn reap_renderer_child(state: &SharedState) -> bool {
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
        wd.last_spawn_at.is_some_and(|at| at.elapsed() < FAST_FAIL)
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
    let attempts = {
        let mut wd = state.watchdog.lock().unwrap();
        wd.attempts += 1;
        wd.cooldown_until = Some(Instant::now() + COOLDOWN);
        wd.attempts
    };
    app::push_log(
        state,
        "warn",
        "orender",
        format!(
            "orender exited within {}s of starting: {status}",
            FAST_FAIL.as_secs()
        ),
    );
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
