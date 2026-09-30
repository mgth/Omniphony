//! Extension point for host-owned control domains (audio output/input).
//!
//! The audio-free core (this crate, inside `liborender`) handles the
//! renderer/layout/speakers/loudness/DRC OSC domains. A host that owns audio
//! (the `orender` CLI) implements [`HostControlHandler`] in the separate
//! `host_audio` crate and registers it with the engine's OSC server, which
//! delegates unrecognised control messages to it and folds its state into the
//! live-state snapshot and the saved config. Hosts without audio (mpv via
//! `liborender`) register nothing, so the core never references the audio crates.

use rosc::{OscMessage, OscPacket};

use crate::osc::ControlEffects;

pub trait HostControlHandler: Send + Sync {
    /// Handle a control message the core did not recognise (e.g.
    /// `/omniphony/control/audio/*`, `/omniphony/control/input/*`). Returns the
    /// resulting effects, or `None` if this handler does not own `addr`.
    fn handle(&self, addr: &str, msg: &OscMessage) -> Option<ControlEffects>;

    /// Extra state messages appended to the live-state snapshot bundle, e.g.
    /// `/omniphony/state/audio` and the live-input portion of
    /// `/omniphony/state/input`.
    fn extend_snapshot(&self) -> Vec<OscPacket>;

    /// Amend the config being saved with host-owned fields (output device,
    /// live input, adaptive resampling, latency target) before it is written.
    fn amend_saved_config(&self, render: &mut renderer::config::RenderConfig);

    /// Monotonic generation counter the engine's OSC listener polls to decide
    /// when to re-broadcast the live-state bundle. Bumps when host-owned state
    /// changed asynchronously (e.g. PipeWire device enumeration). Default `0`
    /// disables polling for hosts that don't need it (embedded).
    fn state_generation(&self) -> u64 {
        0
    }

    // ── Host-declared options (`renderer::options::HostOptionSpec` rows) ──
    //
    // Each a one-line call to the `renderer::options::host_*` helpers over
    // the host's static rows. The defaults declare none, so a host that owns
    // no settings (or the embedded engine, which registers no host) needs
    // nothing here.

    /// The kind of this host's option `key` — how `/control/options` knows
    /// how many arguments its value takes. `None`: not one of this host's.
    fn option_kind(&self, _key: &str) -> Option<renderer::options::OptionKind> {
        None
    }

    /// Apply a batch of this host's options (keys it declares).
    fn apply_options(
        &self,
        items: &[(&str, renderer::options::RawOptionValue)],
    ) -> renderer::options::HostBatchApplied {
        renderer::options::HostBatchApplied {
            results: vec![None; items.len()],
            changed: false,
        }
    }

    /// Apply one of this host's groups on command
    /// (`/control/options/apply`). `None`: not one of this host's groups.
    fn apply_option_group(&self, _group: &str) -> Option<ControlEffects> {
        None
    }

    /// The schema entries of this host's options, published after the core's.
    fn options_schema(&self) -> Vec<serde_json::Value> {
        Vec::new()
    }

    /// The (requested) value of each of this host's options.
    fn options_json(&self) -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::new()
    }

    /// The value in force of each of this host's `Staged` options that
    /// reports one.
    fn options_applied_json(&self) -> serde_json::Map<String, serde_json::Value> {
        serde_json::Map::new()
    }

    /// This host's `Staged` groups, each with whether it holds requested
    /// values not applied yet.
    fn option_groups_pending(&self) -> Vec<(&'static str, bool)> {
        Vec::new()
    }
}
