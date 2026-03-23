use rosc::{OscMessage, OscType};

use crate::context::RuntimeControlContext;

#[derive(Debug, Clone)]
pub enum BroadcastValue {
    Int(i32),
    Float(f32),
    String(String),
}

#[derive(Debug, Clone)]
pub struct BroadcastUpdate {
    pub addr: &'static str,
    pub value: BroadcastValue,
}

#[derive(Debug, Clone, Default)]
pub struct ControlEffects {
    pub mark_dirty: bool,
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
                    addr: "/omniphony/state/audio/output_devices",
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
                addr: "/omniphony/state/audio/output_device",
                value: BroadcastValue::String(requested.unwrap_or_default()),
            });
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
            addr: "/omniphony/state/ramp_mode",
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
                addr: "/omniphony/state/audio/sample_rate",
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
                addr: "/omniphony/state/adaptive_resampling",
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
                addr: "/omniphony/state/adaptive_resampling/enable_far_mode",
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
                addr: "/omniphony/state/adaptive_resampling/force_silence_in_far_mode",
                value: BroadcastValue::Int(if enabled { 1 } else { 0 }),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/adaptive_resampling/hard_recover_in_far_mode" {
        let enabled = parse_bool_arg(msg.args.first());
        if let (Some(audio), Some(enabled)) = (ctx.audio.as_ref(), enabled) {
            audio.set_requested_adaptive_resampling_hard_recover_in_far_mode(enabled);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/adaptive_resampling/hard_recover_in_far_mode",
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
                addr: "/omniphony/state/adaptive_resampling/far_mode_return_fade_in_ms",
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
                addr: "/omniphony/state/adaptive_resampling/kp_near",
                value: BroadcastValue::Float(value),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/adaptive_resampling/kp_far" {
        let value = parse_positive_f32_arg(msg.args.first());
        if let (Some(audio), Some(value)) = (ctx.audio.as_ref(), value) {
            audio.set_requested_adaptive_resampling_kp_far(value);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/adaptive_resampling/kp_far",
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
                addr: "/omniphony/state/adaptive_resampling/ki",
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
                addr: "/omniphony/state/adaptive_resampling/max_adjust",
                value: BroadcastValue::Float(value),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/adaptive_resampling/max_adjust_far" {
        let value = parse_positive_f32_arg(msg.args.first());
        if let (Some(audio), Some(value)) = (ctx.audio.as_ref(), value) {
            audio.set_requested_adaptive_resampling_max_adjust_far(value);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/adaptive_resampling/max_adjust_far",
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
                addr: "/omniphony/state/adaptive_resampling/update_interval_callbacks",
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
                addr: "/omniphony/state/adaptive_resampling/near_far_threshold_ms",
                value: BroadcastValue::Float(value as f32),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/adaptive_resampling/measurement_smoothing_alpha" {
        let value = match msg.args.first() {
            Some(OscType::Float(f)) if *f >= 0.0 && *f <= 1.0 => Some(*f),
            Some(OscType::Int(i)) if *i >= 0 && *i <= 1 => Some(*i as f32),
            _ => None,
        };
        if let (Some(audio), Some(value)) = (ctx.audio.as_ref(), value) {
            audio.set_requested_adaptive_resampling_measurement_smoothing_alpha(value);
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/adaptive_resampling/measurement_smoothing_alpha",
                value: BroadcastValue::Float(value),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/latency_target" {
        let latency_ms = parse_positive_u32_arg(msg.args.first());
        if let (Some(audio), Some(latency_ms)) = (ctx.audio.as_ref(), latency_ms) {
            audio.set_requested_latency_target_ms(Some(latency_ms));
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/latency_target",
                value: BroadcastValue::Float(latency_ms as f32),
            });
        }
        return Some(effects);
    }

    None
}
