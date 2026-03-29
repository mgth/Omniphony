use anyhow::Result;
use rosc::{OscBundle, OscMessage, OscPacket, OscTime, OscType};

use super::OscSender;
use super::export::build_live_state_bundle;
use super::transport::{broadcast_float, broadcast_int};

impl OscSender {
    pub fn send_live_state_bundle(&self) -> Result<()> {
        let control = match self.control {
            Some(ref c) => c,
            None => return Ok(()),
        };
        let bytes = build_live_state_bundle(
            control,
            self.audio_control.as_ref(),
            self.input_control.as_ref(),
        );
        self.send_to_all(&bytes);
        Ok(())
    }

    pub fn send_loudness_state(&self) {
        let control = match self.control {
            Some(ref c) => c,
            None => return,
        };
        let live = control.live.read().unwrap();
        let socket = &self.socket;
        let clients = &self.clients;

        if let Some(dl) = live.dialogue_level {
            broadcast_int(
                socket,
                clients,
                "/omniphony/state/loudness/source",
                dl as i32,
            );
        }

        let gain_linear: f32 = match (live.use_loudness, live.dialogue_level) {
            (true, Some(dl)) => 10.0_f32.powf((-31 - dl as i32) as f32 / 20.0),
            _ => 1.0,
        };
        broadcast_float(
            socket,
            clients,
            "/omniphony/state/loudness/gain",
            gain_linear,
        );
    }

    pub fn send_meter_bundle(
        &self,
        snapshot: &renderer::metering::MeterSnapshot,
        object_gains: &[(usize, renderer::spatial_vbap::Gains)],
        decode_time_ms: Option<f32>,
        render_time_ms: Option<f32>,
        write_time_ms: Option<f32>,
        frame_duration_ms: Option<f32>,
        latency_instant_ms: Option<f32>,
        latency_control_ms: Option<f32>,
        latency_target_ms: Option<f32>,
        resample_ratio: Option<f32>,
        adaptive_band: Option<&str>,
        adaptive_state: Option<&str>,
    ) -> Result<()> {
        let max_gain_id = object_gains.iter().map(|(idx, _)| *idx).max().unwrap_or(0);
        let mut gains_by_id: Vec<Option<&renderer::spatial_vbap::Gains>> =
            vec![None; max_gain_id.saturating_add(1)];
        for (idx, g) in object_gains {
            if *idx < gains_by_id.len() {
                gains_by_id[*idx] = Some(g);
            }
        }

        let mut messages = Vec::with_capacity(
            snapshot.object_levels.len() * 2 + snapshot.speaker_levels.len() + 1,
        );
        let requested_latency_target_ms = self
            .audio_control
            .as_ref()
            .and_then(|control| control.requested_latency_target_ms())
            .map(|ms| ms as f32);

        if let Some(ms) = latency_target_ms.or(latency_instant_ms) {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/latency".to_string(),
                args: vec![OscType::Float(ms)],
            }));
        }
        if let Some(ms) = decode_time_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/decode_time_ms".to_string(),
                args: vec![OscType::Float(ms.max(0.0))],
            }));
        }
        if let Some(ms) = render_time_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/render_time_ms".to_string(),
                args: vec![OscType::Float(ms.max(0.0))],
            }));
        }
        if let Some(ms) = write_time_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/write_time_ms".to_string(),
                args: vec![OscType::Float(ms.max(0.0))],
            }));
        }
        if let Some(ms) = frame_duration_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/frame_duration_ms".to_string(),
                args: vec![OscType::Float(ms.max(0.0))],
            }));
        }
        if let Some(ms) = latency_instant_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/latency_instant".to_string(),
                args: vec![OscType::Float(ms)],
            }));
        }
        if let Some(ms) = latency_control_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/latency_control".to_string(),
                args: vec![OscType::Float(ms)],
            }));
        }
        if let Some(ms) = latency_target_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/latency_target".to_string(),
                args: vec![OscType::Float(ms)],
            }));
        }
        if let Some(ms) = requested_latency_target_ms.or(latency_target_ms) {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/latency_target_requested".to_string(),
                args: vec![OscType::Float(ms)],
            }));
        }
        if let Some(ratio) = resample_ratio {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/resample_ratio".to_string(),
                args: vec![OscType::Float(ratio)],
            }));
        }
        if let Some(band) = adaptive_band {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/band".to_string(),
                args: vec![OscType::String(band.to_string())],
            }));
        }
        if let Some(state) = adaptive_state {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/state".to_string(),
                args: vec![OscType::String(state.to_string())],
            }));
        }

        if let Some(ref control) = self.audio_control {
            let (current_rate_opt, fmt) = control.audio_state();
            let rate_opt = current_rate_opt.or_else(|| control.requested_output_sample_rate());
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/audio/output_device".to_string(),
                args: vec![OscType::String(
                    control.requested_output_device().unwrap_or_default(),
                )],
            }));
            if let Some(rate) = rate_opt {
                messages.push(OscPacket::Message(OscMessage {
                    addr: "/omniphony/state/audio/sample_rate".to_string(),
                    args: vec![OscType::Int(rate as i32)],
                }));
            }
            if !fmt.is_empty() {
                messages.push(OscPacket::Message(OscMessage {
                    addr: "/omniphony/state/audio/sample_format".to_string(),
                    args: vec![OscType::String(fmt)],
                }));
            }
            if let Some(error) = control.audio_error() {
                messages.push(OscPacket::Message(OscMessage {
                    addr: "/omniphony/state/audio/error".to_string(),
                    args: vec![OscType::String(error)],
                }));
            }
        }

        for &(id, peak, rms) in &snapshot.object_levels {
            messages.push(OscPacket::Message(OscMessage {
                addr: format!("/omniphony/meter/object/{}", id),
                args: vec![OscType::Float(peak), OscType::Float(rms)],
            }));
            if let Some(gains) = gains_by_id.get(id as usize).and_then(|entry| *entry) {
                messages.push(OscPacket::Message(OscMessage {
                    addr: format!("/omniphony/meter/object/{}/gains", id),
                    args: gains.iter().map(|&g| OscType::Float(g)).collect(),
                }));
            }
        }
        for (idx, &(peak, rms)) in snapshot.speaker_levels.iter().enumerate() {
            messages.push(OscPacket::Message(OscMessage {
                addr: format!("/omniphony/meter/speaker/{}", idx),
                args: vec![OscType::Float(peak), OscType::Float(rms)],
            }));
        }

        let bundle = OscPacket::Bundle(OscBundle {
            timetag: OscTime {
                seconds: 0,
                fractional: 1,
            },
            content: messages,
        });

        let bytes = rosc::encoder::encode(&bundle)?;
        self.send_to_metering_clients(&bytes);
        Ok(())
    }

    pub fn send_timing_update(
        &self,
        decode_time_ms: Option<f32>,
        render_time_ms: Option<f32>,
        write_time_ms: Option<f32>,
    ) -> Result<()> {
        let mut messages = Vec::new();
        if let Some(ms) = decode_time_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/decode_time_ms".to_string(),
                args: vec![OscType::Float(ms.max(0.0))],
            }));
        }
        if let Some(ms) = render_time_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/render_time_ms".to_string(),
                args: vec![OscType::Float(ms.max(0.0))],
            }));
        }
        if let Some(ms) = write_time_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/write_time_ms".to_string(),
                args: vec![OscType::Float(ms.max(0.0))],
            }));
        }
        if messages.is_empty() {
            return Ok(());
        }
        let packet = OscPacket::Bundle(OscBundle {
            timetag: OscTime::from((0, 1)),
            content: messages,
        });
        let bytes = rosc::encoder::encode(&packet)?;
        self.send_to_metering_clients(&bytes);
        Ok(())
    }

    pub fn send_audio_state(&self, sample_rate_hz: u32, sample_format: &str) -> Result<()> {
        let requested_output_device = self
            .audio_control
            .as_ref()
            .and_then(|control| control.requested_output_device())
            .unwrap_or_default();
        let audio_error = self
            .audio_control
            .as_ref()
            .and_then(|control| control.audio_error())
            .unwrap_or_default();
        let output_devices_json = self
            .audio_control
            .as_ref()
            .and_then(|control| serde_json::to_string(&control.available_output_devices()).ok())
            .unwrap_or_else(|| "[]".to_string());
        let announced_rate = self
            .audio_control
            .as_ref()
            .and_then(|control| control.requested_output_sample_rate())
            .unwrap_or(sample_rate_hz);
        let bundle = OscPacket::Bundle(OscBundle {
            timetag: OscTime {
                seconds: 0,
                fractional: 1,
            },
            content: vec![
                OscPacket::Message(OscMessage {
                    addr: "/omniphony/state/audio/output_devices".to_string(),
                    args: vec![OscType::String(output_devices_json)],
                }),
                OscPacket::Message(OscMessage {
                    addr: "/omniphony/state/audio/output_device".to_string(),
                    args: vec![OscType::String(requested_output_device)],
                }),
                OscPacket::Message(OscMessage {
                    addr: "/omniphony/state/audio/sample_rate".to_string(),
                    args: vec![OscType::Int(announced_rate as i32)],
                }),
                OscPacket::Message(OscMessage {
                    addr: "/omniphony/state/audio/sample_format".to_string(),
                    args: vec![OscType::String(sample_format.to_string())],
                }),
                OscPacket::Message(OscMessage {
                    addr: "/omniphony/state/audio/error".to_string(),
                    args: vec![OscType::String(audio_error)],
                }),
            ],
        });
        let bytes = rosc::encoder::encode(&bundle)?;
        self.send_to_all(&bytes);
        Ok(())
    }
}
