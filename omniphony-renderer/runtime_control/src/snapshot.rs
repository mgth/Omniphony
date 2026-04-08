use std::sync::Arc;

use audio_input::{
    InputBackend, InputClockMode, InputControl, InputLfeMode, InputMapMode, InputMode,
    InputSampleFormat,
};
use audio_output::AudioControl;
use renderer::live_params::{LiveParams, RenderTopology, RendererControl};
use rosc::{OscBundle, OscMessage, OscPacket, OscTime, OscType};
use serde::Serialize;

fn input_mode_name(mode: InputMode) -> &'static str {
    match mode {
        InputMode::Bridge => "pipe_bridge",
        InputMode::Live => "pipewire",
        InputMode::PipewireBridge => "pipewire_bridge",
    }
}

fn input_backend_name(backend: InputBackend) -> &'static str {
    match backend {
        InputBackend::Pipewire => "pipewire",
        InputBackend::Asio => "asio",
    }
}

fn input_map_mode_name(mode: InputMapMode) -> &'static str {
    match mode {
        InputMapMode::SevenOneFixed => "7.1-fixed",
    }
}

fn input_lfe_mode_name(mode: InputLfeMode) -> &'static str {
    match mode {
        InputLfeMode::Object => "object",
        InputLfeMode::Direct => "direct",
        InputLfeMode::Drop => "drop",
    }
}

fn input_sample_format_name(format: InputSampleFormat) -> &'static str {
    match format {
        InputSampleFormat::F32 => "f32",
        InputSampleFormat::S16 => "s16",
    }
}

fn input_clock_mode_name(mode: InputClockMode) -> &'static str {
    match mode {
        InputClockMode::Dac => "dac",
        InputClockMode::Pipewire => "pipewire",
        InputClockMode::Upstream => "upstream",
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RenderBackendStateSnapshot {
    pub selection: String,
    pub effective: String,
    pub effective_label: String,
    pub capabilities: renderer::render_backend::BackendCapabilities,
    pub allowed_evaluation_modes: Vec<String>,
}

fn allowed_evaluation_modes(
    capabilities: renderer::render_backend::BackendCapabilities,
) -> Vec<String> {
    let mut modes = vec!["auto".to_string()];
    if capabilities.supports_realtime {
        modes.push("realtime".to_string());
    }
    if capabilities.supports_precomputed_polar {
        modes.push("precomputed_polar".to_string());
    }
    if capabilities.supports_precomputed_cartesian {
        modes.push("precomputed_cartesian".to_string());
    }
    modes
}

pub fn build_render_backend_state_snapshot(
    live: &LiveParams,
    active_topology: &RenderTopology,
) -> RenderBackendStateSnapshot {
    let backend = &active_topology.backend;
    let capabilities = backend.capabilities();
    RenderBackendStateSnapshot {
        selection: live.backend_id().to_string(),
        effective: backend.backend_id().to_string(),
        effective_label: backend.backend_label().to_string(),
        capabilities,
        allowed_evaluation_modes: allowed_evaluation_modes(capabilities),
    }
}

pub fn build_render_backend_state_json(
    live: &LiveParams,
    active_topology: &RenderTopology,
) -> String {
    serde_json::to_string(&build_render_backend_state_snapshot(live, active_topology))
        .unwrap_or_else(|_| "{}".to_string())
}

pub fn build_live_state_bundle(
    control: &Arc<RendererControl>,
    audio_control: Option<&Arc<AudioControl>>,
    input_control: Option<&Arc<InputControl>>,
) -> Vec<u8> {
    let live = control.live.read().unwrap();
    let radius_m = control.editable_layout().radius_m;
    let active_topology = control.active_topology();
    let effective_backend = active_topology.backend.kind().as_str();
    let effective_evaluation_mode = active_topology.backend.evaluation_mode().as_str();
    let render_backend_state_json = build_render_backend_state_json(&live, &active_topology);

    let mut messages = vec![
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_backend".to_string(),
            args: vec![OscType::String(live.backend_id().to_string())],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_backend/effective".to_string(),
            args: vec![OscType::String(effective_backend.to_string())],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_backend/state".to_string(),
            args: vec![OscType::String(render_backend_state_json)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation_mode".to_string(),
            args: vec![OscType::String(
                live.requested_evaluation_mode().as_str().to_string(),
            )],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation_mode/effective".to_string(),
            args: vec![OscType::String(effective_evaluation_mode.to_string())],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/gain".to_string(),
            args: vec![OscType::Float(live.master_gain)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/spread/min".to_string(),
            args: vec![OscType::Float(live.spread_min)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/spread/max".to_string(),
            args: vec![OscType::Float(live.spread_max)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/spread/from_distance".to_string(),
            args: vec![OscType::Int(if live.spread_from_distance { 1 } else { 0 })],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/spread/distance_range".to_string(),
            args: vec![OscType::Float(live.spread_distance_range)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/spread/distance_curve".to_string(),
            args: vec![OscType::Float(live.spread_distance_curve)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation/cartesian/x_size".to_string(),
            args: vec![OscType::Int(live.evaluation.cartesian.x_size as i32)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation/cartesian/y_size".to_string(),
            args: vec![OscType::Int(live.evaluation.cartesian.y_size as i32)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation/cartesian/z_size".to_string(),
            args: vec![OscType::Int(live.evaluation.cartesian.z_size as i32)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation/cartesian/z_neg_size".to_string(),
            args: vec![OscType::Int(live.evaluation.cartesian.z_neg_size as i32)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation/position_interpolation".to_string(),
            args: vec![OscType::Int(if live.evaluation.position_interpolation {
                1
            } else {
                0
            })],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/log_level".to_string(),
            args: vec![OscType::String(
                sys::live_log::current_runtime_level_name().to_string(),
            )],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/ramp_mode".to_string(),
            args: vec![OscType::String(live.ramp_mode.as_str().to_string())],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation/polar/azimuth_resolution".to_string(),
            args: vec![OscType::Int(live.evaluation.polar.azimuth_values.max(1))],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation/polar/elevation_resolution".to_string(),
            args: vec![OscType::Int(live.evaluation.polar.elevation_values.max(1))],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation/polar/distance_res".to_string(),
            args: vec![OscType::Int(live.evaluation.polar.distance_res.max(1))],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/render_evaluation/polar/distance_max".to_string(),
            args: vec![OscType::Float(live.evaluation.polar.distance_max.max(0.01))],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/vbap/allow_negative_z".to_string(),
            args: vec![OscType::Int(
                if control
                    .backend_rebuild_params
                    .map(|p| p.allow_negative_z)
                    .unwrap_or(true)
                {
                    1
                } else {
                    0
                },
            )],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/loudness".to_string(),
            args: vec![OscType::Int(if live.use_loudness { 1 } else { 0 })],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/distance_model".to_string(),
            args: vec![OscType::String(live.distance_model.to_string())],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/room_ratio".to_string(),
            args: vec![
                OscType::Float(live.room_ratio[0]),
                OscType::Float(live.room_ratio[1]),
                OscType::Float(live.room_ratio[2]),
            ],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/room_ratio_rear".to_string(),
            args: vec![OscType::Float(live.room_ratio_rear)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/room_ratio_lower".to_string(),
            args: vec![OscType::Float(live.room_ratio_lower)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/room_ratio_center_blend".to_string(),
            args: vec![OscType::Float(live.room_ratio_center_blend)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/distance_diffuse/enabled".to_string(),
            args: vec![OscType::Int(if live.use_distance_diffuse { 1 } else { 0 })],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/distance_diffuse/threshold".to_string(),
            args: vec![OscType::Float(live.distance_diffuse_threshold)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/distance_diffuse/curve".to_string(),
            args: vec![OscType::Float(live.distance_diffuse_curve)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/config/saved".to_string(),
            args: vec![OscType::Int(
                if control
                    .config_dirty
                    .load(std::sync::atomic::Ordering::Relaxed)
                {
                    0
                } else {
                    1
                },
            )],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/layout/radius_m".to_string(),
            args: vec![OscType::Float(radius_m)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/input_pipe".to_string(),
            args: vec![OscType::String(control.input_path().unwrap_or_default())],
        }),
    ];

    if let Some(audio_control) = audio_control {
        let requested = audio_control.requested_snapshot();
        let requested_output_device = requested.output_device.clone().unwrap_or_default();
        messages.extend([
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling".to_string(),
                args: vec![OscType::Int(if requested.adaptive_enabled { 1 } else { 0 })],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/enable_far_mode".to_string(),
                args: vec![OscType::Int(if requested.adaptive.enable_far_mode {
                    1
                } else {
                    0
                })],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/kp_near".to_string(),
                args: vec![OscType::Float(requested.adaptive.kp_near as f32)],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/ki".to_string(),
                args: vec![OscType::Float(requested.adaptive.ki as f32)],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/integral_discharge_ratio".to_string(),
                args: vec![OscType::Float(
                    requested.adaptive.integral_discharge_ratio as f32,
                )],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/max_adjust".to_string(),
                args: vec![OscType::Float(requested.adaptive.max_adjust as f32)],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/update_interval_callbacks".to_string(),
                args: vec![OscType::Float(
                    requested.adaptive.update_interval_callbacks as f32,
                )],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/near_far_threshold_ms".to_string(),
                args: vec![OscType::Float(
                    requested.adaptive.near_far_threshold_ms as f32,
                )],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/force_silence_in_far_mode".to_string(),
                args: vec![OscType::Int(
                    if requested.adaptive.force_silence_in_far_mode {
                        1
                    } else {
                        0
                    },
                )],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/hard_recover_high_in_far_mode"
                    .to_string(),
                args: vec![OscType::Int(
                    if requested.adaptive.hard_recover_high_in_far_mode {
                        1
                    } else {
                        0
                    },
                )],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/hard_recover_low_in_far_mode"
                    .to_string(),
                args: vec![OscType::Int(
                    if requested.adaptive.hard_recover_low_in_far_mode {
                        1
                    } else {
                        0
                    },
                )],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/far_mode_return_fade_in_ms".to_string(),
                args: vec![OscType::Float(
                    requested.adaptive.far_mode_return_fade_in_ms as f32,
                )],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/pause".to_string(),
                args: vec![OscType::Int(if requested.adaptive.paused { 1 } else { 0 })],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/audio/output_devices".to_string(),
                args: vec![OscType::String(
                    serde_json::to_string(&audio_control.available_output_devices())
                        .unwrap_or_else(|_| "[]".to_string()),
                )],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/audio/output_device".to_string(),
                args: vec![OscType::String(requested_output_device.clone())],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/audio/output_device/requested".to_string(),
                args: vec![OscType::String(requested_output_device)],
            }),
        ]);

        if let Some(effective_output_device) = audio_control.effective_output_device() {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/audio/output_device/effective".to_string(),
                args: vec![OscType::String(effective_output_device)],
            }));
        }

        if let Some(ms) = requested.latency_target_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/latency".to_string(),
                args: vec![OscType::Float(ms as f32)],
            }));
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/latency_target".to_string(),
                args: vec![OscType::Float(ms as f32)],
            }));
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/latency_target_requested".to_string(),
                args: vec![OscType::Float(ms as f32)],
            }));
        }
    }

    if let Some(input_control) = input_control {
        let requested = input_control.requested_snapshot();
        let applied = input_control.applied_snapshot();

        messages.extend([
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/input/mode".to_string(),
                args: vec![OscType::String(input_mode_name(requested.mode).to_string())],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/input/live/map".to_string(),
                args: vec![OscType::String(
                    input_map_mode_name(requested.map_mode).to_string(),
                )],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/input/live/lfe_mode".to_string(),
                args: vec![OscType::String(
                    input_lfe_mode_name(requested.lfe_mode).to_string(),
                )],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/input/live/clock_mode".to_string(),
                args: vec![OscType::String(
                    input_clock_mode_name(requested.clock_mode).to_string(),
                )],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/input/apply_pending".to_string(),
                args: vec![OscType::Int(if input_control.is_apply_pending() {
                    1
                } else {
                    0
                })],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/input/active_mode".to_string(),
                args: vec![OscType::String(
                    input_mode_name(applied.active_mode).to_string(),
                )],
            }),
        ]);

        if let Some(backend) = requested.backend {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/input/live/backend".to_string(),
                args: vec![OscType::String(input_backend_name(backend).to_string())],
            }));
        }
        if let Some(node_name) = requested.node_name {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/input/live/node".to_string(),
                args: vec![OscType::String(node_name)],
            }));
        }
        if let Some(description) = requested.node_description {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/input/live/description".to_string(),
                args: vec![OscType::String(description)],
            }));
        }
        if let Some(layout_path) = requested.layout_path {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/input/live/layout".to_string(),
                args: vec![OscType::String(layout_path.display().to_string())],
            }));
        }
        if let Some(channels) = requested.channels {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/input/live/channels".to_string(),
                args: vec![OscType::Int(channels as i32)],
            }));
        }
        if let Some(sample_rate_hz) = requested.sample_rate_hz {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/input/live/sample_rate".to_string(),
                args: vec![OscType::Int(sample_rate_hz as i32)],
            }));
        }
        if let Some(sample_format) = requested.sample_format {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/input/live/format".to_string(),
                args: vec![OscType::String(
                    input_sample_format_name(sample_format).to_string(),
                )],
            }));
        }

        if let Some(backend) = applied.backend {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/input/backend".to_string(),
                args: vec![OscType::String(input_backend_name(backend).to_string())],
            }));
        }
        if let Some(channels) = applied.channels {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/input/channels".to_string(),
                args: vec![OscType::Int(channels as i32)],
            }));
        }
        if let Some(sample_rate_hz) = applied.sample_rate_hz {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/input/sample_rate".to_string(),
                args: vec![OscType::Int(sample_rate_hz as i32)],
            }));
        }
        if let Some(node_name) = applied.node_name {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/input/node".to_string(),
                args: vec![OscType::String(node_name)],
            }));
        }
        if let Some(stream_format) = applied.stream_format {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/input/stream_format".to_string(),
                args: vec![OscType::String(stream_format)],
            }));
        }
        if let Some(error) = applied.input_error {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/input/error".to_string(),
                args: vec![OscType::String(error)],
            }));
        }
    }

    let mut all_messages = messages;

    for (&idx, obj) in &live.objects {
        if obj.gain != 1.0 {
            all_messages.push(OscPacket::Message(OscMessage {
                addr: format!("/omniphony/state/object/{}/gain", idx),
                args: vec![OscType::Float(obj.gain)],
            }));
        }
        if obj.muted {
            all_messages.push(OscPacket::Message(OscMessage {
                addr: format!("/omniphony/state/object/{}/mute", idx),
                args: vec![OscType::Int(1)],
            }));
        }
    }

    for (&idx, sp) in &live.speakers {
        if sp.gain != 1.0 {
            all_messages.push(OscPacket::Message(OscMessage {
                addr: format!("/omniphony/state/speaker/{}/gain", idx),
                args: vec![OscType::Float(sp.gain)],
            }));
        }
        if sp.delay_ms != 0.0 {
            all_messages.push(OscPacket::Message(OscMessage {
                addr: format!("/omniphony/state/speaker/{}/delay", idx),
                args: vec![OscType::Float(sp.delay_ms)],
            }));
        }
        if sp.muted {
            all_messages.push(OscPacket::Message(OscMessage {
                addr: format!("/omniphony/state/speaker/{}/mute", idx),
                args: vec![OscType::Int(1)],
            }));
        }
    }

    {
        let layout = control.editable_layout();
        for (idx, speaker) in layout.speakers.iter().enumerate() {
            all_messages.push(OscPacket::Message(OscMessage {
                addr: format!("/omniphony/state/speaker/{}/name", idx),
                args: vec![OscType::String(speaker.name.clone())],
            }));
        }
    }

    {
        let gain_linear: f32 = match (live.use_loudness, live.dialogue_level) {
            (true, Some(dl)) => 10.0_f32.powf((-31 - dl as i32) as f32 / 20.0),
            _ => 1.0,
        };
        all_messages.push(OscPacket::Message(OscMessage {
            addr: "/omniphony/state/loudness/gain".to_string(),
            args: vec![OscType::Float(gain_linear)],
        }));
    }
    if let Some(dl) = live.dialogue_level {
        all_messages.push(OscPacket::Message(OscMessage {
            addr: "/omniphony/state/loudness/source".to_string(),
            args: vec![OscType::Int(dl as i32)],
        }));
    }

    if let Some(audio_control) = audio_control {
        let (current_rate_opt, fmt) = audio_control.audio_state();
        let rate_opt = current_rate_opt.or_else(|| audio_control.requested_output_sample_rate());
        if let Some(effective_output_device) = audio_control.effective_output_device() {
            all_messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/audio/output_device/effective".to_string(),
                args: vec![OscType::String(effective_output_device)],
            }));
        }
        if let Some(rate) = rate_opt {
            all_messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/audio/sample_rate".to_string(),
                args: vec![OscType::Int(rate as i32)],
            }));
        }
        if !fmt.is_empty() {
            all_messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/audio/sample_format".to_string(),
                args: vec![OscType::String(fmt)],
            }));
        }
        if let Some(error) = audio_control.audio_error() {
            all_messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/audio/error".to_string(),
                args: vec![OscType::String(error)],
            }));
        }
    }

    all_messages.push(OscPacket::Message(OscMessage {
        addr: "/omniphony/state/snapshot_complete".to_string(),
        args: vec![OscType::Int(1)],
    }));

    let bundle = OscPacket::Bundle(OscBundle {
        timetag: OscTime {
            seconds: 0,
            fractional: 1,
        },
        content: all_messages,
    });

    rosc::encoder::encode(&bundle).unwrap_or_default()
}
