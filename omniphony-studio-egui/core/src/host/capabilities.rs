//! Toolkit-independent presentation policy derived from the current producer.
//! Unknown legacy producers keep the standalone controls visible. Known
//! capabilities are authoritative; an offline embedded producer must not hide
//! the local recovery actions needed to bring up a standalone renderer.

use super::commands::SharedState;
use crate::osc::ConnectionState;
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionPolicy {
    pub renderer_ready: bool,
    pub audio_output: bool,
    pub adaptive_resampling: bool,
    /// The output is regulated by the sync engine: a latency target and its
    /// two loop settings, none of the adaptive resampler's tuning. Only a
    /// renderer that says so: an unknown one is taken for the legacy engine.
    pub sync_engine: bool,
    pub manage_process: bool,
    pub input: bool,
}

impl ActionPolicy {
    pub fn of(state: &SharedState) -> Self {
        let connection = state.stats.connection_state();
        let local = state
            .stats
            .target
            .lock()
            .unwrap()
            .is_none_or(|target| target.ip().is_loopback());
        let live = state.read();
        Self::from_producer(
            connection,
            live.app.osc_snapshot_ready,
            local,
            live.app.producer_capabilities.as_ref(),
        )
    }

    pub fn from_producer(
        connection: ConnectionState,
        snapshot_ready: bool,
        local: bool,
        caps: Option<&Value>,
    ) -> Self {
        let connected = connection == ConnectionState::Connected;
        // Cached capabilities do not describe a renderer that is no longer
        // connected. Keep the connection and recovery forms available then.
        let caps = caps.filter(|_| connected);
        let embedded =
            caps.and_then(|v| v.get("variant")).and_then(Value::as_str) == Some("embedded");
        let supports = |field: &str, name: &str| {
            caps.is_none_or(|caps| {
                caps.get(field)
                    .and_then(Value::as_array)
                    .is_some_and(|items| items.iter().any(|item| item.as_str() == Some(name)))
            })
        };
        Self {
            renderer_ready: connected && snapshot_ready,
            audio_output: !embedded && supports("domains", "audio"),
            adaptive_resampling: !embedded
                && supports("domains", "audio")
                && supports("controlConfig", "adaptive_resampling"),
            sync_engine: !embedded
                && caps.is_some_and(|caps| {
                    caps.get("controlConfig")
                        .and_then(Value::as_array)
                        .is_some_and(|items| items.iter().any(|item| item.as_str() == Some("sync")))
                }),
            manage_process: local && !embedded,
            input: !embedded && supports("domains", "input"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn standalone_embedded_offline_and_legacy_matrix() {
        let standalone = json!({"variant":"standalone", "domains":["audio", "input"], "controlConfig":["adaptive_resampling"]});
        let embedded = json!({"variant":"embedded", "domains":["render"], "controlConfig":[]});
        let connected = ConnectionState::Connected;
        let full = ActionPolicy::from_producer(connected, true, true, Some(&standalone));
        assert!(
            full.renderer_ready
                && full.audio_output
                && full.input
                && full.adaptive_resampling
                && full.manage_process
        );
        let hosted = ActionPolicy::from_producer(connected, true, true, Some(&embedded));
        assert!(hosted.renderer_ready);
        assert!(
            !hosted.audio_output
                && !hosted.input
                && !hosted.adaptive_resampling
                && !hosted.manage_process
        );
        let offline =
            ActionPolicy::from_producer(ConnectionState::Reconnecting, true, true, Some(&embedded));
        assert!(!offline.renderer_ready);
        assert!(offline.manage_process && offline.input);
        let legacy = ActionPolicy::from_producer(connected, true, true, None);
        assert_eq!(legacy, full);
        let remote = ActionPolicy::from_producer(connected, true, false, Some(&standalone));
        assert!(!remote.manage_process);
        let partial = ActionPolicy::from_producer(
            connected,
            false,
            true,
            Some(&json!({"domains":["audio"]})),
        );
        assert!(partial.audio_output);
        assert!(!partial.adaptive_resampling && !partial.input && !partial.renderer_ready);
        assert!(!full.sync_engine && !legacy.sync_engine && !hosted.sync_engine);
    }

    /// The sync engine's renderer has an audio output and a latency target,
    /// no resampler tuning and, for now, no input to reconfigure.
    #[test]
    fn the_sync_engine_shows_its_latency_and_hides_the_resampler_tuning() {
        let sync = json!({
            "variant": "standalone",
            "domains": ["renderer", "audio"],
            "controlConfig": ["layout", "speakers", "audio", "sync"],
        });
        let policy =
            ActionPolicy::from_producer(ConnectionState::Connected, true, true, Some(&sync));
        assert!(policy.audio_output && policy.sync_engine && policy.manage_process);
        assert!(!policy.adaptive_resampling && !policy.input);
        // Not connected any more: what it said no longer describes anything.
        let offline =
            ActionPolicy::from_producer(ConnectionState::Reconnecting, true, true, Some(&sync));
        assert!(!offline.sync_engine);
    }
}
