//! Turning [`Event`]s into the `(name, payload)` pair the frontend receives.
//!
//! Kept out of both the machine and the runner so the parity test and the
//! live runner serialise through exactly the same code. A test that checked a
//! second, test-only serialiser would prove nothing about what the UI is sent.

use serde_json::Value;

use super::state_machine::Event;

/// The event's name as the recorder wrote it.
pub fn event_name(event: &Event) -> &'static str {
    match event {
        Event::ApplyParams { .. } => "applyParams",
        Event::Progress { .. } => "progress",
        Event::AwaitUserAction { .. } => "awaitUserAction",
        Event::SourceLost { .. } => "sourceLost",
        Event::SourceRecovered { .. } => "sourceRecovered",
        Event::Complete(_) => "complete",
        Event::Cancelled => "cancelled",
        Event::Error { .. } => "error",
    }
}

/// The event's payload, in the shape the frontend listener receives it.
///
/// This matters beyond parity for its own sake: the wizard reads payload
/// fields directly (`payload.palierStats?.peakToPeakPpm`,
/// `payload.verdict?.reason`, wizard-ui.js:115-119), so a payload the port
/// does not reproduce is a readout the UI silently loses.
pub fn event_payload(event: &Event) -> Value {
    // `emit('progress', { step, ...detail })` and friends: the tag is
    // merged into the detail object, not nested under it.
    let merged = |tag_key: &str, tag: &str, detail: &Value| {
        let mut o = serde_json::Map::new();
        o.insert(tag_key.to_string(), Value::String(tag.to_string()));
        if let Some(d) = detail.as_object() {
            for (k, v) in d {
                o.insert(k.clone(), v.clone());
            }
        }
        Value::Object(o)
    };
    match event {
        Event::ApplyParams {
            kp_near,
            ki,
            max_adjust,
            update_interval_callbacks,
        } => {
            // The JS emits only the keys it is actually changing.
            let mut o = serde_json::Map::new();
            for (k, v) in [
                ("kpNear", kp_near),
                ("ki", ki),
                ("maxAdjust", max_adjust),
                ("updateIntervalCallbacks", update_interval_callbacks),
            ] {
                if let Some(v) = v {
                    o.insert(k.to_string(), serde_json::json!(v));
                }
            }
            Value::Object(o)
        }
        Event::Progress { step, detail } => merged("step", step, detail),
        Event::AwaitUserAction { kind, detail } => merged("kind", kind, detail),
        Event::Error { kind, detail } => merged("kind", kind, detail),
        Event::SourceLost { events } => serde_json::json!({ "events": events }),
        Event::SourceRecovered { restored_state } => {
            serde_json::json!({ "restoredState": restored_state })
        }
        Event::Complete(r) => serde_json::json!({
            "kpCrit": r.kp_crit,
            "kpFinal": r.kp_final,
            "kiFinal": r.ki_final,
            "maxAdjustFinal": r.max_adjust_final,
            "updateIntervalFinal": r.update_interval_final,
            "tighteningOscillation": r.tightening_oscillation,
            "tighteningConverged": r.tightening_converged,
        }),
        // `emit('cancelled', {})` — an empty object, not a null.
        Event::Cancelled => serde_json::json!({}),
    }
}
