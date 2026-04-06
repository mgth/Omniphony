use rosc::{OscMessage, OscType};
use std::collections::HashMap;

use crate::context::RuntimeControlContext;
use audio_input::{
    InputBackend, InputClockMode, InputLfeMode, InputMapMode, InputMode, InputSampleFormat,
};
use renderer::render_backend::RenderBackendKind;
use renderer::live_params::LiveEvaluationMode;

#[derive(Debug, Clone, Default)]
pub struct SpeakerPatch {
    pub az: Option<f32>,
    pub el: Option<f32>,
    pub distance: Option<f32>,
    pub x: Option<f32>,
    pub y: Option<f32>,
    pub z: Option<f32>,
    pub coord_mode: Option<String>,
    pub spatialize: Option<bool>,
    pub name: Option<String>,
}

#[derive(Debug, Clone)]
pub enum BroadcastValue {
    Int(i32),
    Float(f32),
    Fff(f32, f32, f32),
    String(String),
}

#[derive(Debug, Clone)]
pub struct BroadcastUpdate {
    pub addr: String,
    pub value: BroadcastValue,
}

#[derive(Debug, Clone, Default)]
pub struct ControlEffects {
    pub mark_dirty: bool,
    pub trigger_layout_recompute: bool,
    pub speaker_layout_broadcast: Option<renderer::speaker_layout::SpeakerLayout>,
    pub broadcasts: Vec<BroadcastUpdate>,
    pub log_message: Option<String>,
}

fn parse_bool_arg(arg: Option<&OscType>) -> Option<bool> {
    match arg {
        Some(OscType::Int(i)) => Some(*i != 0),
        Some(OscType::Float(f)) => Some(*f != 0.0),
        _ => None,
    }
}

fn parse_positive_u32_arg(arg: Option<&OscType>) -> Option<u32> {
    match arg {
        Some(OscType::Int(i)) if *i > 0 => Some(*i as u32),
                    Some(OscType::Float(f)) if *f > 0.0 => Some(*f as u32),
            _ => None,
    }
}

fn parse_nonnegative_u32_arg(arg: Option<&OscType>) -> Option<u32> {
    match arg {
        Some(OscType::Int(i)) if *i >= 0 => Some(*i as u32),
        Some(OscType::Float(f)) if *f >= 0.0 => Some(*f as u32),
        _ => None,
    }
}

fn parse_positive_f32_arg(arg: Option<&OscType>) -> Option<f32> {
    match arg {
        Some(OscType::Float(f)) if *f > 0.0 => Some(*f),
        Some(OscType::Int(i)) if *i > 0 => Some(*i as f32),
        _ => None,
    }
}

fn parse_nonnegative_f32_arg(arg: Option<&OscType>) -> Option<f32> {
    match arg {
        Some(OscType::Float(f)) if *f >= 0.0 => Some(*f),
        Some(OscType::Int(i)) if *i >= 0 => Some(*i as f32),
        _ => None,
    }
}

fn parse_f32_arg(arg: Option<&OscType>) -> Option<f32> {
    match arg {
        Some(OscType::Float(f)) => Some(*f),
        Some(OscType::Int(i)) => Some(*i as f32),
        _ => None,
    }
}

fn parse_string_arg(arg: Option<&OscType>) -> Option<String> {
    match arg {
        Some(OscType::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        _ => None,
    }
}

fn parse_input_layout_arg(arg: Option<&OscType>) -> Option<renderer::speaker_layout::SpeakerLayout> {
    let raw = parse_string_arg(arg)?;
    serde_yaml_ng::from_str::<renderer::speaker_layout::SpeakerLayout>(&raw).ok()
}

fn remap_live_speakers_remove(
    speakers: &mut std::collections::HashMap<usize, renderer::live_params::SpeakerLiveParams>,
    remove_idx: usize,
) {
    let mut next = std::collections::HashMap::new();
    for (idx, params) in speakers.drain() {
        if idx == remove_idx {
            continue;
        }
        let mapped = if idx > remove_idx { idx - 1 } else { idx };
        next.insert(mapped, params);
    }
    *speakers = next;
}

fn remap_live_speakers_move(
    speakers: &mut std::collections::HashMap<usize, renderer::live_params::SpeakerLiveParams>,
    from: usize,
    to: usize,
) {
    if from == to {
        return;
    }
    let moved = speakers.remove(&from);
    let mut next = std::collections::HashMap::new();
    for (idx, params) in speakers.drain() {
        let mapped = if from < to {
            if idx > from && idx <= to {
                idx - 1
            } else {
                idx
            }
        } else if idx >= to && idx < from {
            idx + 1
        } else {
            idx
        };
        next.insert(mapped, params);
    }
    if let Some(params) = moved {
        next.insert(to, params);
    }
    *speakers = next;
}

fn apply_pending_speakers(
    pending: &mut HashMap<usize, SpeakerPatch>,
    ctx: &RuntimeControlContext,
) -> renderer::speaker_layout::SpeakerLayout {
    let layout = ctx.renderer.with_editable_layout(|layout| {
        for (idx, patch) in pending.iter() {
            if let Some(speaker) = layout.speakers.get_mut(*idx) {
                if let Some(az) = patch.az {
                    speaker.azimuth = az;
                }
                if let Some(el) = patch.el {
                    speaker.elevation = el;
                }
                if let Some(dist) = patch.distance {
                    speaker.distance = dist;
                }
                if let Some(x) = patch.x {
                    speaker.x = x.clamp(-1.0, 1.0);
                }
                if let Some(y) = patch.y {
                    speaker.y = y.clamp(-1.0, 1.0);
                }
                if let Some(z) = patch.z {
                    speaker.z = z.clamp(-1.0, 1.0);
                }
                if let Some(coord_mode) = &patch.coord_mode {
                    speaker.coord_mode = if coord_mode.eq_ignore_ascii_case("cartesian") {
                        "cartesian".to_string()
                    } else {
                        "polar".to_string()
                    };
                }
                if let Some(spatialize) = patch.spatialize {
                    speaker.spatialize = spatialize;
                }
                if let Some(name) = &patch.name {
                    speaker.name = name.clone();
                }
            }
        }
        layout.clone()
    });
    pending.clear();
    layout
}

pub fn apply_simple_osc_control(
    msg: &OscMessage,
    ctx: &RuntimeControlContext,
) -> Option<ControlEffects> {
    let addr = msg.addr.as_str();
    let mut effects = ControlEffects::default();

    if addr == "/omniphony/control/audio/output_devices/refresh" {
        if let Some(audio) = ctx.audio.as_ref() {
            if let Some(devices) = audio.refresh_available_output_devices() {
                let json = serde_json::to_string(&devices).unwrap_or_else(|_| "[]".to_string());
                effects.broadcasts.push(BroadcastUpdate {
                    addr: "/omniphony/state/audio/output_devices".to_string(),
                    value: BroadcastValue::String(json),
                });
                effects.log_message =
                    Some(format!("OSC: output_devices/refresh → {} device(s)", devices.len()));
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/audio/output_device" {
        let requested = msg.args.first().and_then(|arg| match arg {
            OscType::String(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            }
            _ => None,
        });
        if let Some(audio) = ctx.audio.as_ref() {
            audio.set_requested_output_device(requested.clone());
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/audio/output_device".to_string(),
                value: BroadcastValue::String(requested.unwrap_or_default()),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/render_backend" {
        let requested =
            parse_string_arg(msg.args.first()).and_then(|value| RenderBackendKind::from_str(&value));
        if let Some(requested) = requested {
            let mut live = ctx.renderer.live.write().unwrap();
            if live.backend_kind != requested {
                live.backend_kind = requested;
                effects.mark_dirty = true;
                effects.trigger_layout_recompute = true;
                effects.broadcasts.push(BroadcastUpdate {
                    addr: "/omniphony/state/render_backend".to_string(),
                    value: BroadcastValue::String(requested.as_str().to_string()),
                });
                effects.log_message =
                    Some(format!("OSC: render_backend -> {}", requested.as_str()));
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/render_evaluation_mode" {
        let requested =
            parse_string_arg(msg.args.first()).and_then(|value| LiveEvaluationMode::from_str(&value));
        if let Some(requested) = requested {
            let mut live = ctx.renderer.live.write().unwrap();
            let accepted = match live.backend_kind {
                RenderBackendKind::Vbap => {
                    if requested != LiveEvaluationMode::Realtime {
                        if live.evaluation.mode != requested {
                            live.set_evaluation_mode(requested);
                            effects.mark_dirty = true;
                            effects.trigger_layout_recompute = true;
                        }
                        true
                    } else {
                        false
                    }
                }
                RenderBackendKind::ExperimentalDistance => requested == LiveEvaluationMode::Realtime,
            };
            if accepted {
                if live.backend_kind == RenderBackendKind::Vbap {
                    effects.mark_dirty = true;
                }
                effects.broadcasts.push(BroadcastUpdate {
                    addr: "/omniphony/state/render_evaluation_mode".to_string(),
                    value: BroadcastValue::String(
                        live.requested_evaluation_mode().as_str().to_string(),
                    ),
                });
                effects.log_message = Some(format!(
                    "OSC: render_evaluation_mode -> {}",
                    live.requested_evaluation_mode().as_str()
                ));
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/input/mode" {
        let requested = parse_string_arg(msg.args.first()).and_then(|value| {
            match value.to_ascii_lowercase().as_str() {
                "bridge" | "pipe_bridge" => Some(InputMode::Bridge),
                "live" | "pipewire" => Some(InputMode::Live),
                "pipewire_bridge" => Some(InputMode::PipewireBridge),
                _ => None,
            }
        });
        if let (Some(input), Some(requested)) = (ctx.input.as_ref(), requested) {
            input.set_requested_mode(requested);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/input/mode".to_string(),
                value: BroadcastValue::String(match requested {
                    InputMode::Bridge => "pipe_bridge".to_string(),
                    InputMode::Live => "pipewire".to_string(),
                    InputMode::PipewireBridge => "pipewire_bridge".to_string(),
                }),
            });
            effects.log_message = Some(format!(
                "OSC: input mode staged → {}",
                match requested {
                    InputMode::Bridge => "pipe_bridge",
                    InputMode::Live => "pipewire",
                    InputMode::PipewireBridge => "pipewire_bridge",
                }
            ));
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/input/live/backend" {
        let requested = parse_string_arg(msg.args.first()).and_then(|value| {
            match value.to_ascii_lowercase().as_str() {
                "pipewire" => Some(InputBackend::Pipewire),
                "asio" => Some(InputBackend::Asio),
                _ => None,
            }
        });
        if let (Some(input), Some(requested)) = (ctx.input.as_ref(), requested) {
            input.set_requested_backend(Some(requested));
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/input/live/backend".to_string(),
                value: BroadcastValue::String(match requested {
                    InputBackend::Pipewire => "pipewire".to_string(),
                    InputBackend::Asio => "asio".to_string(),
                }),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/input/live/node" {
        let requested = parse_string_arg(msg.args.first());
        if let Some(input) = ctx.input.as_ref() {
            input.set_requested_node_name(requested.clone());
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/input/live/node".to_string(),
                value: BroadcastValue::String(requested.unwrap_or_default()),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/input/live/description" {
        let requested = parse_string_arg(msg.args.first());
        if let Some(input) = ctx.input.as_ref() {
            input.set_requested_node_description(requested.clone());
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/input/live/description".to_string(),
                value: BroadcastValue::String(requested.unwrap_or_default()),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/input/live/layout" {
        let requested = parse_string_arg(msg.args.first()).map(std::path::PathBuf::from);
        if let Some(input) = ctx.input.as_ref() {
            let state_value = requested
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            input.set_requested_layout_path(requested);
            input.set_requested_current_layout(None);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/input/live/layout".to_string(),
                value: BroadcastValue::String(state_value),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/input/live/layout_import" {
        let requested = parse_input_layout_arg(msg.args.first());
        if let Some(input) = ctx.input.as_ref() {
            input.set_requested_current_layout(requested);
            effects.mark_dirty = true;
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/input/live/channels" {
        let requested = match msg.args.first() {
            Some(OscType::Int(i)) if *i > 0 => Some(*i as u16),
            Some(OscType::Float(f)) if *f > 0.0 => Some(*f as u16),
            _ => None,
        };
        if let (Some(input), Some(requested)) = (ctx.input.as_ref(), requested) {
            input.set_requested_channels(Some(requested));
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/input/live/channels".to_string(),
                value: BroadcastValue::Int(requested as i32),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/input/live/sample_rate" {
        let requested = parse_positive_u32_arg(msg.args.first());
        if let (Some(input), Some(requested)) = (ctx.input.as_ref(), requested) {
            input.set_requested_sample_rate_hz(Some(requested));
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/input/live/sample_rate".to_string(),
                value: BroadcastValue::Int(requested as i32),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/input/live/format" {
        let requested = parse_string_arg(msg.args.first()).and_then(|value| {
            match value.to_ascii_lowercase().as_str() {
                "f32" => Some(InputSampleFormat::F32),
                "s16" => Some(InputSampleFormat::S16),
                _ => None,
            }
        });
        if let (Some(input), Some(requested)) = (ctx.input.as_ref(), requested) {
            input.set_requested_sample_format(Some(requested));
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/input/live/format".to_string(),
                value: BroadcastValue::String(match requested {
                    InputSampleFormat::F32 => "f32".to_string(),
                    InputSampleFormat::S16 => "s16".to_string(),
                }),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/input/live/map" {
        let requested = parse_string_arg(msg.args.first()).and_then(|value| {
            match value.to_ascii_lowercase().as_str() {
                "7.1-fixed" => Some(InputMapMode::SevenOneFixed),
                _ => None,
            }
        });
        if let (Some(input), Some(requested)) = (ctx.input.as_ref(), requested) {
            input.set_requested_map_mode(requested);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/input/live/map".to_string(),
                value: BroadcastValue::String("7.1-fixed".to_string()),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/input/live/lfe_mode" {
        let requested = parse_string_arg(msg.args.first()).and_then(|value| {
            match value.to_ascii_lowercase().as_str() {
                "object" => Some(InputLfeMode::Object),
                "direct" => Some(InputLfeMode::Direct),
                "drop" => Some(InputLfeMode::Drop),
                _ => None,
            }
        });
        if let (Some(input), Some(requested)) = (ctx.input.as_ref(), requested) {
            input.set_requested_lfe_mode(requested);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/input/live/lfe_mode".to_string(),
                value: BroadcastValue::String(match requested {
                    InputLfeMode::Object => "object".to_string(),
                    InputLfeMode::Direct => "direct".to_string(),
                    InputLfeMode::Drop => "drop".to_string(),
                }),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/input/live/clock_mode" {
        let requested = parse_string_arg(msg.args.first()).and_then(|value| {
            match value.to_ascii_lowercase().as_str() {
                "dac" => Some(InputClockMode::Dac),
                "pipewire" => Some(InputClockMode::Pipewire),
                "upstream" => Some(InputClockMode::Upstream),
                _ => None,
            }
        });
        if let (Some(input), Some(requested)) = (ctx.input.as_ref(), requested) {
            input.set_requested_clock_mode(requested);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/input/live/clock_mode".to_string(),
                value: BroadcastValue::String(match requested {
                    InputClockMode::Dac => "dac".to_string(),
                    InputClockMode::Pipewire => "pipewire".to_string(),
                    InputClockMode::Upstream => "upstream".to_string(),
                }),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/input/apply" {
        if let Some(input) = ctx.input.as_ref() {
            input.request_apply();
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/input/apply_pending".to_string(),
                value: BroadcastValue::Int(1),
            });
            effects.log_message = Some("OSC: input apply requested".to_string());
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/ramp_mode" {
        let Some(mode) = msg.args.first().and_then(|arg| match arg {
            OscType::String(s) => renderer::live_params::RampMode::from_str(s),
            _ => None,
        }) else {
            return Some(effects);
        };
        ctx.renderer.set_requested_ramp_mode(mode);
        ctx.renderer.live.write().unwrap().ramp_mode = mode;
        effects.mark_dirty = true;
        effects.broadcasts.push(BroadcastUpdate {
            addr: "/omniphony/state/ramp_mode".to_string(),
            value: BroadcastValue::String(mode.as_str().to_string()),
        });
        effects.log_message = Some(format!("OSC: ramp_mode → {}", mode.as_str()));
        return Some(effects);
    }

    if addr == "/omniphony/control/audio/sample_rate" {
        let requested_hz = match msg.args.first() {
            Some(OscType::Int(i)) if *i > 0 => Some(*i as u32),
            Some(OscType::Float(f)) if *f > 0.0 => Some(*f as u32),
            Some(OscType::Int(_)) | Some(OscType::Float(_)) => None,
            _ => None,
        };
        if let Some(audio) = ctx.audio.as_ref() {
            audio.set_requested_output_sample_rate(requested_hz);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/audio/sample_rate".to_string(),
                value: BroadcastValue::Int(requested_hz.unwrap_or(0) as i32),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/adaptive_resampling" {
        let enabled = parse_bool_arg(msg.args.first());
        if let (Some(audio), Some(enabled)) = (ctx.audio.as_ref(), enabled) {
            audio.set_requested_adaptive_resampling(enabled);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/adaptive_resampling".to_string(),
                value: BroadcastValue::Int(if enabled { 1 } else { 0 }),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/adaptive_resampling/enable_far_mode" {
        let enabled = parse_bool_arg(msg.args.first());
        if let (Some(audio), Some(enabled)) = (ctx.audio.as_ref(), enabled) {
            audio.set_requested_adaptive_resampling_enable_far_mode(enabled);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/adaptive_resampling/enable_far_mode".to_string(),
                value: BroadcastValue::Int(if enabled { 1 } else { 0 }),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/adaptive_resampling/force_silence_in_far_mode" {
        let enabled = parse_bool_arg(msg.args.first());
        if let (Some(audio), Some(enabled)) = (ctx.audio.as_ref(), enabled) {
            audio.set_requested_adaptive_resampling_force_silence_in_far_mode(enabled);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/adaptive_resampling/force_silence_in_far_mode".to_string(),
                value: BroadcastValue::Int(if enabled { 1 } else { 0 }),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/adaptive_resampling/hard_recover_high_in_far_mode"
        || addr == "/omniphony/control/adaptive_resampling/hard_recover_in_far_mode"
    {
        let enabled = parse_bool_arg(msg.args.first());
        if let (Some(audio), Some(enabled)) = (ctx.audio.as_ref(), enabled) {
            audio.set_requested_adaptive_resampling_hard_recover_high_in_far_mode(enabled);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/adaptive_resampling/hard_recover_high_in_far_mode"
                    .to_string(),
                value: BroadcastValue::Int(if enabled { 1 } else { 0 }),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/adaptive_resampling/hard_recover_low_in_far_mode" {
        let enabled = parse_bool_arg(msg.args.first());
        if let (Some(audio), Some(enabled)) = (ctx.audio.as_ref(), enabled) {
            audio.set_requested_adaptive_resampling_hard_recover_low_in_far_mode(enabled);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/adaptive_resampling/hard_recover_low_in_far_mode"
                    .to_string(),
                value: BroadcastValue::Int(if enabled { 1 } else { 0 }),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/adaptive_resampling/far_mode_return_fade_in_ms" {
        let value = parse_nonnegative_u32_arg(msg.args.first());
        if let (Some(audio), Some(value)) = (ctx.audio.as_ref(), value) {
            audio.set_requested_adaptive_resampling_far_mode_return_fade_in_ms(value);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/adaptive_resampling/far_mode_return_fade_in_ms"
                    .to_string(),
                value: BroadcastValue::Float(value as f32),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/adaptive_resampling/kp_near" {
        let value = parse_positive_f32_arg(msg.args.first());
        if let (Some(audio), Some(value)) = (ctx.audio.as_ref(), value) {
            audio.set_requested_adaptive_resampling_kp_near(value);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/adaptive_resampling/kp_near".to_string(),
                value: BroadcastValue::Float(value),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/adaptive_resampling/ki" {
        let value = parse_positive_f32_arg(msg.args.first());
        if let (Some(audio), Some(value)) = (ctx.audio.as_ref(), value) {
            audio.set_requested_adaptive_resampling_ki(value);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/adaptive_resampling/ki".to_string(),
                value: BroadcastValue::Float(value),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/adaptive_resampling/integral_discharge_ratio" {
        let value = parse_nonnegative_f32_arg(msg.args.first()).map(|v| v.min(1.0));
        if let (Some(audio), Some(value)) = (ctx.audio.as_ref(), value) {
            audio.set_requested_adaptive_resampling_integral_discharge_ratio(value);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/adaptive_resampling/integral_discharge_ratio".to_string(),
                value: BroadcastValue::Float(value),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/adaptive_resampling/max_adjust" {
        let value = parse_positive_f32_arg(msg.args.first());
        if let (Some(audio), Some(value)) = (ctx.audio.as_ref(), value) {
            audio.set_requested_adaptive_resampling_max_adjust(value);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/adaptive_resampling/max_adjust".to_string(),
                value: BroadcastValue::Float(value),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/adaptive_resampling/update_interval_callbacks" {
        let value = parse_positive_u32_arg(msg.args.first());
        if let (Some(audio), Some(value)) = (ctx.audio.as_ref(), value) {
            audio.set_requested_adaptive_resampling_update_interval_callbacks(value);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/adaptive_resampling/update_interval_callbacks"
                    .to_string(),
                value: BroadcastValue::Float(value as f32),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/adaptive_resampling/near_far_threshold_ms" {
        let value = parse_positive_u32_arg(msg.args.first());
        if let (Some(audio), Some(value)) = (ctx.audio.as_ref(), value) {
            audio.set_requested_adaptive_resampling_near_far_threshold_ms(value);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/adaptive_resampling/near_far_threshold_ms".to_string(),
                value: BroadcastValue::Float(value as f32),
            });
        }
        return Some(effects);
    }


    if addr == "/omniphony/control/adaptive_resampling/pause" {
        let paused = parse_bool_arg(msg.args.first());
        if let (Some(audio), Some(paused)) = (ctx.audio.as_ref(), paused) {
            audio.set_requested_adaptive_resampling_paused(paused);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/adaptive_resampling/pause".to_string(),
                value: BroadcastValue::Int(if paused { 1 } else { 0 }),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/adaptive_resampling/reset_ratio" {
        if let Some(audio) = ctx.audio.as_ref() {
            audio.request_ratio_reset();
            effects.mark_dirty = true;
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/latency_target" {
        let latency_ms = parse_positive_u32_arg(msg.args.first());
        if let (Some(audio), Some(latency_ms)) = (ctx.audio.as_ref(), latency_ms) {
            audio.set_requested_latency_target_ms(Some(latency_ms));
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/latency_target".to_string(),
                value: BroadcastValue::Float(latency_ms as f32),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/layout/radius_m" {
        if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.max(0.01)) {
            ctx.renderer.with_editable_layout(|layout| layout.radius_m = v);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/layout/radius_m".to_string(),
                value: BroadcastValue::Float(v),
            });
            effects.log_message = Some(format!("OSC: layout radius_m → {}", v));
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/gain" {
        if let Some(gain) = parse_f32_arg(msg.args.first()) {
            ctx.renderer.live.write().unwrap().master_gain = gain;
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/gain".to_string(),
                value: BroadcastValue::Float(gain),
            });
        }
        return Some(effects);
    }

    macro_rules! layout_float_with_recompute {
        ($path:literal, $field:ident, $state:literal) => {
            if addr == $path {
                if let Some(value) = parse_f32_arg(msg.args.first()) {
                    ctx.renderer.live.write().unwrap().$field = value;
                    effects.mark_dirty = true;
                    effects.trigger_layout_recompute = true;
                    effects.broadcasts.push(BroadcastUpdate {
                        addr: $state.to_string(),
                        value: BroadcastValue::Float(value),
                    });
                }
                return Some(effects);
            }
        };
    }

    layout_float_with_recompute!(
        "/omniphony/control/spread/min",
        spread_min,
        "/omniphony/state/spread/min"
    );
    layout_float_with_recompute!(
        "/omniphony/control/spread/max",
        spread_max,
        "/omniphony/state/spread/max"
    );
    layout_float_with_recompute!(
        "/omniphony/control/spread/distance_range",
        spread_distance_range,
        "/omniphony/state/spread/distance_range"
    );
    layout_float_with_recompute!(
        "/omniphony/control/spread/distance_curve",
        spread_distance_curve,
        "/omniphony/state/spread/distance_curve"
    );

    if addr == "/omniphony/control/spread/from_distance" {
        if let Some(v) = parse_bool_arg(msg.args.first()) {
            ctx.renderer.live.write().unwrap().spread_from_distance = v;
            effects.mark_dirty = true;
            effects.trigger_layout_recompute = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/spread/from_distance".to_string(),
                value: BroadcastValue::Int(if v { 1 } else { 0 }),
            });
        }
        return Some(effects);
    }

    if let Some(rest) = addr
        .strip_prefix("/omniphony/control/vbap/cart/")
        .or_else(|| addr.strip_prefix("/omniphony/control/render_evaluation/cartesian/"))
    {
        let size = match msg.args.first() {
            Some(OscType::Int(i)) => Some((*i).max(1) as usize),
            Some(OscType::Float(f)) => Some((*f).round().max(1.0) as usize),
            _ => None,
        };
        if let Some(size) = size {
            let (legacy_state_addr, evaluation_state_addr) = match rest {
                "x_size" => {
                    ctx.renderer.live.write().unwrap().evaluation.cartesian.x_size = size;
                    (
                        Some("/omniphony/state/vbap/cart/x_size"),
                        Some("/omniphony/state/render_evaluation/cartesian/x_size"),
                    )
                }
                "y_size" => {
                    ctx.renderer.live.write().unwrap().evaluation.cartesian.y_size = size;
                    (
                        Some("/omniphony/state/vbap/cart/y_size"),
                        Some("/omniphony/state/render_evaluation/cartesian/y_size"),
                    )
                }
                "z_size" => {
                    ctx.renderer.live.write().unwrap().evaluation.cartesian.z_size = size;
                    (
                        Some("/omniphony/state/vbap/cart/z_size"),
                        Some("/omniphony/state/render_evaluation/cartesian/z_size"),
                    )
                }
                "z_neg_size" => {
                    ctx.renderer.live.write().unwrap().evaluation.cartesian.z_neg_size = size;
                    (
                        Some("/omniphony/state/vbap/cart/z_neg_size"),
                        Some("/omniphony/state/render_evaluation/cartesian/z_neg_size"),
                    )
                }
                _ => (None, None),
            };
            if let Some(state_addr) = legacy_state_addr {
                effects.mark_dirty = true;
                effects.trigger_layout_recompute = true;
                effects.broadcasts.push(BroadcastUpdate {
                    addr: state_addr.to_string(),
                    value: BroadcastValue::Int(size as i32),
                });
            }
            if let Some(state_addr) = evaluation_state_addr {
                effects.broadcasts.push(BroadcastUpdate {
                    addr: state_addr.to_string(),
                    value: BroadcastValue::Int(size as i32),
                });
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/vbap/table_mode" {
        if let Some(OscType::String(mode)) = msg.args.first() {
            if let Some(mode) = renderer::live_params::LiveVbapTableMode::from_str(mode) {
                let evaluation_mode =
                    renderer::live_params::LiveEvaluationMode::from_vbap_table_mode(mode);
                ctx.renderer.live.write().unwrap().set_evaluation_mode(evaluation_mode);
                effects.mark_dirty = true;
                effects.trigger_layout_recompute = true;
                effects.broadcasts.push(BroadcastUpdate {
                    addr: "/omniphony/state/vbap/table_mode".to_string(),
                    value: BroadcastValue::String(mode.as_str().to_string()),
                });
                effects.broadcasts.push(BroadcastUpdate {
                    addr: "/omniphony/state/render_evaluation_mode".to_string(),
                    value: BroadcastValue::String(evaluation_mode.as_str().to_string()),
                });
                effects.log_message = Some(format!(
                    "OSC: vbap/table_mode -> {} (legacy; render_evaluation_mode={})",
                    mode.as_str(),
                    evaluation_mode.as_str()
                ));
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/vbap/position_interpolation" {
        if let Some(enabled) = parse_bool_arg(msg.args.first()) {
            ctx.renderer.live.write().unwrap().vbap_position_interpolation = enabled;
            effects.mark_dirty = true;
            effects.trigger_layout_recompute = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/vbap/position_interpolation".to_string(),
                value: BroadcastValue::Int(if enabled { 1 } else { 0 }),
            });
        }
        return Some(effects);
    }

    if let Some(rest) = addr
        .strip_prefix("/omniphony/control/vbap/polar/")
        .or_else(|| addr.strip_prefix("/omniphony/control/render_evaluation/polar/"))
    {
        match rest {
            "azimuth_resolution" | "elevation_resolution" => {
                let res = match msg.args.first() {
                    Some(OscType::Int(i)) => Some((*i).max(1)),
                    Some(OscType::Float(f)) => Some((*f as i32).max(1)),
                    _ => None,
                };
                if let Some(res) = res {
                    let (legacy_state_addr, evaluation_state_addr) = match rest {
                        "azimuth_resolution" => {
                            ctx.renderer.live.write().unwrap().evaluation.polar.azimuth_values =
                                res;
                            (
                                Some("/omniphony/state/vbap/polar/azimuth_resolution"),
                                Some("/omniphony/state/render_evaluation/polar/azimuth_resolution"),
                            )
                        }
                        "elevation_resolution" => {
                            ctx.renderer.live.write().unwrap().evaluation.polar.elevation_values =
                                res;
                            (
                                Some("/omniphony/state/vbap/polar/elevation_resolution"),
                                Some(
                                    "/omniphony/state/render_evaluation/polar/elevation_resolution",
                                ),
                            )
                        }
                        _ => (None, None),
                    };
                    if let Some(state_addr) = legacy_state_addr {
                        effects.mark_dirty = true;
                        effects.trigger_layout_recompute = true;
                        effects.broadcasts.push(BroadcastUpdate {
                            addr: state_addr.to_string(),
                            value: BroadcastValue::Int(res),
                        });
                    }
                    if let Some(state_addr) = evaluation_state_addr {
                        effects.broadcasts.push(BroadcastUpdate {
                            addr: state_addr.to_string(),
                            value: BroadcastValue::Int(res),
                        });
                    }
                }
            }
            "distance_res" => {
                let res = match msg.args.first() {
                    Some(OscType::Int(i)) => Some((*i).max(1)),
                    Some(OscType::Float(f)) => Some((*f as i32).max(1)),
                    _ => None,
                };
                if let Some(res) = res {
                    ctx.renderer.live.write().unwrap().evaluation.polar.distance_res = res;
                    effects.mark_dirty = true;
                    effects.trigger_layout_recompute = true;
                    effects.broadcasts.push(BroadcastUpdate {
                        addr: "/omniphony/state/vbap/polar/distance_res".to_string(),
                        value: BroadcastValue::Int(res),
                    });
                    effects.broadcasts.push(BroadcastUpdate {
                        addr: "/omniphony/state/render_evaluation/polar/distance_res".to_string(),
                        value: BroadcastValue::Int(res),
                    });
                }
            }
            "distance_max" => {
                let max_v = match msg.args.first() {
                    Some(OscType::Int(i)) => Some((*i as f32).max(0.01)),
                    Some(OscType::Float(f)) => Some((*f).max(0.01)),
                    _ => None,
                };
                if let Some(max_v) = max_v {
                    ctx.renderer.live.write().unwrap().evaluation.polar.distance_max = max_v;
                    effects.mark_dirty = true;
                    effects.trigger_layout_recompute = true;
                    effects.broadcasts.push(BroadcastUpdate {
                        addr: "/omniphony/state/vbap/polar/distance_max".to_string(),
                        value: BroadcastValue::Float(max_v),
                    });
                    effects.broadcasts.push(BroadcastUpdate {
                        addr: "/omniphony/state/render_evaluation/polar/distance_max".to_string(),
                        value: BroadcastValue::Float(max_v),
                    });
                }
            }
            _ => {}
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/loudness" {
        if let Some(v) = parse_bool_arg(msg.args.first()) {
            ctx.renderer.live.write().unwrap().use_loudness = v;
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/loudness".to_string(),
                value: BroadcastValue::Int(if v { 1 } else { 0 }),
            });
            let live = ctx.renderer.live.read().unwrap();
            let gain_linear: f32 = match (live.use_loudness, live.dialogue_level) {
                (true, Some(dl)) => 10.0_f32.powf((-31 - dl as i32) as f32 / 20.0),
                _ => 1.0,
            };
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/loudness/gain".to_string(),
                value: BroadcastValue::Float(gain_linear),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/distance_model" {
        if let Some(OscType::String(model)) = msg.args.first() {
            if let Ok(model) = model.parse::<renderer::spatial_vbap::DistanceModel>() {
                ctx.renderer.live.write().unwrap().distance_model = model;
                effects.mark_dirty = true;
                effects.trigger_layout_recompute = true;
                effects.broadcasts.push(BroadcastUpdate {
                    addr: "/omniphony/state/distance_model".to_string(),
                    value: BroadcastValue::String(model.to_string()),
                });
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/room_ratio" {
        if msg.args.len() >= 3 {
            let w = parse_f32_arg(msg.args.first());
            let l = parse_f32_arg(msg.args.get(1));
            let h = parse_f32_arg(msg.args.get(2));
            if let (Some(w), Some(l), Some(h)) = (w, l, h) {
                ctx.renderer.live.write().unwrap().room_ratio = [w, l, h];
                effects.mark_dirty = true;
                effects.trigger_layout_recompute = true;
                effects.broadcasts.push(BroadcastUpdate {
                    addr: "/omniphony/state/room_ratio".to_string(),
                    value: BroadcastValue::Fff(w, l, h),
                });
                effects.log_message = Some(format!("OSC: room_ratio → [{}, {}, {}]", w, l, h));
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/room_ratio_rear" {
        if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.max(0.01)) {
            ctx.renderer.live.write().unwrap().room_ratio_rear = v;
            effects.mark_dirty = true;
            effects.trigger_layout_recompute = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/room_ratio_rear".to_string(),
                value: BroadcastValue::Float(v),
            });
            effects.log_message = Some(format!("OSC: room_ratio_rear → {}", v));
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/room_ratio_lower" {
        if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.max(0.01)) {
            ctx.renderer.live.write().unwrap().room_ratio_lower = v;
            effects.mark_dirty = true;
            effects.trigger_layout_recompute = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/room_ratio_lower".to_string(),
                value: BroadcastValue::Float(v),
            });
            effects.log_message = Some(format!("OSC: room_ratio_lower → {}", v));
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/room_ratio_center_blend" {
        if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.clamp(0.0, 1.0)) {
            ctx.renderer.live.write().unwrap().room_ratio_center_blend = v;
            effects.mark_dirty = true;
            effects.trigger_layout_recompute = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/room_ratio_center_blend".to_string(),
                value: BroadcastValue::Float(v),
            });
            effects.log_message = Some(format!("OSC: room_ratio_center_blend → {}", v));
        }
        return Some(effects);
    }

    if let Some(rest) = addr.strip_prefix("/omniphony/control/distance_diffuse/") {
        match rest {
            "enabled" => {
                if let Some(v) = parse_bool_arg(msg.args.first()) {
                    ctx.renderer.live.write().unwrap().use_distance_diffuse = v;
                    effects.mark_dirty = true;
                    effects.trigger_layout_recompute = true;
                    effects.broadcasts.push(BroadcastUpdate {
                        addr: "/omniphony/state/distance_diffuse/enabled".to_string(),
                        value: BroadcastValue::Int(if v { 1 } else { 0 }),
                    });
                }
                return Some(effects);
            }
            "threshold" => {
                if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.max(1e-6)) {
                    ctx.renderer.live.write().unwrap().distance_diffuse_threshold = v;
                    effects.mark_dirty = true;
                    effects.trigger_layout_recompute = true;
                    effects.broadcasts.push(BroadcastUpdate {
                        addr: "/omniphony/state/distance_diffuse/threshold".to_string(),
                        value: BroadcastValue::Float(v),
                    });
                }
                return Some(effects);
            }
            "curve" => {
                if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.max(0.0)) {
                    ctx.renderer.live.write().unwrap().distance_diffuse_curve = v;
                    effects.mark_dirty = true;
                    effects.trigger_layout_recompute = true;
                    effects.broadcasts.push(BroadcastUpdate {
                        addr: "/omniphony/state/distance_diffuse/curve".to_string(),
                        value: BroadcastValue::Float(v),
                    });
                }
                return Some(effects);
            }
            _ => {}
        }
    }

    if let Some(rest) = addr.strip_prefix("/omniphony/control/object/") {
        if let Some(idx_str) = rest.strip_suffix("/gain") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                if let Some(gain) = parse_f32_arg(msg.args.first()) {
                    ctx.renderer
                        .live
                        .write()
                        .unwrap()
                        .objects
                        .entry(idx)
                        .or_default()
                        .gain = gain;
                    ctx.renderer.mark_object_params_dirty();
                    effects.mark_dirty = true;
                    effects.broadcasts.push(BroadcastUpdate {
                        addr: format!("/omniphony/state/object/{}/gain", idx),
                        value: BroadcastValue::Float(gain),
                    });
                }
            }
            return Some(effects);
        }
        if let Some(idx_str) = rest.strip_suffix("/mute") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                if let Some(muted) = parse_bool_arg(msg.args.first()) {
                    ctx.renderer
                        .live
                        .write()
                        .unwrap()
                        .objects
                        .entry(idx)
                        .or_default()
                        .muted = muted;
                    ctx.renderer.mark_object_params_dirty();
                    effects.mark_dirty = true;
                    effects.broadcasts.push(BroadcastUpdate {
                        addr: format!("/omniphony/state/object/{}/mute", idx),
                        value: BroadcastValue::Int(if muted { 1 } else { 0 }),
                    });
                    effects.log_message = Some(format!("OSC: object[{}] mute → {}", idx, muted));
                }
            }
            return Some(effects);
        }
    }

    None
}

pub fn apply_speaker_osc_control(
    msg: &OscMessage,
    ctx: &RuntimeControlContext,
    pending_speakers: &mut HashMap<usize, SpeakerPatch>,
) -> Option<ControlEffects> {
    let addr = msg.addr.as_str();
    let mut effects = ControlEffects::default();

    if addr == "/omniphony/control/speakers/add" {
        pending_speakers.clear();
        let idx = ctx.renderer.editable_layout().speakers.len();
        let default_name = format!("spk-{}", idx);
        let name = match msg.args.first() {
            Some(OscType::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
            _ => default_name,
        };
        let az = parse_f32_arg(msg.args.get(1)).unwrap_or(0.0);
        let el = parse_f32_arg(msg.args.get(2)).unwrap_or(0.0);
        let distance = parse_f32_arg(msg.args.get(3)).unwrap_or(1.0).max(0.01);
        let spatialize = parse_bool_arg(msg.args.get(4)).unwrap_or(true);
        let delay_ms = parse_f32_arg(msg.args.get(5)).unwrap_or(0.0).max(0.0);
        let layout = ctx.renderer.with_editable_layout(|layout| {
            layout
                .speakers
                .push(renderer::speaker_layout::Speaker::from_polar(
                    name,
                    az.clamp(-180.0, 180.0),
                    el.clamp(-90.0, 90.0),
                    distance,
                    spatialize,
                    delay_ms,
                ));
            layout.clone()
        });
        if delay_ms > 0.0 {
            ctx.renderer
                .live
                .write()
                .unwrap()
                .speakers
                .entry(idx)
                .or_default()
                .delay_ms = delay_ms;
            ctx.renderer.mark_speaker_params_dirty();
        }
        effects.mark_dirty = true;
        effects.trigger_layout_recompute = true;
        effects.speaker_layout_broadcast = Some(layout);
        return Some(effects);
    }

    if addr == "/omniphony/control/speakers/remove" {
        pending_speakers.clear();
        let remove_idx = match msg.args.first() {
            Some(OscType::Int(v)) if *v >= 0 => *v as usize,
            Some(OscType::Float(v)) if *v >= 0.0 => *v as usize,
            _ => return Some(effects),
        };
        let Some(layout) = ctx.renderer.with_editable_layout(|layout| {
            if remove_idx >= layout.speakers.len() {
                return None;
            }
            layout.speakers.remove(remove_idx);
            Some(layout.clone())
        }) else {
            return Some(effects);
        };
        {
            let mut live = ctx.renderer.live.write().unwrap();
            remap_live_speakers_remove(&mut live.speakers, remove_idx);
        }
        ctx.renderer.mark_speaker_params_dirty();
        effects.mark_dirty = true;
        effects.trigger_layout_recompute = true;
        effects.speaker_layout_broadcast = Some(layout);
        return Some(effects);
    }

    if addr == "/omniphony/control/speakers/move" {
        pending_speakers.clear();
        let from_idx = match msg.args.first() {
            Some(OscType::Int(v)) if *v >= 0 => *v as usize,
            Some(OscType::Float(v)) if *v >= 0.0 => *v as usize,
            _ => return Some(effects),
        };
        let to_idx = match msg.args.get(1) {
            Some(OscType::Int(v)) if *v >= 0 => *v as usize,
            Some(OscType::Float(v)) if *v >= 0.0 => *v as usize,
            _ => return Some(effects),
        };
        let Some(layout) = ctx.renderer.with_editable_layout(|layout| {
            let len = layout.speakers.len();
            if from_idx >= len || to_idx >= len || from_idx == to_idx {
                return None;
            }
            let speaker = layout.speakers.remove(from_idx);
            layout.speakers.insert(to_idx, speaker);
            Some(layout.clone())
        }) else {
            return Some(effects);
        };
        {
            let mut live = ctx.renderer.live.write().unwrap();
            remap_live_speakers_move(&mut live.speakers, from_idx, to_idx);
        }
        ctx.renderer.mark_speaker_params_dirty();
        effects.mark_dirty = true;
        effects.trigger_layout_recompute = true;
        effects.speaker_layout_broadcast = Some(layout);
        return Some(effects);
    }

    if let Some(rest) = addr.strip_prefix("/omniphony/control/speaker/") {
        let parts: Vec<&str> = rest.splitn(2, '/').collect();
        if parts.len() != 2 {
            return Some(effects);
        }
        let Ok(idx) = parts[0].parse::<usize>() else {
            return Some(effects);
        };
        let field = parts[1];
        if field == "mute" {
            if let Some(muted) = parse_bool_arg(msg.args.first()) {
                ctx.renderer
                    .live
                    .write()
                    .unwrap()
                    .speakers
                    .entry(idx)
                    .or_default()
                    .muted = muted;
                ctx.renderer.mark_speaker_params_dirty();
                effects.mark_dirty = true;
                effects.broadcasts.push(BroadcastUpdate {
                    addr: format!("/omniphony/state/speaker/{}/mute", idx),
                    value: BroadcastValue::Int(if muted { 1 } else { 0 }),
                });
                effects.log_message = Some(format!("OSC: speaker[{}] mute → {}", idx, muted));
            }
            return Some(effects);
        }
        if field == "spatialize" {
            if let Some(spatialize) = parse_bool_arg(msg.args.first()) {
                let patch = pending_speakers.entry(idx).or_default();
                patch.spatialize = Some(spatialize);
            }
            return Some(effects);
        }
        if field == "name" {
            if let Some(OscType::String(name)) = msg.args.first() {
                let trimmed = name.trim();
                if !trimmed.is_empty() {
                    let patch = pending_speakers.entry(idx).or_default();
                    patch.name = Some(trimmed.to_string());
                }
            }
            return Some(effects);
        }
        if field == "coord_mode" {
            if let Some(OscType::String(mode)) = msg.args.first() {
                let normalized = if mode.eq_ignore_ascii_case("cartesian") {
                    "cartesian"
                } else {
                    "polar"
                };
                let patch = pending_speakers.entry(idx).or_default();
                patch.coord_mode = Some(normalized.to_string());
            }
            return Some(effects);
        }
        if let Some(f) = parse_f32_arg(msg.args.first()) {
            let patch = pending_speakers.entry(idx).or_default();
            match field {
                "az" => patch.az = Some(f),
                "el" => patch.el = Some(f),
                "distance" => patch.distance = Some(f),
                "x" => patch.x = Some(f.clamp(-1.0, 1.0)),
                "y" => patch.y = Some(f.clamp(-1.0, 1.0)),
                "z" => patch.z = Some(f.clamp(-1.0, 1.0)),
                "gain" => {
                    ctx.renderer
                        .live
                        .write()
                        .unwrap()
                        .speakers
                        .entry(idx)
                        .or_default()
                        .gain = f;
                    ctx.renderer.mark_speaker_params_dirty();
                    effects.mark_dirty = true;
                    effects.broadcasts.push(BroadcastUpdate {
                        addr: format!("/omniphony/state/speaker/{}/gain", idx),
                        value: BroadcastValue::Float(f),
                    });
                }
                "delay" => {
                    let delay_ms = f.max(0.0);
                    ctx.renderer
                        .live
                        .write()
                        .unwrap()
                        .speakers
                        .entry(idx)
                        .or_default()
                        .delay_ms = delay_ms;
                    ctx.renderer.mark_speaker_params_dirty();
                    ctx.renderer.with_editable_layout(|layout| {
                        if let Some(spk) = layout.speakers.get_mut(idx) {
                            spk.delay_ms = delay_ms;
                        }
                    });
                    effects.mark_dirty = true;
                    effects.broadcasts.push(BroadcastUpdate {
                        addr: format!("/omniphony/state/speaker/{}/delay", idx),
                        value: BroadcastValue::Float(delay_ms),
                    });
                    effects.log_message =
                        Some(format!("OSC: speaker[{}] delay → {:.2} ms", idx, delay_ms));
                }
                _ => {}
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/speakers/apply" {
        let layout = apply_pending_speakers(pending_speakers, ctx);
        effects.mark_dirty = true;
        effects.trigger_layout_recompute = true;
        effects.speaker_layout_broadcast = Some(layout);
        return Some(effects);
    }

    if addr == "/omniphony/control/speakers/reset" {
        pending_speakers.clear();
        return Some(effects);
    }

    None
}
