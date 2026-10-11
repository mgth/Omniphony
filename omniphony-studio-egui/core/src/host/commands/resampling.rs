//! The latency target and the settings of the loop that holds it.
//!
//! Each command forwards a single value to the renderer over OSC. See
//! `docs/resampling-rework-plan.md` for what the renderer does with them.

use super::OscControlMsg;
use super::{SharedState, send_control};
use crate::osc_contract;

/// The two settings of the sync engine's latency loop
/// (`docs/resampling-rework-plan.md`), host options of the renderer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncSetting {
    /// Bandwidth of the latency loop (Hz); 0 is the source's own default.
    LoopBandwidthHz,
    /// Latency error beyond which the output realigns (ms).
    RealignThresholdMs,
}

impl SyncSetting {
    /// The option's key in the renderer's registry.
    pub fn key(self) -> &'static str {
        match self {
            Self::LoopBandwidthHz => "loop_bandwidth_hz",
            Self::RealignThresholdMs => "realign_threshold_ms",
        }
    }

    /// The range the renderer takes.
    pub fn range(self) -> (f64, f64) {
        match self {
            Self::LoopBandwidthHz => (0.0, 1.0),
            Self::RealignThresholdMs => (1.0, 1000.0),
        }
    }

    /// The value shown before the renderer has published one.
    pub fn default(self) -> f64 {
        match self {
            Self::LoopBandwidthHz => 0.0,
            Self::RealignThresholdMs => 20.0,
        }
    }

    /// The value in force, as the renderer last published it.
    pub fn get(self, state: &SharedState) -> f64 {
        state
            .read()
            .host_option_f64(self.key())
            .unwrap_or(self.default())
    }
}

/// The clocks a source can be taken to run on (the renderer's `source_clock`
/// option), in the order a client lists them. The renderer falls back to the
/// input's own for one the input cannot have.
pub const SOURCE_CLOCKS: [&str; 4] = ["auto", "own", "follow", "none"];

/// The source clock asked for, as the renderer last published it.
pub fn source_clock(state: &SharedState) -> String {
    state
        .read()
        .host_option_str("source_clock")
        .filter(|clock| SOURCE_CLOCKS.contains(&clock.as_str()))
        .unwrap_or_else(|| SOURCE_CLOCKS[0].to_owned())
}

/// Ask for another source clock: shown at once and sent. A name that is not
/// one of [`SOURCE_CLOCKS`] is not sent.
pub fn set_source_clock(state: &SharedState, clock: &str) {
    if !SOURCE_CLOCKS.contains(&clock) {
        return;
    }
    state
        .inner
        .lock()
        .unwrap()
        .set_host_option("source_clock", serde_json::json!(clock));
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_OPTION.to_string(),
            args: vec![
                rosc::OscType::String("source_clock".to_owned()),
                rosc::OscType::String(clock.to_owned()),
            ],
        },
    );
}

/// Set one of the sync engine's loop settings: clamped, shown at once and
/// sent. The renderer reopens its output with it.
pub fn set_sync_setting(state: &SharedState, setting: SyncSetting, value: f64) {
    if !value.is_finite() {
        return;
    }
    let (min, max) = setting.range();
    let value = value.clamp(min, max);
    state
        .inner
        .lock()
        .unwrap()
        .set_host_option(setting.key(), serde_json::json!(value));
    send_control(
        &state.osc_tx,
        OscControlMsg::SendArgs {
            address: osc_contract::CONTROL_OPTION.to_string(),
            args: vec![
                rosc::OscType::String(setting.key().to_owned()),
                rosc::OscType::Float(value as f32),
            ],
        },
    );
}

/// Apply the target the form asked for: the model shows it at once (the
/// requested value and the target the meter draws against), and the renderer
/// is told.
pub fn set_latency_target(state: &SharedState, ms: i64) {
    let requested = ms.max(1);
    {
        let mut live = state.inner.lock().unwrap();
        live.app.latency.latency_requested_ms = Some(requested);
        live.app.latency.latency_target_ms = Some(requested);
    }
    control_latency_target(state, requested as i32);
}

pub fn control_latency_target(state: &SharedState, value: i32) {
    send_control(
        &state.osc_tx,
        OscControlMsg::SendInt {
            address: osc_contract::CONTROL_LATENCY_TARGET.to_string(),
            value: value.max(1),
        },
    );
}

#[cfg(test)]
mod sync_setting_tests {
    use super::*;
    use crate::host::commands::tests::state_with_outbox;
    use crate::osc::Control;

    /// A loop setting is clamped to what the renderer takes, shown at once
    /// and sent as the registry's option message, its key first.
    #[test]
    fn a_loop_setting_is_clamped_shown_and_sent() {
        let (state, rx) = state_with_outbox(std::sync::Arc::new(|| {}));
        assert_eq!(SyncSetting::RealignThresholdMs.get(&state), 20.0);
        assert_eq!(SyncSetting::LoopBandwidthHz.get(&state), 0.0);

        set_sync_setting(&state, SyncSetting::RealignThresholdMs, 5_000.0);
        assert_eq!(SyncSetting::RealignThresholdMs.get(&state), 1000.0);
        set_sync_setting(&state, SyncSetting::LoopBandwidthHz, 0.01);
        set_sync_setting(&state, SyncSetting::LoopBandwidthHz, f64::NAN);
        assert_eq!(SyncSetting::LoopBandwidthHz.get(&state), 0.01);

        let sent: Vec<(String, Vec<rosc::OscType>)> = rx
            .try_iter()
            .filter_map(|control| match control {
                Control::Send { address, args } => Some((address, args)),
                _ => None,
            })
            .collect();
        assert_eq!(sent.len(), 2, "the NaN is not sent");
        assert!(
            sent.iter()
                .all(|(address, _)| address == osc_contract::CONTROL_OPTION)
        );
        assert_eq!(
            sent[0].1,
            vec![
                rosc::OscType::String("realign_threshold_ms".into()),
                rosc::OscType::Float(1000.0)
            ]
        );
        assert_eq!(
            sent[1].1[0],
            rosc::OscType::String("loop_bandwidth_hz".into())
        );
    }

    /// The source clock is one of the renderer's names: shown at once, sent
    /// as the option message, and anything else is not sent at all.
    #[test]
    fn a_source_clock_is_sent_by_name_or_not_at_all() {
        let (state, rx) = state_with_outbox(std::sync::Arc::new(|| {}));
        assert_eq!(source_clock(&state), "auto");
        set_source_clock(&state, "none");
        assert_eq!(source_clock(&state), "none");
        set_source_clock(&state, "dac");
        assert_eq!(source_clock(&state), "none");
        let sent: Vec<Vec<rosc::OscType>> = rx
            .try_iter()
            .filter_map(|control| match control {
                Control::Send { args, .. } => Some(args),
                _ => None,
            })
            .collect();
        assert_eq!(
            sent,
            vec![vec![
                rosc::OscType::String("source_clock".into()),
                rosc::OscType::String("none".into())
            ]]
        );
    }
}
