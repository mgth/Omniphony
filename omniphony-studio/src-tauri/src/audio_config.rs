//! The audio configuration schema: defaults, ranges, and the one place they
//! are enforced.
//!
//! These twenty-odd clamps used to live in `buildAudioConfigPayload` in the
//! frontend, which meant the definition of a valid adaptive-resampling
//! configuration sat in the layer that *collects* the values rather than the
//! one that owns them. The command forwarded whatever it was handed.
//!
//! Now the command takes the raw form values, applies the schema, forwards the
//! corrected configuration and hands it back — so what the user sees after an
//! edit is what the renderer was actually told.
//!
//! Every bound here is the frontend's, preserved: this is a move, not a
//! retune. The one deliberate difference is that a non-finite value falls back
//! to its default instead of propagating — JS would send a `NaN` through as
//! JSON `null`, which the renderer then read as "absent".

use serde::{Deserialize, Serialize};

/// Round half away from zero, matching JavaScript's `Math.round` for the
/// positive values these fields hold. Rust's `f64::round` agrees there; this
/// exists so the intent is stated rather than assumed.
fn round_like_js(value: f64) -> f64 {
    value.round()
}

/// A number with a default for when the form sends nothing usable.
fn finite_or(value: Option<f64>, default: f64) -> f64 {
    match value {
        Some(v) if v.is_finite() => v,
        _ => default,
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct AdaptiveResampling {
    pub enabled: bool,
    pub enable_far_mode: bool,
    pub force_silence_in_far_mode: bool,
    pub hard_recover_high_in_far_mode: bool,
    pub hard_recover_low_in_far_mode: bool,
    pub far_mode_return_fade_in_ms: Option<f64>,
    pub kp_near: Option<f64>,
    pub ki: Option<f64>,
    /// Kept in the payload for protocol compatibility. Known to have no effect
    /// on the loop, so it is passed through rather than given a range that
    /// would imply one.
    pub integral_discharge_ratio: Option<f64>,
    pub max_adjust: Option<f64>,
    pub high_recover_entry_margin_ms: Option<f64>,
    pub update_interval_callbacks: Option<f64>,
    pub low_recover_settle_stable_ms: Option<f64>,
    pub low_recover_entry_margin_ms: Option<f64>,
    pub low_recover_exit_margin_ms: Option<f64>,
    pub low_recover_settle_margin_ms: Option<f64>,
    pub low_recover_refill_delta_alpha: Option<f64>,
    pub control_smoothing_cutoff_hz: Option<f64>,
    pub control_smoothing_order: Option<f64>,
    pub paused: bool,
    pub use_pre_bridge_clock: bool,
    pub use_output_pacing: bool,
    pub disable_backpressure: bool,
}

impl Default for AdaptiveResampling {
    fn default() -> Self {
        Self {
            enabled: false,
            enable_far_mode: false,
            force_silence_in_far_mode: false,
            hard_recover_high_in_far_mode: false,
            hard_recover_low_in_far_mode: false,
            far_mode_return_fade_in_ms: None,
            kp_near: None,
            ki: None,
            integral_discharge_ratio: None,
            max_adjust: None,
            high_recover_entry_margin_ms: None,
            update_interval_callbacks: None,
            low_recover_settle_stable_ms: None,
            low_recover_entry_margin_ms: None,
            low_recover_exit_margin_ms: None,
            low_recover_settle_margin_ms: None,
            low_recover_refill_delta_alpha: None,
            control_smoothing_cutoff_hz: None,
            control_smoothing_order: None,
            paused: false,
            use_pre_bridge_clock: false,
            use_output_pacing: false,
            disable_backpressure: false,
        }
    }
}

/// The effective configuration: every field resolved to a usable number.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveAdaptiveResampling {
    pub enabled: bool,
    pub enable_far_mode: bool,
    pub force_silence_in_far_mode: bool,
    pub hard_recover_high_in_far_mode: bool,
    pub hard_recover_low_in_far_mode: bool,
    pub far_mode_return_fade_in_ms: f64,
    pub kp_near: f64,
    pub ki: f64,
    pub integral_discharge_ratio: f64,
    pub max_adjust: f64,
    pub high_recover_entry_margin_ms: f64,
    pub update_interval_callbacks: f64,
    pub low_recover_settle_stable_ms: f64,
    pub low_recover_entry_margin_ms: f64,
    pub low_recover_exit_margin_ms: f64,
    pub low_recover_settle_margin_ms: f64,
    pub low_recover_refill_delta_alpha: f64,
    pub control_smoothing_cutoff_hz: f64,
    pub control_smoothing_order: f64,
    pub paused: bool,
    pub use_pre_bridge_clock: bool,
    pub use_output_pacing: bool,
    pub disable_backpressure: bool,
}

impl AdaptiveResampling {
    /// Apply the schema.
    ///
    /// The bounds are the frontend's, field for field. Where a field has no
    /// bound (`kpNear`, `ki`, `maxAdjust`, `integralDischargeRatio`) it keeps
    /// only its default — the renderer owns whatever range those have.
    pub fn resolve(&self) -> EffectiveAdaptiveResampling {
        EffectiveAdaptiveResampling {
            enabled: self.enabled,
            enable_far_mode: self.enable_far_mode,
            force_silence_in_far_mode: self.force_silence_in_far_mode,
            hard_recover_high_in_far_mode: self.hard_recover_high_in_far_mode,
            hard_recover_low_in_far_mode: self.hard_recover_low_in_far_mode,

            far_mode_return_fade_in_ms: round_like_js(finite_or(
                self.far_mode_return_fade_in_ms,
                0.0,
            ))
            .max(0.0),
            kp_near: finite_or(self.kp_near, 1.0),
            ki: finite_or(self.ki, 1.0),
            integral_discharge_ratio: finite_or(self.integral_discharge_ratio, 0.25),
            max_adjust: finite_or(self.max_adjust, 0.01),

            // At least one callback, at least one millisecond: a zero here
            // would make the recovery test fire on every callback.
            high_recover_entry_margin_ms: round_like_js(finite_or(
                self.high_recover_entry_margin_ms,
                1000.0,
            ))
            .max(1.0),
            update_interval_callbacks: round_like_js(finite_or(
                self.update_interval_callbacks,
                1.0,
            ))
            .max(1.0),

            low_recover_settle_stable_ms: finite_or(self.low_recover_settle_stable_ms, 200.0)
                .max(0.0),
            low_recover_entry_margin_ms: finite_or(self.low_recover_entry_margin_ms, 18.0).max(0.0),
            low_recover_exit_margin_ms: finite_or(self.low_recover_exit_margin_ms, 6.0).max(0.0),
            low_recover_settle_margin_ms: finite_or(self.low_recover_settle_margin_ms, 6.0)
                .max(0.0),
            // A blend factor: outside [0, 1] it is not a blend.
            low_recover_refill_delta_alpha: finite_or(self.low_recover_refill_delta_alpha, 0.5)
                .clamp(0.0, 1.0),

            // A zero cutoff would divide by zero in the smoother.
            control_smoothing_cutoff_hz: finite_or(self.control_smoothing_cutoff_hz, 0.5)
                .max(0.001),
            // Only first and second order are implemented.
            control_smoothing_order: round_like_js(finite_or(self.control_smoothing_order, 1.0))
                .clamp(1.0, 2.0),

            paused: self.paused,
            use_pre_bridge_clock: self.use_pre_bridge_clock,
            use_output_pacing: self.use_output_pacing,
            disable_backpressure: self.disable_backpressure,
        }
    }
}

/// The audio configuration as the form sends it.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AudioConfig {
    pub output_device: Option<String>,
    pub sample_rate: Option<f64>,
    pub latency_target_ms: Option<f64>,
    pub adaptive_resampling: AdaptiveResampling,
}

/// The audio configuration as the renderer will receive it.
#[derive(Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveAudioConfig {
    pub output_device: Option<String>,
    pub sample_rate: Option<f64>,
    pub latency_target_ms: Option<f64>,
    pub adaptive_resampling: EffectiveAdaptiveResampling,
}

impl AudioConfig {
    pub fn resolve(&self) -> EffectiveAudioConfig {
        EffectiveAudioConfig {
            // An empty device name means "whatever the system picks", which is
            // what absent means — so it is normalised to absent rather than
            // sent as an empty string the renderer would try to open.
            output_device: self
                .output_device
                .as_ref()
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty()),
            sample_rate: self.sample_rate.filter(|v| v.is_finite() && *v > 0.0),
            latency_target_ms: self.latency_target_ms.filter(|v| v.is_finite() && *v > 0.0),
            adaptive_resampling: self.adaptive_resampling.resolve(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(json: serde_json::Value) -> AudioConfig {
        serde_json::from_value(json).expect("payload must deserialize")
    }

    #[test]
    fn an_empty_payload_resolves_to_the_defaults() {
        let effective = raw(serde_json::json!({})).resolve();
        let r = &effective.adaptive_resampling;
        assert_eq!(r.kp_near, 1.0);
        assert_eq!(r.ki, 1.0);
        assert_eq!(r.integral_discharge_ratio, 0.25);
        assert_eq!(r.max_adjust, 0.01);
        assert_eq!(r.high_recover_entry_margin_ms, 1000.0);
        assert_eq!(r.update_interval_callbacks, 1.0);
        assert_eq!(r.low_recover_settle_stable_ms, 200.0);
        assert_eq!(r.control_smoothing_cutoff_hz, 0.5);
        assert_eq!(r.control_smoothing_order, 1.0);
        assert!(!r.enabled);
    }

    /// The acceptance case: out-of-range input must come back corrected.
    #[test]
    fn out_of_range_values_are_pulled_into_range() {
        let effective = raw(serde_json::json!({
            "adaptiveResampling": {
                "farModeReturnFadeInMs": -500,
                "highRecoverEntryMarginMs": 0,
                "updateIntervalCallbacks": 0,
                "lowRecoverSettleStableMs": -1,
                "lowRecoverRefillDeltaAlpha": 5,
                "controlSmoothingCutoffHz": 0,
                "controlSmoothingOrder": 9,
            }
        }))
        .resolve();
        let r = &effective.adaptive_resampling;
        assert_eq!(r.far_mode_return_fade_in_ms, 0.0);
        assert_eq!(r.high_recover_entry_margin_ms, 1.0);
        assert_eq!(r.update_interval_callbacks, 1.0);
        assert_eq!(r.low_recover_settle_stable_ms, 0.0);
        assert_eq!(r.low_recover_refill_delta_alpha, 1.0);
        assert_eq!(r.control_smoothing_cutoff_hz, 0.001);
        assert_eq!(r.control_smoothing_order, 2.0);
    }

    #[test]
    fn the_blend_factor_is_clamped_at_both_ends() {
        let low = raw(serde_json::json!({
            "adaptiveResampling": { "lowRecoverRefillDeltaAlpha": -3 }
        }))
        .resolve();
        assert_eq!(low.adaptive_resampling.low_recover_refill_delta_alpha, 0.0);
    }

    #[test]
    fn the_smoothing_order_is_rounded_before_clamping() {
        for (input, expected) in [(1.4, 1.0), (1.6, 2.0), (2.4, 2.0), (0.2, 1.0)] {
            let effective = raw(serde_json::json!({
                "adaptiveResampling": { "controlSmoothingOrder": input }
            }))
            .resolve();
            assert_eq!(
                effective.adaptive_resampling.control_smoothing_order, expected,
                "order {input}"
            );
        }
    }

    /// JS sent a `NaN` through as JSON `null`, which the renderer then read as
    /// "absent". Falling back to the default is the deliberate difference.
    #[test]
    fn a_non_finite_value_falls_back_to_its_default() {
        let effective = raw(serde_json::json!({
            "adaptiveResampling": { "kpNear": null, "controlSmoothingCutoffHz": null }
        }))
        .resolve();
        assert_eq!(effective.adaptive_resampling.kp_near, 1.0);
        assert_eq!(
            effective.adaptive_resampling.control_smoothing_cutoff_hz,
            0.5
        );
    }

    /// Fields with no bound keep whatever the user typed — the renderer owns
    /// their range, and inventing one here would silently retune the loop.
    #[test]
    fn unbounded_fields_are_passed_through_untouched() {
        let effective = raw(serde_json::json!({
            "adaptiveResampling": { "kpNear": 250.0, "ki": -4.0, "maxAdjust": 0.9 }
        }))
        .resolve();
        assert_eq!(effective.adaptive_resampling.kp_near, 250.0);
        assert_eq!(effective.adaptive_resampling.ki, -4.0);
        assert_eq!(effective.adaptive_resampling.max_adjust, 0.9);
    }

    #[test]
    fn an_empty_device_name_becomes_absent() {
        let effective = raw(serde_json::json!({ "outputDevice": "   " })).resolve();
        assert_eq!(effective.output_device, None);

        let effective = raw(serde_json::json!({ "outputDevice": " hw:1 " })).resolve();
        assert_eq!(effective.output_device.as_deref(), Some("hw:1"));
    }

    #[test]
    fn a_non_positive_sample_rate_becomes_absent() {
        assert_eq!(
            raw(serde_json::json!({ "sampleRate": 0 }))
                .resolve()
                .sample_rate,
            None
        );
        assert_eq!(
            raw(serde_json::json!({ "sampleRate": 48000 }))
                .resolve()
                .sample_rate,
            Some(48000.0)
        );
    }

    #[test]
    fn booleans_survive_the_round_trip() {
        let effective = raw(serde_json::json!({
            "adaptiveResampling": { "enabled": true, "paused": true, "useOutputPacing": true }
        }))
        .resolve();
        let r = &effective.adaptive_resampling;
        assert!(r.enabled && r.paused && r.use_output_pacing);
        assert!(!r.disable_backpressure);
    }

    /// Resolving an already-effective payload must not move it.
    #[test]
    fn resolve_is_idempotent() {
        let once = raw(serde_json::json!({
            "adaptiveResampling": { "controlSmoothingOrder": 9, "lowRecoverRefillDeltaAlpha": 5 }
        }))
        .resolve();
        let twice = raw(serde_json::to_value(&once).unwrap()).resolve();
        assert_eq!(once.adaptive_resampling, twice.adaptive_resampling);
    }
}
