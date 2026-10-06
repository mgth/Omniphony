use anyhow::Result;
use rosc::{OscBundle, OscMessage, OscPacket, OscTime, OscType};
use serde_json::json;

use super::OscSender;
use super::telemetry::{Event, MeterReport, MeterTimings};
use renderer::live_params::RendererControl;
use runtime_control::osc_contract;
use std::sync::Arc;

/// What the render path reports to clients besides the objects. Each call
/// queues plain values for the telemetry thread (`telemetry`), which
/// encodes and sends them; none of them sends anything itself.
impl OscSender {
    /// Queue a refresh of the whole live state, built and sent on the
    /// telemetry thread.
    pub fn send_live_state_bundle(&mut self) {
        let Some(control) = self.control.clone() else {
            return;
        };
        let host = self.host_handler.clone();
        self.telemetry.push(Event::LiveState { control, host });
    }

    pub fn send_loudness_state(&mut self) {
        let Some(control) = self.control.as_ref() else {
            return;
        };
        // Read when it is published, not here: see `loudness_state_message`.
        self.telemetry.push(Event::Loudness {
            control: Arc::clone(control),
        });
    }

    /// Publish the diag schema and/or values bundle to subscribed clients.
    /// Independent of the meter bundle so diag traces can be turned on/off
    /// and re-cadenced without touching audio-level publication. No-op when
    /// both arguments are `None`.
    pub fn send_diag_bundle(
        &mut self,
        diag_schema_json: Option<String>,
        diag_values_json: Option<String>,
    ) {
        if diag_schema_json.is_none() && diag_values_json.is_none() {
            return;
        }
        self.telemetry.push(Event::Diag {
            schema: diag_schema_json,
            values: diag_values_json,
        });
    }

    /// Queue the meter bundle of `rendered`: the levels in `snapshot`, its
    /// per-object gains, and `timings`.
    ///
    /// The gain lists are taken from `rendered`, and lists the telemetry thread
    /// is done with put in their place, so the renderer refills those once the
    /// frame is recycled: nothing is copied or allocated here.
    pub fn send_meter_bundle(
        &mut self,
        snapshot: &mut renderer::metering::MeterSnapshot,
        rendered: &mut renderer::spatial_renderer::RenderedFrame,
        timings: MeterTimings,
    ) {
        // The report takes the snapshot and the renderer's lists; the render
        // path gets spare ones back, refilled on its next metered frame.
        let (gains, band_gains, spare) = self.telemetry.meter_lists();
        let report = MeterReport {
            block: self.telemetry.block,
            snapshot: std::mem::replace(snapshot, spare),
            object_gains: std::mem::replace(&mut rendered.object_gains, gains),
            object_band_gains: std::mem::replace(&mut rendered.object_band_gains, band_gains),
            // Where the object test's source is, orbit and room clamp applied.
            // Rides the meter bundle because it is the same kind of thing: a
            // per-interval readout of what the render just did, wanted at the
            // same rate and by the same clients, and already gated by that
            // subscription.
            object_test_position: rendered.object_test_position,
            object_test_level: rendered.object_test_level,
            timings,
        };
        self.telemetry.push(Event::Meter(report));
    }

    pub fn send_timing_update(
        &mut self,
        decode_time_ms: Option<f32>,
        render_time_ms: Option<f32>,
        write_time_ms: Option<f32>,
    ) {
        if decode_time_ms.is_none() && render_time_ms.is_none() && write_time_ms.is_none() {
            return;
        }
        let block = self.telemetry.block;
        self.telemetry.push(Event::Timing {
            block,
            decode_ms: decode_time_ms,
            render_ms: render_time_ms,
            write_ms: write_time_ms,
        });
    }
}

/// `/state/loudness`, read from `control` when it is published: on the
/// telemetry thread, inside the publication lock (see
/// [`super::transport::publish_state`]), so it is never older than a snapshot
/// that went out before it.
pub(super) fn loudness_state_message(control: &RendererControl) -> OscMessage {
    let (enabled, source) = {
        let live = control.live.read();
        (live.options.use_loudness, live.dialogue_level)
    };
    let gain_linear: f32 = match (enabled, source) {
        (true, Some(dl)) => 10.0_f32.powf((-31 - dl as i32) as f32 / 20.0),
        _ => 1.0,
    };
    let payload = json!({
        "enabled": enabled,
        "source": source,
        "gain": gain_linear
    })
    .to_string();
    OscMessage {
        addr: osc_contract::STATE_LOUDNESS.to_string(),
        args: vec![OscType::String(payload)],
    }
}

pub(super) fn encode_diag_bundle(
    diag_schema_json: Option<String>,
    diag_values_json: Option<String>,
) -> Option<Vec<u8>> {
    let mut messages = Vec::with_capacity(2);
    if let Some(json) = diag_schema_json {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_DIAG_SCHEMA.to_string(),
            args: vec![OscType::String(json)],
        }));
    }
    if let Some(json) = diag_values_json {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_DIAG_VALUES.to_string(),
            args: vec![OscType::String(json)],
        }));
    }
    if messages.is_empty() {
        return None;
    }
    let bundle = OscPacket::Bundle(OscBundle {
        timetag: OscTime {
            seconds: 0,
            fractional: 1,
        },
        content: messages,
    });
    rosc::encoder::encode(&bundle).ok()
}

pub(super) fn encode_meter_bundle(report: &MeterReport) -> Result<Vec<u8>> {
    let MeterReport {
        snapshot,
        object_gains,
        object_band_gains,
        object_test_position,
        object_test_level,
        timings,
        ..
    } = report;
    let MeterTimings {
        decode_time_ms,
        crossover_time_ms,
        render_time_ms,
        write_time_ms,
        frame_duration_ms,
        latency_instant_ms,
        latency_control_ms,
        latency_smoothed_ms,
        latency_target_ms,
        latency_downstream_ms,
        latency_avail_input_ms,
        latency_output_fifo_ms,
        latency_resampler_pending_ms,
        resample_ratio,
        adaptive_band,
        adaptive_state,
        drc_gain,
    } = *timings;
    let object_test_position = *object_test_position;
    let object_test_level = *object_test_level;
    let max_gain_id = object_gains.iter().map(|(idx, _)| *idx).max().unwrap_or(0);
    let mut gains_by_id: Vec<Option<&renderer::spatial_vbap::Gains>> =
        vec![None; max_gain_id.saturating_add(1)];
    for (idx, g) in object_gains {
        if *idx < gains_by_id.len() {
            gains_by_id[*idx] = Some(g);
        }
    }

    let max_band_id = object_band_gains
        .iter()
        .map(|(idx, _)| *idx)
        .max()
        .unwrap_or(0);
    let mut band_gains_by_id: Vec<Option<&Vec<renderer::spatial_vbap::Gains>>> =
        vec![None; max_band_id.saturating_add(1)];
    for (idx, bg) in object_band_gains {
        if *idx < band_gains_by_id.len() {
            band_gains_by_id[*idx] = Some(bg);
        }
    }

    let mut messages =
        Vec::with_capacity(snapshot.object_levels.len() * 2 + snapshot.speaker_levels.len() + 1);
    if let Some(ms) = latency_control_ms
        .or(latency_instant_ms)
        .or(latency_target_ms)
    {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_LATENCY.to_string(),
            args: vec![OscType::Float(ms)],
        }));
    }
    if let Some(ms) = decode_time_ms {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_DECODE_TIME_MS.to_string(),
            args: vec![OscType::Float(ms.max(0.0))],
        }));
    }
    if let Some(ms) = render_time_ms {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_RENDER_TIME_MS.to_string(),
            args: vec![OscType::Float(ms.max(0.0))],
        }));
    }
    if let Some(ms) = crossover_time_ms {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_CROSSOVER_TIME_MS.to_string(),
            args: vec![OscType::Float(ms.max(0.0))],
        }));
    }
    if let Some(ms) = write_time_ms {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_WRITE_TIME_MS.to_string(),
            args: vec![OscType::Float(ms.max(0.0))],
        }));
    }
    if let Some(ms) = frame_duration_ms {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_FRAME_DURATION_MS.to_string(),
            args: vec![OscType::Float(ms.max(0.0))],
        }));
    }
    if let Some(ms) = latency_instant_ms {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_LATENCY_INSTANT.to_string(),
            args: vec![OscType::Float(ms)],
        }));
    }
    if let Some(ms) = latency_control_ms {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_LATENCY_CONTROL.to_string(),
            args: vec![OscType::Float(ms)],
        }));
    }
    if let Some(ms) = latency_smoothed_ms {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_LATENCY_SMOOTHED.to_string(),
            args: vec![OscType::Float(ms)],
        }));
    }
    if let Some(ms) = latency_target_ms {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_LATENCY_TARGET.to_string(),
            args: vec![OscType::Float(ms)],
        }));
    }
    if let Some(ms) = latency_downstream_ms {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_LATENCY_DOWNSTREAM.to_string(),
            args: vec![OscType::Float(ms)],
        }));
    }
    if let Some(ms) = latency_avail_input_ms {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_LATENCY_AVAIL_INPUT.to_string(),
            args: vec![OscType::Float(ms)],
        }));
    }
    if let Some(ms) = latency_output_fifo_ms {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_LATENCY_OUTPUT_FIFO.to_string(),
            args: vec![OscType::Float(ms)],
        }));
    }
    if let Some(ms) = latency_resampler_pending_ms {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_LATENCY_RESAMPLER_PENDING.to_string(),
            args: vec![OscType::Float(ms)],
        }));
    }
    if let Some(ratio) = resample_ratio {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_RESAMPLE_RATIO.to_string(),
            args: vec![OscType::Float(ratio)],
        }));
    }
    if let Some(band) = adaptive_band {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_ADAPTIVE_RESAMPLING_BAND.to_string(),
            args: vec![OscType::String(band.to_string())],
        }));
    }
    if let Some(state) = adaptive_state {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_ADAPTIVE_RESAMPLING_STATE.to_string(),
            args: vec![OscType::String(state.to_string())],
        }));
    }
    if let Some(gain) = drc_gain {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::METER_DRC_GAIN.to_string(),
            args: vec![OscType::Float(gain)],
        }));
    }

    // Per-crossover-band RMS rides the object message as EXTRA args after
    // (peak, rms) — an older client reads the first two and ignores the rest.
    let band_levels_by_id: std::collections::HashMap<u32, &Vec<f32>> = snapshot
        .object_band_levels
        .iter()
        .map(|(id, bands)| (*id, bands))
        .collect();
    for &(id, peak, rms) in &snapshot.object_levels {
        let mut args = vec![OscType::Float(peak), OscType::Float(rms)];
        if let Some(bands) = band_levels_by_id.get(&id) {
            args.extend(bands.iter().map(|&db| OscType::Float(db)));
        }
        messages.push(OscPacket::Message(OscMessage {
            addr: format!("/omniphony/meter/object/{}", id),
            args,
        }));
        if let Some(gains) = gains_by_id.get(id as usize).and_then(|entry| *entry) {
            messages.push(OscPacket::Message(OscMessage {
                addr: format!("/omniphony/meter/object/{}/gains", id),
                args: gains.iter().map(|&g| OscType::Float(g)).collect(),
            }));
        }
        if let Some(bands) = band_gains_by_id.get(id as usize).and_then(|entry| *entry) {
            for (b, bg) in bands.iter().enumerate() {
                messages.push(OscPacket::Message(OscMessage {
                    addr: format!("/omniphony/meter/object/{}/band/{}/gains", id, b),
                    args: bg.iter().map(|&g| OscType::Float(g)).collect(),
                }));
            }
        }
    }
    for (idx, &(peak, rms)) in snapshot.speaker_levels.iter().enumerate() {
        messages.push(OscPacket::Message(OscMessage {
            addr: format!("/omniphony/meter/speaker/{}", idx),
            args: vec![OscType::Float(peak), OscType::Float(rms)],
        }));
    }
    // Headphone L/R meters (binaural modes only): dedicated addresses —
    // in cascaded mode the speaker meters above carry the virtual buses.
    if let Some(ears) = snapshot.ear_levels {
        for (idx, &(peak, rms)) in ears.iter().enumerate() {
            messages.push(OscPacket::Message(OscMessage {
                addr: format!("/omniphony/meter/ear/{}", idx),
                args: vec![OscType::Float(peak), OscType::Float(rms)],
            }));
        }
    }
    if let Some([x, y, z]) = object_test_position {
        // Position and level in one message: they describe the same source
        // over the same interval, and a client that has one and not the
        // other can only draw something half true.
        let (peak, rms) = object_test_level.unwrap_or((-100.0, -100.0));
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_OBJECT_TEST_POSITION.to_string(),
            args: vec![
                OscType::Float(x),
                OscType::Float(y),
                OscType::Float(z),
                OscType::Float(peak),
                OscType::Float(rms),
            ],
        }));
    }
    messages.push(OscPacket::Message(OscMessage {
        addr: osc_contract::METER_MASTER.to_string(),
        args: vec![
            OscType::Float(snapshot.master_peak),
            OscType::Float(snapshot.master_rms),
        ],
    }));

    let bundle = OscPacket::Bundle(OscBundle {
        timetag: OscTime {
            seconds: 0,
            fractional: 1,
        },
        content: messages,
    });

    Ok(rosc::encoder::encode(&bundle)?)
}

pub(super) fn encode_timing_update(
    decode_time_ms: Option<f32>,
    render_time_ms: Option<f32>,
    write_time_ms: Option<f32>,
) -> Option<Vec<u8>> {
    let mut messages = Vec::new();
    if let Some(ms) = decode_time_ms {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_DECODE_TIME_MS.to_string(),
            args: vec![OscType::Float(ms.max(0.0))],
        }));
    }
    if let Some(ms) = render_time_ms {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_RENDER_TIME_MS.to_string(),
            args: vec![OscType::Float(ms.max(0.0))],
        }));
    }
    if let Some(ms) = write_time_ms {
        messages.push(OscPacket::Message(OscMessage {
            addr: osc_contract::STATE_WRITE_TIME_MS.to_string(),
            args: vec![OscType::Float(ms.max(0.0))],
        }));
    }
    if messages.is_empty() {
        return None;
    }
    let packet = OscPacket::Bundle(OscBundle {
        timetag: OscTime::from((0, 1)),
        content: messages,
    });
    rosc::encoder::encode(&packet).ok()
}
