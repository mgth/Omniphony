use std::sync::Arc;

use audio_output::AudioControl;
use renderer::live_params::RendererControl;
use rosc::{OscBundle, OscMessage, OscPacket, OscTime, OscType};

pub fn build_live_state_bundle(
    control: &Arc<RendererControl>,
    audio_control: Option<&Arc<AudioControl>>,
) -> Vec<u8> {
    let live = control.live.read().unwrap();
    let radius_m = control.editable_layout().radius_m;
    let active_topology = control.active_topology();
    let effective_mode = match active_topology.vbap.table_mode() {
        renderer::spatial_vbap::VbapTableMode::Polar => "polar",
        renderer::spatial_vbap::VbapTableMode::Cartesian { .. } => "cartesian",
    };

    let mut messages = vec![
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
            addr: "/omniphony/state/vbap/cart/x_size".to_string(),
            args: vec![OscType::Int(live.vbap_cart_x_size as i32)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/vbap/cart/y_size".to_string(),
            args: vec![OscType::Int(live.vbap_cart_y_size as i32)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/vbap/cart/z_size".to_string(),
            args: vec![OscType::Int(live.vbap_cart_z_size as i32)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/vbap/cart/z_neg_size".to_string(),
            args: vec![OscType::Int(live.vbap_cart_z_neg_size as i32)],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/vbap/table_mode".to_string(),
            args: vec![OscType::String(live.vbap_table_mode.as_str().to_string())],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/vbap/position_interpolation".to_string(),
            args: vec![OscType::Int(if live.vbap_position_interpolation { 1 } else { 0 })],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/vbap/effective_mode".to_string(),
            args: vec![OscType::String(effective_mode.to_string())],
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
            addr: "/omniphony/state/vbap/polar/azimuth_resolution".to_string(),
            args: vec![OscType::Int(live.vbap_polar_azimuth_values.max(1))],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/vbap/polar/elevation_resolution".to_string(),
            args: vec![OscType::Int(live.vbap_polar_elevation_values.max(1))],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/vbap/polar/distance_res".to_string(),
            args: vec![OscType::Int(live.vbap_polar_distance_res.max(1))],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/vbap/polar/distance_max".to_string(),
            args: vec![OscType::Float(live.vbap_polar_distance_max.max(0.01))],
        }),
        OscPacket::Message(OscMessage {
            addr: "/omniphony/state/vbap/allow_negative_z".to_string(),
            args: vec![OscType::Int(
                if control
                    .vbap_rebuild_params
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
            args: vec![OscType::Int(if control.config_dirty.load(std::sync::atomic::Ordering::Relaxed) { 0 } else { 1 })],
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
        messages.extend([
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling".to_string(),
                args: vec![OscType::Int(if requested.adaptive_enabled { 1 } else { 0 })],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/enable_far_mode".to_string(),
                args: vec![OscType::Int(if requested.adaptive.enable_far_mode { 1 } else { 0 })],
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
                addr: "/omniphony/state/adaptive_resampling/max_adjust".to_string(),
                args: vec![OscType::Float(requested.adaptive.max_adjust as f32)],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/update_interval_callbacks".to_string(),
                args: vec![OscType::Float(requested.adaptive.update_interval_callbacks as f32)],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/near_far_threshold_ms".to_string(),
                args: vec![OscType::Float(requested.adaptive.near_far_threshold_ms as f32)],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/force_silence_in_far_mode".to_string(),
                args: vec![OscType::Int(if requested.adaptive.force_silence_in_far_mode { 1 } else { 0 })],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/hard_recover_in_far_mode".to_string(),
                args: vec![OscType::Int(if requested.adaptive.hard_recover_in_far_mode { 1 } else { 0 })],
            }),
            OscPacket::Message(OscMessage {
                addr: "/omniphony/state/adaptive_resampling/far_mode_return_fade_in_ms".to_string(),
                args: vec![OscType::Float(requested.adaptive.far_mode_return_fade_in_ms as f32)],
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
                args: vec![OscType::String(requested.output_device.unwrap_or_default())],
            }),
        ]);

        if let Some(ms) = requested.latency_target_ms {
            messages.push(OscPacket::Message(OscMessage {
                addr: "/omniphony/state/latency_target".to_string(),
                args: vec![OscType::Float(ms as f32)],
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

    let bundle = OscPacket::Bundle(OscBundle {
        timetag: OscTime {
            seconds: 0,
            fractional: 1,
        },
        content: all_messages,
    });

    rosc::encoder::encode(&bundle).unwrap_or_default()
}
