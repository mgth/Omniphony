use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub(crate) struct OscClientState {
    pub(crate) last_seen: Option<Instant>,
    pub(crate) metering_enabled: bool,
    /// Whether this client wants `/omniphony/state/diag_*` updates. Decoupled
    /// from `metering_enabled` so a client can subscribe to diag traces
    /// without the audio-level meter bundle (and vice versa).
    pub(crate) diag_enabled: bool,
    /// Whether this client is subscribed to the precomputed speaker gain table.
    /// While subscribed, the renderer pushes a fresh table (chunked) on every
    /// topology rebuild — but only if the client's last-pushed version differs.
    pub(crate) gaintable_enabled: bool,
    /// Version (hash) of the gain table last pushed to this client, so a rebuild
    /// or a re-subscribe carrying the same version skips re-sending the data.
    pub(crate) gaintable_version: Option<u32>,
    /// Speaker index this client wants the gain table for (the heatmap shows one
    /// speaker; pushes are serialized per speaker). `None` until first subscribe.
    pub(crate) gaintable_speaker: Option<usize>,
}

pub(crate) struct OscClientRegistry {
    clients: Mutex<HashMap<SocketAddr, OscClientState>>,
    timeout: Duration,
}

impl OscClientRegistry {
    pub(crate) fn new(timeout: Duration) -> Self {
        Self {
            clients: Mutex::new(HashMap::new()),
            timeout,
        }
    }

    pub(crate) fn insert_permanent(&self, addr: SocketAddr) {
        self.clients.lock().unwrap().insert(
            addr,
            OscClientState {
                last_seen: None,
                metering_enabled: false,
                diag_enabled: false,
                gaintable_enabled: false,
                gaintable_version: None,
                gaintable_speaker: None,
            },
        );
    }

    pub(crate) fn register(&self, addr: SocketAddr) -> (bool, bool) {
        let mut clients = self.clients.lock().unwrap();
        let prev_state = clients.get(&addr).copied();
        let (
            metering_enabled,
            diag_enabled,
            gaintable_enabled,
            gaintable_version,
            gaintable_speaker,
        ) = prev_state
            .map(|e| {
                (
                    e.metering_enabled,
                    e.diag_enabled,
                    e.gaintable_enabled,
                    e.gaintable_version,
                    e.gaintable_speaker,
                )
            })
            .unwrap_or((false, false, false, None, None));
        let prev = clients.insert(
            addr,
            OscClientState {
                last_seen: Some(Instant::now()),
                metering_enabled,
                diag_enabled,
                gaintable_enabled,
                gaintable_version,
                gaintable_speaker,
            },
        );
        (prev.is_none(), metering_enabled)
    }

    pub(crate) fn heartbeat(&self, addr: SocketAddr) -> bool {
        let mut clients = self.clients.lock().unwrap();
        match clients.get_mut(&addr) {
            // Actively-registered client: refresh its liveness and ack.
            Some(entry) if entry.last_seen.is_some() => {
                entry.last_seen = Some(Instant::now());
                true
            }
            // Known only as a config-seeded *permanent* target that has never
            // actively registered (`last_seen == None`). Report it as unknown so
            // the heartbeat is answered with `/heartbeat/unknown`, forcing a
            // re-`register`. This is what makes a producer swap visible to the
            // client: when a fresh instance takes over the RX port (CLI⇄mpv),
            // its registry holds this address only as the permanent seed, so the
            // client re-registers and receives a new capabilities bundle, a full
            // object resend (names) and the metering subscription — instead of
            // being silently acked into a stale connection. Pure listeners that
            // never heartbeat are unaffected (broadcasts still reach them).
            _ => false,
        }
    }

    /// Enable/disable metering on all *permanent* clients (those registered via
    /// [`insert_permanent`], i.e. the config-defined default OSC target). Lets
    /// `--osc-metering` / `render.osc_metering` pre-subscribe the default target
    /// to meter bundles without it having to send a runtime enable message.
    pub(crate) fn set_metering_for_permanent(&self, enabled: bool) {
        let mut clients = self.clients.lock().unwrap();
        for client in clients.values_mut() {
            if client.last_seen.is_none() {
                client.metering_enabled = enabled;
            }
        }
    }

    pub(crate) fn set_metering(&self, addr: SocketAddr, enabled: bool) -> bool {
        let mut clients = self.clients.lock().unwrap();
        if let Some(entry) = clients.get_mut(&addr) {
            entry.metering_enabled = enabled;
            true
        } else {
            false
        }
    }

    pub(crate) fn set_diag(&self, addr: SocketAddr, enabled: bool) -> bool {
        let mut clients = self.clients.lock().unwrap();
        if let Some(entry) = clients.get_mut(&addr) {
            entry.diag_enabled = enabled;
            true
        } else {
            false
        }
    }

    /// Subscribe/unsubscribe a client to the gain-table push stream. Keeps the
    /// last-pushed version on unsubscribe so a quick re-subscribe can skip a
    /// resend. Returns false if the client is unknown.
    pub(crate) fn set_gaintable(&self, addr: SocketAddr, enabled: bool) -> bool {
        let mut clients = self.clients.lock().unwrap();
        if let Some(entry) = clients.get_mut(&addr) {
            entry.gaintable_enabled = enabled;
            true
        } else {
            false
        }
    }

    /// Record the gain-table version last pushed to a client (so future rebuilds
    /// and re-subscribes can compare and skip).
    pub(crate) fn set_gaintable_version(&self, addr: SocketAddr, version: u32) {
        let mut clients = self.clients.lock().unwrap();
        if let Some(entry) = clients.get_mut(&addr) {
            entry.gaintable_version = Some(version);
        }
    }

    /// Record which speaker a client wants the gain table for.
    pub(crate) fn set_gaintable_speaker(&self, addr: SocketAddr, speaker: usize) {
        let mut clients = self.clients.lock().unwrap();
        if let Some(entry) = clients.get_mut(&addr) {
            entry.gaintable_speaker = Some(speaker);
        }
    }

    /// The speaker a client is subscribed for, if any.
    pub(crate) fn gaintable_speaker(&self, addr: SocketAddr) -> Option<usize> {
        self.clients
            .lock()
            .unwrap()
            .get(&addr)
            .and_then(|c| c.gaintable_speaker)
    }

    /// Live gain-table subscribers as `(addr, last_pushed_version, speaker)`.
    /// Permanent clients always count; timed clients only while within the
    /// heartbeat window.
    pub(crate) fn gaintable_subscribers(&self) -> Vec<(SocketAddr, Option<u32>, Option<usize>)> {
        let clients = self.clients.lock().unwrap();
        let now = Instant::now();
        clients
            .iter()
            .filter(|(_, c)| {
                c.gaintable_enabled
                    && c.last_seen
                        .map(|t| now.duration_since(t) < self.timeout)
                        .unwrap_or(true)
            })
            .map(|(addr, c)| (*addr, c.gaintable_version, c.gaintable_speaker))
            .collect()
    }

    pub(crate) fn is_any_live(&self) -> bool {
        let clients = self.clients.lock().unwrap();
        let now = Instant::now();
        clients.values().any(|client| {
            client
                .last_seen
                .map(|t| now.duration_since(t) < self.timeout)
                .unwrap_or(true)
        })
    }

    pub(crate) fn is_any_metering_live(&self) -> bool {
        let clients = self.clients.lock().unwrap();
        let now = Instant::now();
        clients.values().any(|client| {
            client.metering_enabled
                && client
                    .last_seen
                    .map(|t| now.duration_since(t) < self.timeout)
                    .unwrap_or(true)
        })
    }

    pub(crate) fn is_any_diag_live(&self) -> bool {
        let clients = self.clients.lock().unwrap();
        let now = Instant::now();
        clients.values().any(|client| {
            client.diag_enabled
                && client
                    .last_seen
                    .map(|t| now.duration_since(t) < self.timeout)
                    .unwrap_or(true)
        })
    }

    #[cfg(test)]
    pub(crate) fn metering_for(&self, addr: SocketAddr) -> Option<bool> {
        self.clients
            .lock()
            .unwrap()
            .get(&addr)
            .map(|c| c.metering_enabled)
    }

    pub(crate) fn send_filtered<F>(&self, socket: &std::net::UdpSocket, bytes: &[u8], predicate: F)
    where
        F: Fn(&OscClientState) -> bool,
    {
        let mut clients = self.clients.lock().unwrap();
        let now = Instant::now();
        clients.retain(|addr, client| match client.last_seen {
            None => true,
            Some(t) => {
                if now.duration_since(t) >= self.timeout {
                    log::info!("OSC client timed out, removing: {}", addr);
                    false
                } else {
                    true
                }
            }
        });
        for (addr, client) in clients.iter() {
            if predicate(client) {
                if let Err(e) = socket.send_to(bytes, *addr) {
                    log::warn!("OSC broadcast error to {}: {}", addr, e);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permanent_metering_toggle_drives_metering_live() {
        let reg = OscClientRegistry::new(Duration::from_secs(5));
        let addr: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        reg.insert_permanent(addr);

        // Default target starts opted-out → no metering clients.
        assert_eq!(reg.metering_for(addr), Some(false));
        assert!(!reg.is_any_metering_live());

        // `--osc-metering` pre-enables it → metering now flows to the target.
        reg.set_metering_for_permanent(true);
        assert_eq!(reg.metering_for(addr), Some(true));
        assert!(reg.is_any_metering_live());

        reg.set_metering_for_permanent(false);
        assert!(!reg.is_any_metering_live());
    }

    #[test]
    fn permanent_seed_is_unknown_until_it_registers() {
        // Regression guard for the CLI⇄mpv swap: a fresh producer instance seeds
        // the config default target as a *permanent* client. Its heartbeat must
        // be reported unknown (→ client re-registers and re-handshakes) instead
        // of being silently acked, which would mask the producer change.
        let reg = OscClientRegistry::new(Duration::from_secs(5));
        let addr: SocketAddr = "127.0.0.1:9000".parse().unwrap();
        reg.insert_permanent(addr);

        // Seeded but never actively registered → heartbeat is unknown.
        assert!(
            !reg.heartbeat(addr),
            "permanent-only seed must not be acked"
        );

        // Registering reuses the permanent slot (so `is_new` is false — the live
        // bundle is sent regardless), promotes it to a live client (`last_seen`
        // set), and from then on its heartbeats are acked.
        let (is_new, _) = reg.register(addr);
        assert!(!is_new, "permanent seed already occupies the slot");
        assert!(reg.heartbeat(addr), "registered client must be acked");
    }

    #[test]
    fn unknown_address_heartbeat_is_unknown() {
        let reg = OscClientRegistry::new(Duration::from_secs(5));
        let addr: SocketAddr = "127.0.0.1:9100".parse().unwrap();
        assert!(!reg.heartbeat(addr));
    }
}
