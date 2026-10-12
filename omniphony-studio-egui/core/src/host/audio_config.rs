//! The audio configuration schema: what the output form may send, and the one
//! place it is made valid.
//!
//! The command takes the raw form values, applies the schema, forwards the
//! corrected configuration and hands it back, so what the user sees after an
//! edit is what the renderer was actually told.
//!
//! A non-finite value is dropped instead of propagating: sent as JSON `null`,
//! the renderer would read it as "absent" without anyone having decided so.

use serde::{Deserialize, Serialize};

/// The audio configuration as the form sends it.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct AudioConfig {
    pub output_device: Option<String>,
    pub sample_rate: Option<f64>,
    pub latency_target_ms: Option<f64>,
}

/// The audio configuration as the renderer will receive it.
#[derive(Serialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveAudioConfig {
    pub output_device: Option<String>,
    pub sample_rate: Option<u32>,
    pub latency_target_ms: Option<u32>,
}

impl AudioConfig {
    pub fn resolve(&self) -> EffectiveAudioConfig {
        // Integers on the wire: the renderer types these `Option<u32>`, and
        // `serde_json` refuses a float for a `u32`.
        let whole = |value: Option<f64>| {
            value
                .filter(|v| v.is_finite() && *v > 0.0)
                .map(|v| v.round() as u32)
        };
        EffectiveAudioConfig {
            // An empty device name means "whatever the system picks", which is
            // what absent means — so it is normalised to absent rather than
            // sent as an empty string the renderer would try to open.
            output_device: self
                .output_device
                .as_ref()
                .map(|d| d.trim().to_string())
                .filter(|d| !d.is_empty()),
            sample_rate: whole(self.sample_rate),
            latency_target_ms: whole(self.latency_target_ms),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(json: serde_json::Value) -> AudioConfig {
        serde_json::from_value(json).expect("payload must deserialize")
    }

    /// The renderer types these `Option<u32>` and `serde_json` refuses a float
    /// for a `u32`, so a single `48000.0` in place of `48000` makes it drop
    /// the whole patch — silently. Every audio setting then stops arriving and
    /// the UI looks like it is ignoring clicks. So the assertion is on the
    /// serialised text, which is what actually crosses the wire.
    #[test]
    fn the_wire_format_keeps_integers_integral() {
        let text = serde_json::to_string(
            &raw(serde_json::json!({ "sampleRate": 48000.0, "latencyTargetMs": 49.6 })).resolve(),
        )
        .unwrap();
        assert!(text.contains(r#""sampleRate":48000"#), "{text}");
        assert!(text.contains(r#""latencyTargetMs":50"#), "{text}");
        assert!(!text.contains('.'), "a float reached the wire: {text}");
    }

    /// What the form used to send with the document (the old regulation's
    /// block) is not an error: it is not part of the schema any more.
    #[test]
    fn an_unknown_block_is_ignored() {
        let effective = raw(serde_json::json!({
            "sampleRate": 48000,
            "adaptiveResampling": { "enabled": true, "kpNear": 2.0 }
        }))
        .resolve();
        assert_eq!(effective.sample_rate, Some(48000));
        assert!(
            !serde_json::to_string(&effective)
                .unwrap()
                .contains("adaptive")
        );
    }

    #[test]
    fn an_empty_device_name_becomes_absent() {
        let effective = raw(serde_json::json!({ "outputDevice": "   " })).resolve();
        assert_eq!(effective.output_device, None);

        let effective = raw(serde_json::json!({ "outputDevice": " hw:1 " })).resolve();
        assert_eq!(effective.output_device.as_deref(), Some("hw:1"));
    }

    #[test]
    fn a_non_positive_or_non_finite_number_becomes_absent() {
        for value in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let config = AudioConfig {
                sample_rate: Some(value),
                latency_target_ms: Some(value),
                ..Default::default()
            };
            let effective = config.resolve();
            assert_eq!(effective.sample_rate, None, "{value}");
            assert_eq!(effective.latency_target_ms, None, "{value}");
        }
        assert_eq!(
            raw(serde_json::json!({ "sampleRate": 48000 }))
                .resolve()
                .sample_rate,
            Some(48000)
        );
    }

    /// Resolving an already-effective payload must not move it.
    #[test]
    fn resolve_is_idempotent() {
        let once = raw(serde_json::json!({
            "outputDevice": " hw:1 ", "sampleRate": 44100.4, "latencyTargetMs": 180
        }))
        .resolve();
        let twice = raw(serde_json::to_value(&once).unwrap()).resolve();
        assert_eq!(once, twice);
    }
}
