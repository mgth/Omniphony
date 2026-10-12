//! PipeWire plumbing shared by the output writer and the live-input sinks
//! (`audio_input` builds on this crate): connecting a main loop to the daemon,
//! taking a bounded registry snapshot, and reading the owner of a client.

use anyhow::{Result, anyhow};
use pipewire as pw;
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// A main loop connected to the PipeWire daemon. The context is kept only to
/// hold the connection open; drop the whole struct to disconnect.
pub struct MainLoopConnection {
    pub mainloop: pw::main_loop::MainLoopRc,
    pub context: pw::context::ContextRc,
    pub core: pw::core::CoreRc,
}

/// `pw::init()`, then a main loop, a context and a core connection on it.
pub fn connect_main_loop() -> Result<MainLoopConnection> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| anyhow!("Failed to create PipeWire main loop: {e:?}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|e| anyhow!("Failed to create PipeWire context: {e:?}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| anyhow!("Failed to connect to PipeWire core: {e:?}"))?;
    Ok(MainLoopConnection {
        mainloop,
        context,
        core,
    })
}

/// Take one registry snapshot on `mainloop`: bind the registry, feed every
/// announced global to `on_global`, and return once the daemon has answered
/// a core sync queued after the bind — i.e. once every existing global has
/// been announced. `Ok(false)` when the daemon did not answer within
/// `timeout` (what was announced so far has still been delivered), so a
/// wedged daemon can never hang the caller.
pub fn registry_snapshot<F>(
    mainloop: &pw::main_loop::MainLoopRc,
    core: &pw::core::CoreRc,
    timeout: Duration,
    on_global: F,
) -> Result<bool>
where
    F: Fn(&pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>) + 'static,
{
    let registry = core
        .get_registry()
        .map_err(|e| anyhow!("Failed to get PipeWire registry: {e:?}"))?;
    // Queued after the registry bind, so its `done` lands after every
    // existing global has been announced.
    let pending = core
        .sync(0)
        .map_err(|e| anyhow!("PipeWire sync failed: {e:?}"))?;

    let done = Rc::new(Cell::new(false));
    let done_for_core = Rc::clone(&done);
    let _core_listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE && seq == pending {
                done_for_core.set(true);
            }
        })
        .register();
    let _registry_listener = registry.add_listener_local().global(on_global).register();

    let deadline = Instant::now() + timeout;
    while !done.get() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        let _ = mainloop
            .loop_()
            .iterate(remaining.min(Duration::from_millis(50)));
    }
    Ok(true)
}

/// A client in the registry. The pid prefers `pipewire.sec.pid`, which the
/// daemon fills from the socket credentials, over the client-declared
/// `application.process.id`; the binary prefers the declared
/// `application.process.binary` over `application.name`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientEntry {
    pub id: u32,
    pub pid: Option<u32>,
    pub binary: Option<String>,
}

/// A property value, trimmed, `None` when absent or blank.
pub fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// Reads a registry `Client` global.
pub fn client_from_props<'a>(id: u32, get: impl Fn(&str) -> Option<&'a str>) -> ClientEntry {
    let pid = non_empty(get(*pw::keys::SEC_PID))
        .or_else(|| non_empty(get(*pw::keys::APP_PROCESS_ID)))
        .and_then(|v| v.parse().ok());
    let binary = non_empty(get(*pw::keys::APP_PROCESS_BINARY))
        .or_else(|| non_empty(get(*pw::keys::APP_NAME)))
        .map(str::to_owned);
    ClientEntry { id, pid, binary }
}

/// The pid owning the node with `client_id`, when that client is known.
pub fn owner_pid(client_id: Option<u32>, clients: &[ClientEntry]) -> Option<u32> {
    let client_id = client_id?;
    clients
        .iter()
        .find(|client| client.id == client_id)
        .and_then(|client| client.pid)
}
