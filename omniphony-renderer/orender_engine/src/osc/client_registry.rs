use std::collections::{BTreeMap, HashMap};
use std::net::SocketAddr;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone)]
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
    /// Every gain-table target this client wants, mapped to the version last
    /// pushed for it: a speaker index, or
    /// [`renderer::band_gaintable::GLOBAL_ENERGY_INDEX`] for the all-speaker
    /// energy field.
    ///
    /// A *map*, not a single target: a client showing two heatmaps at once
    /// needs both fields, and holding one slot meant the second display
    /// silently starved on a stale cache. Bounded by [`MAX_GAINTABLE_TARGETS`]
    /// so a misbehaving client cannot grow it without limit.
    pub(crate) gaintable_targets: BTreeMap<i64, Option<u32>>,
}

/// Most gain-table targets one client may hold at once. Three displays (the
/// per-speaker heatmap, the global energy one and the discontinuity one) is
/// the real case, and the discontinuity display swaps between two targets
/// (gain-configuration vs centroid); the margin covers that rotation without
/// letting a buggy client accumulate targets forever. Kept comfortably above
/// the real case because eviction drops the LOWEST target first — which is a
/// negative sentinel, i.e. one of the global heatmaps, not an idle speaker.
const MAX_GAINTABLE_TARGETS: usize = 6;

pub(crate) struct OscClientRegistry {
    clients: Mutex<HashMap<SocketAddr, OscClientState>>,
    timeout: Duration,
    /// Whether any client is live, any subscribes to the meters, any to the
    /// diag traces: what the render path asks every block, so it reads these
    /// rather than take the lock (#670). Published on every change and on
    /// every telemetry tick, which is when a timed-out client is noticed.
    any_live: AtomicBool,
    any_metering_live: AtomicBool,
    any_diag_live: AtomicBool,
}

impl OscClientRegistry {
    pub(crate) fn new(timeout: Duration) -> Self {
        Self {
            clients: Mutex::new(HashMap::new()),
            timeout,
            any_live: AtomicBool::new(false),
            any_metering_live: AtomicBool::new(false),
            any_diag_live: AtomicBool::new(false),
        }
    }

    /// Publish who is live in `clients` for the lock-free queries.
    fn publish_presence(&self, clients: &HashMap<SocketAddr, OscClientState>) {
        let now = Instant::now();
        let (mut live, mut metering, mut diag) = (false, false, false);
        for client in clients.values() {
            if client
                .last_seen
                .is_some_and(|t| now.duration_since(t) >= self.timeout)
            {
                continue;
            }
            live = true;
            metering |= client.metering_enabled;
            diag |= client.diag_enabled;
        }
        self.any_live.store(live, Ordering::Relaxed);
        self.any_metering_live.store(metering, Ordering::Relaxed);
        self.any_diag_live.store(diag, Ordering::Relaxed);
    }

    /// Publish who is live now, for the timeouts no change reports.
    pub(crate) fn refresh_presence(&self) {
        self.publish_presence(&self.clients.lock().unwrap());
    }

    pub(crate) fn insert_permanent(&self, addr: SocketAddr) {
        let mut clients = self.clients.lock().unwrap();
        clients.insert(
            addr,
            OscClientState {
                last_seen: None,
                metering_enabled: false,
                diag_enabled: false,
                gaintable_enabled: false,
                gaintable_targets: BTreeMap::new(),
            },
        );
        self.publish_presence(&clients);
    }

    pub(crate) fn register(&self, addr: SocketAddr) -> (bool, bool) {
        let mut clients = self.clients.lock().unwrap();
        let prev_state = clients.get(&addr).cloned();
        let (metering_enabled, diag_enabled, gaintable_enabled, gaintable_targets) = prev_state
            .map(|e| {
                (
                    e.metering_enabled,
                    e.diag_enabled,
                    e.gaintable_enabled,
                    e.gaintable_targets,
                )
            })
            .unwrap_or((false, false, false, BTreeMap::new()));
        let prev = clients.insert(
            addr,
            OscClientState {
                last_seen: Some(Instant::now()),
                metering_enabled,
                diag_enabled,
                gaintable_enabled,
                gaintable_targets,
            },
        );
        self.publish_presence(&clients);
        (prev.is_none(), metering_enabled)
    }

    pub(crate) fn heartbeat(&self, addr: SocketAddr) -> bool {
        let mut clients = self.clients.lock().unwrap();
        match clients.get_mut(&addr) {
            // Actively-registered client: refresh its liveness and ack.
            Some(entry) if entry.last_seen.is_some() => {
                entry.last_seen = Some(Instant::now());
                self.publish_presence(&clients);
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
        self.publish_presence(&clients);
    }

    pub(crate) fn set_metering(&self, addr: SocketAddr, enabled: bool) -> bool {
        let mut clients = self.clients.lock().unwrap();
        if let Some(entry) = clients.get_mut(&addr) {
            entry.metering_enabled = enabled;
            self.publish_presence(&clients);
            true
        } else {
            false
        }
    }

    pub(crate) fn set_diag(&self, addr: SocketAddr, enabled: bool) -> bool {
        let mut clients = self.clients.lock().unwrap();
        if let Some(entry) = clients.get_mut(&addr) {
            entry.diag_enabled = enabled;
            self.publish_presence(&clients);
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

    /// Record the version last pushed to a client **for one target**, so a
    /// rebuild or a re-subscribe carrying the same version can skip the resend.
    pub(crate) fn set_gaintable_version(&self, addr: SocketAddr, target: i64, version: u32) {
        let mut clients = self.clients.lock().unwrap();
        if let Some(entry) = clients.get_mut(&addr) {
            entry.gaintable_targets.insert(target, Some(version));
        }
    }

    /// Add a target this client wants the gain table for, keeping the ones it
    /// already has. Evicts the lowest target when full rather than refusing, so
    /// a client that legitimately rotates targets keeps working.
    pub(crate) fn add_gaintable_target(&self, addr: SocketAddr, target: i64) {
        let mut clients = self.clients.lock().unwrap();
        if let Some(entry) = clients.get_mut(&addr) {
            if !entry.gaintable_targets.contains_key(&target)
                && entry.gaintable_targets.len() >= MAX_GAINTABLE_TARGETS
            {
                if let Some(oldest) = entry.gaintable_targets.keys().next().copied() {
                    entry.gaintable_targets.remove(&oldest);
                }
            }
            entry.gaintable_targets.entry(target).or_insert(None);
        }
    }

    /// Which target a client last received `version` for. A NACK carries only
    /// the version, so this is what maps it back to the field to resend —
    /// correct even with several transfers in flight.
    pub(crate) fn gaintable_target_for_version(
        &self,
        addr: SocketAddr,
        version: u32,
    ) -> Option<i64> {
        self.clients.lock().unwrap().get(&addr).and_then(|c| {
            c.gaintable_targets
                .iter()
                .find(|(_, v)| **v == Some(version))
                .map(|(target, _)| *target)
        })
    }

    /// Forget every target of a client (on unsubscribe), so a later subscribe
    /// starts from a clean slate.
    pub(crate) fn clear_gaintable_targets(&self, addr: SocketAddr) {
        let mut clients = self.clients.lock().unwrap();
        if let Some(entry) = clients.get_mut(&addr) {
            entry.gaintable_targets.clear();
        }
    }

    /// Live gain-table subscribers as `(addr, [(target, last_pushed_version)])`.
    /// Permanent clients always count; timed clients only while within the
    /// heartbeat window.
    #[allow(clippy::type_complexity)]
    pub(crate) fn gaintable_subscribers(&self) -> Vec<(SocketAddr, Vec<(i64, Option<u32>)>)> {
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
            .map(|(addr, c)| {
                (
                    *addr,
                    c.gaintable_targets
                        .iter()
                        .map(|(target, version)| (*target, *version))
                        .collect(),
                )
            })
            .collect()
    }

    pub(crate) fn is_any_live(&self) -> bool {
        self.any_live.load(Ordering::Relaxed)
    }

    pub(crate) fn is_any_metering_live(&self) -> bool {
        self.any_metering_live.load(Ordering::Relaxed)
    }

    pub(crate) fn is_any_diag_live(&self) -> bool {
        self.any_diag_live.load(Ordering::Relaxed)
    }

    /// Hold the registry, as a slow or contended sender would.
    #[cfg(test)]
    pub(crate) fn lock_for_test(
        &self,
    ) -> std::sync::MutexGuard<'_, HashMap<SocketAddr, OscClientState>> {
        self.clients.lock().unwrap()
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
        self.publish_presence(&clients);
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
