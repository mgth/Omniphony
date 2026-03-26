use crossbeam::queue::ArrayQueue;
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use crate::{AdaptiveControllerState, AdaptiveResamplingConfig};

pub const MAX_INTEGRAL_TERM: f64 = 0.0002;

#[derive(Debug, Clone)]
pub struct AdaptiveRuntimeState {
    pub controller_state: AdaptiveControllerState,
    pub callback_count: u64,
    pub underrun_warned: bool,
    pub refill_streak: u32,
    pub far_mode_was_muted: bool,
    pub far_mode_fade_remaining_frames: usize,
    pub far_mode_fade_total_frames: usize,
    pub last_logged_ratio_bits: u64,
}

impl AdaptiveRuntimeState {
    pub fn new(initial_ratio: f64) -> Self {
        Self {
            controller_state: AdaptiveControllerState::default(),
            callback_count: 0,
            underrun_warned: false,
            refill_streak: 0,
            far_mode_was_muted: false,
            far_mode_fade_remaining_frames: 0,
            far_mode_fade_total_frames: 0,
            last_logged_ratio_bits: initial_ratio.to_bits(),
        }
    }

    pub fn advance_callback(&mut self) -> u64 {
        self.callback_count = self.callback_count.saturating_add(1);
        self.callback_count
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LatencyMetrics {
    pub total_available_input_domain: usize,
    pub control_available: usize,
    pub measured_latency_ms: f32,
}

pub struct LatencyMetricTargets<'a> {
    pub measured_latency_ms_bits: &'a Arc<AtomicU32>,
}

#[derive(Debug, Clone, Copy)]
pub struct ResetOutcome {
    pub effective_resample_ratio: f64,
    pub displayed_rate_adjust: f32,
    pub adaptive_band: u8,
}

pub fn output_to_input_domain_samples(output_samples: usize, ratio: f64) -> usize {
    if ratio > 0.0 {
        ((output_samples as f64) / ratio).round() as usize
    } else {
        output_samples
    }
}

pub fn update_latency_metrics(
    state: &mut AdaptiveRuntimeState,
    available_input_samples: usize,
    output_fifo_input_domain_samples: usize,
    callback_input_domain_samples: usize,
    channel_count: usize,
    sample_rate: u32,
    graph_latency_ms: f32,
    targets: LatencyMetricTargets<'_>,
) -> LatencyMetrics {
    let _ = state; // no EMA state needed
    let total_available_input_domain =
        available_input_samples.saturating_add(output_fifo_input_domain_samples);
    let control_available =
        total_available_input_domain.saturating_sub(callback_input_domain_samples / 2);
    let measured_latency_ms =
        (control_available as f32 / channel_count as f32 / sample_rate as f32) * 1000.0
            + graph_latency_ms;
    targets
        .measured_latency_ms_bits
        .store(measured_latency_ms.to_bits(), Ordering::Relaxed);

    LatencyMetrics {
        total_available_input_domain,
        control_available,
        measured_latency_ms,
    }
}

pub fn reset_adaptive_runtime(
    state: &mut AdaptiveRuntimeState,
    base_ratio: f64,
) -> ResetOutcome {
    state.controller_state.accumulated_drift = 0.0;
    state.last_logged_ratio_bits = base_ratio.to_bits();
    ResetOutcome {
        effective_resample_ratio: base_ratio,
        displayed_rate_adjust: 1.0,
        adaptive_band: crate::ADAPTIVE_BAND_NONE,
    }
}

pub fn paused_rate_adjust(base_ratio: f64, effective_resample_ratio: f64) -> f32 {
    if effective_resample_ratio > 0.0 {
        (base_ratio / effective_resample_ratio) as f32
    } else {
        1.0
    }
}

pub fn align_samples_to_audio_frame(samples: usize, channel_count: usize) -> usize {
    if channel_count == 0 {
        samples
    } else {
        samples - (samples % channel_count)
    }
}

pub fn discard_ring_samples(buffer: &ArrayQueue<f32>, samples_to_discard: usize) -> usize {
    let mut dropped = 0usize;
    while dropped < samples_to_discard {
        if buffer.pop().is_some() {
            dropped += 1;
        } else {
            break;
        }
    }
    dropped
}

#[derive(Debug, Clone, Copy)]
pub struct FarModeDecision {
    pub mute_far_output: bool,
    pub hard_recover_far: bool,
}

pub fn update_far_mode_state(
    state: &mut AdaptiveRuntimeState,
    adaptive_config: &AdaptiveResamplingConfig,
    adaptive_resampling_enabled: bool,
    is_far_band: bool,
    output_sample_rate: u32,
) -> FarModeDecision {
    let mute_far_output =
        adaptive_resampling_enabled && adaptive_config.force_silence_in_far_mode && is_far_band;
    let hard_recover_far =
        adaptive_resampling_enabled && adaptive_config.hard_recover_in_far_mode && is_far_band;

    if mute_far_output {
        state.far_mode_was_muted = true;
        state.far_mode_fade_remaining_frames = 0;
        state.far_mode_fade_total_frames = 0;
    } else if state.far_mode_was_muted {
        state.far_mode_was_muted = false;
        state.far_mode_fade_total_frames =
            ((output_sample_rate as u64 * adaptive_config.far_mode_return_fade_in_ms as u64)
                / 1000) as usize;
        state.far_mode_fade_remaining_frames = state.far_mode_fade_total_frames;
    }

    FarModeDecision {
        mute_far_output,
        hard_recover_far,
    }
}

pub fn apply_interleaved_fade(
    samples: &mut [f32],
    audio_frame_width: usize,
    state: &mut AdaptiveRuntimeState,
) {
    if audio_frame_width == 0
        || state.far_mode_fade_remaining_frames == 0
        || state.far_mode_fade_total_frames == 0
    {
        return;
    }

    let frames_in_buffer = samples.len() / audio_frame_width;
    for frame_idx in 0..frames_in_buffer {
        let fade_done = state
            .far_mode_fade_total_frames
            .saturating_sub(state.far_mode_fade_remaining_frames);
        let gain = fade_done as f32 / state.far_mode_fade_total_frames as f32;
        let frame_start = frame_idx * audio_frame_width;
        for sample in &mut samples[frame_start..frame_start + audio_frame_width] {
            *sample *= gain;
        }
        state.far_mode_fade_remaining_frames =
            state.far_mode_fade_remaining_frames.saturating_sub(1);
        if state.far_mode_fade_remaining_frames == 0 {
            break;
        }
    }
}

pub fn postprocess_interleaved_output(
    samples: &mut [f32],
    audio_frame_width: usize,
    mute_output: bool,
    state: &mut AdaptiveRuntimeState,
) {
    if mute_output {
        samples.fill(0.0);
    } else {
        apply_interleaved_fade(samples, audio_frame_width, state);
    }
}

pub fn zero_pad_tail(samples: &mut [f32], written: usize) {
    for sample in samples.iter_mut().skip(written) {
        *sample = 0.0;
    }
}

pub fn note_refill_or_underrun(
    state: &mut AdaptiveRuntimeState,
    info_label: &str,
    debug_label: &str,
    available: usize,
    needed: usize,
) {
    state.refill_streak = state.refill_streak.saturating_add(1);
    if !state.underrun_warned {
        if state.refill_streak >= 2 {
            log::info!(
                "{}: {} of {} samples available; zero-padding remainder (streak={})",
                info_label,
                available,
                needed,
                state.refill_streak
            );
        } else {
            log::debug!(
                "{}: {} of {} samples available; zero-padding remainder (streak={})",
                debug_label,
                available,
                needed,
                state.refill_streak
            );
        }
        state.underrun_warned = true;
    }
}

#[derive(Debug, Clone, Copy)]
pub struct HardRecoverPlan {
    pub desired_consume_input_samples: usize,
    pub desired_consume_output_samples: usize,
}

pub fn compute_hard_recover_plan(
    callback_input_domain_samples: usize,
    control_available: usize,
    target_buffer_fill: usize,
    effective_resample_ratio: f64,
    channel_count: usize,
) -> HardRecoverPlan {
    let desired_consume_input_samples =
        (callback_input_domain_samples as i64 + control_available as i64 - target_buffer_fill as i64)
            .max(0) as usize;
    let desired_consume_input_samples =
        align_samples_to_audio_frame(desired_consume_input_samples, channel_count);
    let desired_consume_output_samples =
        ((desired_consume_input_samples as f64) * effective_resample_ratio).round() as usize;
    let desired_consume_output_samples =
        align_samples_to_audio_frame(desired_consume_output_samples, channel_count);

    HardRecoverPlan {
        desired_consume_input_samples,
        desired_consume_output_samples,
    }
}
