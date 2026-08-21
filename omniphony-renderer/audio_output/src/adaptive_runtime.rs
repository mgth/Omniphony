use crossbeam::queue::ArrayQueue;
use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use crate::{ADAPTIVE_BAND_FAR, ADAPTIVE_BAND_NEAR, ADAPTIVE_BAND_NONE};
use crate::{
    AdaptiveControlStep, AdaptiveControllerState, AdaptiveResamplingConfig, compute_adaptive_step,
};

pub const MAX_INTEGRAL_TERM: f64 = 0.0002;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LowRecoverPhase {
    Inactive,
    Refill,
    Settling,
}

pub fn adaptive_runtime_state_name(
    low_recover_phase: LowRecoverPhase,
    hard_recover_high: bool,
) -> &'static str {
    if hard_recover_high {
        "high-recover"
    } else {
        match low_recover_phase {
            LowRecoverPhase::Inactive => "stable",
            LowRecoverPhase::Refill => "low-recover",
            LowRecoverPhase::Settling => "settling",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AdaptiveRuntimeState {
    pub controller_state: AdaptiveControllerState,
    pub callback_count: u64,
    pub underrun_warned: bool,
    pub refill_streak: u32,
    pub startup_low_recover_active: bool,
    pub low_recover_phase: LowRecoverPhase,
    pub low_recover_settle_stable_ms: f32,
    pub low_recover_refill_last_control_available: Option<usize>,
    pub low_recover_refill_delta_ema: f32,
    pub far_mode_was_muted: bool,
    pub far_mode_fade_remaining_frames: usize,
    pub far_mode_fade_total_frames: usize,
    pub last_logged_ratio_bits: u64,
    pub recovery_reacquire_pending: bool,
    pub hard_recover_high_active: bool,
    /// IIR low-pass state for the control_available smoothing seen by the
    /// PI servo. Replaces the old EMA. The previous output is kept as
    /// `smoothed_control_available` (also published in the latency
    /// telemetry); the additional internal state of the biquad lives in
    /// `iir_state`.
    pub smoothed_control_available: Option<f64>,
    pub iir_state: crate::iir::IirLowPassState,
    /// Bootstrap calibration for pre-bridge clock mode: the integer offset
    /// `(input_clock_samples_eq - cumulative_drained_samples)` **averaged**
    /// over `PRE_BRIDGE_CALIBRATION_CALLBACKS` callbacks once the input
    /// PwStream is producing subframes. Subsequent samples subtract this so
    /// the substituted `smoothed_control_available` starts at the calibrated
    /// reference and only deviates as the two clocks genuinely drift. The
    /// average is taken because the decoder's internal latency varies within
    /// each batching cycle (notably for the lossless multichannel codec with
    /// its ~320 ms cycle); a single-sample offset captured at a random phase
    /// of the cycle places the ring level at `target ± decoder_internal_jitter`,
    /// which can be up to ±160 ms. Averaging over ~1.5 s (≥ one full cycle)
    /// removes that bias. Reset on reacquisition.
    pub pre_bridge_offset_samples: i64,
    pub pre_bridge_offset_initialized: bool,
    pub pre_bridge_offset_accum: i128,
    pub pre_bridge_offset_count: u32,
}

/// Calibration window length in callbacks. At ~21 ms per callback this
/// covers ~1.5 s, more than one decoder batching cycle (~320 ms) so the
/// decoder's internal-latency oscillation averages out cleanly.
pub const PRE_BRIDGE_CALIBRATION_CALLBACKS: u32 = 72;

impl AdaptiveRuntimeState {
    pub fn new(initial_ratio: f64) -> Self {
        Self {
            controller_state: AdaptiveControllerState::default(),
            callback_count: 0,
            underrun_warned: false,
            refill_streak: 0,
            startup_low_recover_active: false,
            low_recover_phase: LowRecoverPhase::Inactive,
            low_recover_settle_stable_ms: 0.0,
            low_recover_refill_last_control_available: None,
            low_recover_refill_delta_ema: 0.0,
            far_mode_was_muted: false,
            far_mode_fade_remaining_frames: 0,
            far_mode_fade_total_frames: 0,
            last_logged_ratio_bits: initial_ratio.to_bits(),
            recovery_reacquire_pending: false,
            hard_recover_high_active: false,
            smoothed_control_available: None,
            iir_state: crate::iir::IirLowPassState::default(),
            pre_bridge_offset_samples: 0,
            pre_bridge_offset_initialized: false,
            pre_bridge_offset_accum: 0,
            pre_bridge_offset_count: 0,
        }
    }

    pub fn advance_callback(&mut self) -> u64 {
        self.callback_count = self.callback_count.saturating_add(1);
        self.callback_count
    }

    pub fn activate_startup_low_recover(&mut self) {
        self.startup_low_recover_active = true;
        self.low_recover_phase = LowRecoverPhase::Refill;
        self.low_recover_settle_stable_ms = 0.0;
        self.low_recover_refill_last_control_available = None;
        self.low_recover_refill_delta_ema = 0.0;
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LatencyMetrics {
    pub total_available_input_domain: usize,
    pub control_available: usize,
    /// IIR low-pass of `control_available`. Drives the PI servo and the
    /// Settling **dwell** of the low-recover state machine (stability bounds +
    /// settle timer + trim). The state machine's entry/exit actions use the
    /// raw `control_available` instead; `control_available` also stays raw for
    /// telemetry. See `update_far_mode_state` for the raw-vs-smoothed split.
    pub smoothed_control_available: usize,
    pub control_latency_ms: f32,
    pub measured_latency_ms: f32,
}

pub struct LatencyMetricTargets<'a> {
    pub measured_latency_ms_bits: &'a Arc<AtomicU32>,
    pub control_latency_ms_bits: &'a Arc<AtomicU32>,
}

pub fn store_latency_metrics_from_control_available(
    control_available: usize,
    channel_count: usize,
    sample_rate: u32,
    graph_latency_ms: f32,
    targets: LatencyMetricTargets<'_>,
) {
    let control_latency_ms =
        (control_available as f32 / channel_count as f32 / sample_rate as f32) * 1000.0;
    let measured_latency_ms = control_latency_ms + graph_latency_ms;
    targets
        .measured_latency_ms_bits
        .store(measured_latency_ms.to_bits(), Ordering::Relaxed);
    targets
        .control_latency_ms_bits
        .store(control_latency_ms.to_bits(), Ordering::Relaxed);
}

#[derive(Debug, Clone, Copy)]
pub struct ResetOutcome {
    pub effective_resample_ratio: f64,
    pub displayed_rate_adjust: f32,
    pub adaptive_band: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct AdaptiveServoDecision {
    pub step: AdaptiveControlStep,
    pub displayed_rate_adjust: f32,
    pub effective_resample_ratio: f64,
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
    pending_resampler_input_samples: usize,
    pacer_buffer_samples: usize,
    callback_input_domain_samples: usize,
    channel_count: usize,
    sample_rate: u32,
    graph_latency_ms: f32,
    control_smoothing_cutoff_hz: f64,
    control_smoothing_order: u32,
    callback_dt_s: f64,
    targets: LatencyMetricTargets<'_>,
) -> LatencyMetrics {
    // PI-visible total: ring + FIFO + pending. Deliberately excludes
    // `pacer_buffer_samples`. The pacer's level oscillates with the
    // decoder's batching (renderer pushes in AU bursts, drain is paced
    // to IEC chunk rate), and the whole point of the pacer was to make
    // the ring smooth so the PI doesn't see that oscillation. Folding
    // the pacer level back into the PI's input would re-inject the
    // batching ripple and defeat the smoothing benefit.
    let total_available_input_domain = available_input_samples
        .saturating_add(output_fifo_input_domain_samples)
        .saturating_add(pending_resampler_input_samples);
    let control_available =
        total_available_input_domain.saturating_sub(callback_input_domain_samples / 2);
    // Low-pass the control-path level fed to the servo / far-mode state
    // machine through a parametric IIR (1st or 2nd order, configurable
    // cutoff in Hz). Telemetry below keeps the raw `control_available`.
    let smoothed = state.iir_state.step(
        control_available as f64,
        control_smoothing_cutoff_hz,
        callback_dt_s,
        control_smoothing_order,
    );
    state.smoothed_control_available = Some(smoothed);
    let smoothed_control_available = smoothed.round().max(0.0) as usize;
    // User-visible latency reporting: include the pacer FIFO level so the
    // reported value matches the true end-to-end latency from renderer
    // output to DAC. The PI input (above) excludes the pacer for the
    // reasons noted; here we want the user's measurement to reflect what
    // actually plays.
    let display_control_available = control_available.saturating_add(pacer_buffer_samples);
    store_latency_metrics_from_control_available(
        display_control_available,
        channel_count,
        sample_rate,
        graph_latency_ms,
        targets,
    );
    let control_latency_ms =
        (display_control_available as f32 / channel_count as f32 / sample_rate as f32) * 1000.0;
    let measured_latency_ms = control_latency_ms + graph_latency_ms;

    LatencyMetrics {
        total_available_input_domain,
        control_available,
        smoothed_control_available,
        control_latency_ms,
        measured_latency_ms,
    }
}

pub fn reset_adaptive_runtime(state: &mut AdaptiveRuntimeState, base_ratio: f64) -> ResetOutcome {
    state.controller_state.accumulated_drift = 0.0;
    state.startup_low_recover_active = false;
    state.low_recover_phase = LowRecoverPhase::Inactive;
    state.low_recover_settle_stable_ms = 0.0;
    reset_low_recover_refill_tracking(state);
    state.far_mode_was_muted = false;
    state.far_mode_fade_remaining_frames = 0;
    state.far_mode_fade_total_frames = 0;
    state.last_logged_ratio_bits = base_ratio.to_bits();
    state.recovery_reacquire_pending = false;
    state.hard_recover_high_active = false;
    state.smoothed_control_available = None;
    state.iir_state.reset();
    state.pre_bridge_offset_samples = 0;
    state.pre_bridge_offset_initialized = false;
    state.pre_bridge_offset_accum = 0;
    state.pre_bridge_offset_count = 0;
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

pub fn should_run_adaptive_servo(
    callback_count: u64,
    update_interval_callbacks: u32,
    total_available_input_domain: usize,
    minimum_available_samples: usize,
) -> bool {
    let update_interval = update_interval_callbacks.max(1) as u64;
    callback_count % update_interval == 0
        && total_available_input_domain >= minimum_available_samples
}

pub fn run_adaptive_servo(
    state: &mut AdaptiveRuntimeState,
    config: &AdaptiveResamplingConfig,
    metrics: LatencyMetrics,
    target_buffer_fill: usize,
    base_ratio: f64,
    deadband_samples: usize,
    max_integral_term: f64,
    samples_per_ms: usize,
    samples_per_ms_f64: f64,
) -> AdaptiveServoDecision {
    let high_recover_entry_margin_samples =
        (config.high_recover_entry_margin_ms as usize).saturating_mul(samples_per_ms);
    let step = compute_adaptive_step(
        &mut state.controller_state,
        config,
        metrics.smoothed_control_available,
        target_buffer_fill,
        high_recover_entry_margin_samples,
        base_ratio,
        deadband_samples,
        max_integral_term,
        samples_per_ms_f64,
    );

    AdaptiveServoDecision {
        displayed_rate_adjust: step.consume_adjust as f32,
        effective_resample_ratio: step.current_ratio,
        adaptive_band: step.band,
        step,
    }
}

pub fn align_samples_to_audio_frame(samples: usize, channel_count: usize) -> usize {
    if channel_count == 0 {
        samples
    } else {
        samples - (samples % channel_count)
    }
}

pub fn far_mode_band_from_latency(
    adaptive_config: &AdaptiveResamplingConfig,
    control_available: usize,
    target_buffer_fill: usize,
    samples_per_ms: usize,
) -> u8 {
    if !adaptive_config.enable_far_mode {
        return ADAPTIVE_BAND_NONE;
    }

    let high_recover_entry_margin_samples =
        (adaptive_config.high_recover_entry_margin_ms as usize).saturating_mul(samples_per_ms);
    let is_far = high_recover_entry_margin_samples > 0
        && control_available.abs_diff(target_buffer_fill) >= high_recover_entry_margin_samples;
    if is_far {
        ADAPTIVE_BAND_FAR
    } else {
        ADAPTIVE_BAND_NEAR
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
    pub hard_recover_high: bool,
    pub hold_low_recover: bool,
    pub consume_while_muted: bool,
    pub low_recover_trim_input_samples: usize,
    pub low_recover_trim_output_samples: usize,
    pub recovery_reacquire_pending: bool,
}

pub fn reset_controller_integrator(state: &mut AdaptiveRuntimeState) {
    state.controller_state.accumulated_drift = 0.0;
}

fn reset_low_recover_refill_tracking(state: &mut AdaptiveRuntimeState) {
    state.low_recover_refill_last_control_available = None;
    state.low_recover_refill_delta_ema = 0.0;
}

fn compute_low_recover_settle_trim_plan(
    control_available: usize,
    target_buffer_fill: usize,
    settle_tolerance_input_samples: usize,
    effective_resample_ratio: f64,
    channel_count: usize,
) -> HardRecoverPlan {
    let tolerance_input_samples = align_samples_to_audio_frame(
        settle_tolerance_input_samples.max(channel_count),
        channel_count,
    );
    let overshoot_input_samples = control_available.saturating_sub(target_buffer_fill);
    let desired_consume_input_samples = if overshoot_input_samples > tolerance_input_samples {
        overshoot_input_samples.saturating_sub(tolerance_input_samples)
    } else {
        0
    };
    let desired_consume_input_samples =
        align_samples_to_audio_frame(desired_consume_input_samples, channel_count);
    let desired_consume_output_samples = align_samples_to_audio_frame(
        ((desired_consume_input_samples as f64) * effective_resample_ratio).round() as usize,
        channel_count,
    );

    HardRecoverPlan {
        desired_consume_input_samples,
        desired_consume_output_samples,
    }
}

/// Drives the low-recover / settling state machine.
///
/// Two views of the buffer level are passed in deliberately:
///
/// - `control_available` (raw) drives the **entry/exit actions** —
///   Inactive→Refill entry, the predictive Refill→Settling exit, and the
///   high-recover decisions. These need to react on the real instantaneous
///   level; low-passing them here re-adds phase lag that previously drove a
///   slow oscillation.
/// - `smoothed_control_available` (IIR low-pass, same value the PI servo
///   uses) drives the **Settling dwell**: the lower/upper stability bounds,
///   the settle timer, and the settle trim. The dwell only needs to confirm
///   the *mean* level is centered; judging it on raw lets the input-burst /
///   decoder-batching sawtooth poke out of the ±settle_margin band and
///   perpetually re-arm the timer, so the warmup never reaches `stable`.
///   The IIR is reset on Refill→Settling so the dwell starts from the real
///   level instead of a value still lagging behind the refill ramp.
pub fn update_far_mode_state(
    state: &mut AdaptiveRuntimeState,
    adaptive_config: &AdaptiveResamplingConfig,
    is_far_band: bool,
    control_available: usize,
    smoothed_control_available: usize,
    target_buffer_fill: usize,
    callback_input_domain_samples: usize,
    effective_resample_ratio: f64,
    channel_count: usize,
    input_sample_rate: u32,
    output_sample_rate: u32,
) -> FarModeDecision {
    let far_mode_enabled = adaptive_config.enable_far_mode;
    let mut low_recover_trim_plan = HardRecoverPlan {
        desired_consume_input_samples: 0,
        desired_consume_output_samples: 0,
    };
    let previous_low_recover_phase = state.low_recover_phase;
    let previous_hard_recover_high = state.hard_recover_high_active;
    let settle_callback_ms = if channel_count > 0 && input_sample_rate > 0 {
        (callback_input_domain_samples as f32 / channel_count as f32 / input_sample_rate as f32)
            * 1000.0
    } else {
        0.0
    };
    let samples_per_ms = if channel_count > 0 && input_sample_rate > 0 {
        (input_sample_rate as usize).saturating_mul(channel_count) / 1000
    } else {
        0
    };
    let margin_samples = |margin_ms: f32| -> usize {
        let raw = (margin_ms * samples_per_ms as f32).round() as usize;
        align_samples_to_audio_frame(raw.max(channel_count), channel_count)
    };
    let entry_tolerance_input_samples = align_samples_to_audio_frame(
        margin_samples(adaptive_config.low_recover_entry_margin_ms).max(channel_count),
        channel_count,
    );
    let exit_tolerance_input_samples = margin_samples(adaptive_config.low_recover_exit_margin_ms);
    let settle_tolerance_input_samples = align_samples_to_audio_frame(
        margin_samples(adaptive_config.low_recover_settle_margin_ms).max(channel_count),
        channel_count,
    );
    let low_recover_entry_threshold =
        target_buffer_fill.saturating_sub(entry_tolerance_input_samples);
    let low_recover_exit_threshold =
        target_buffer_fill.saturating_sub(exit_tolerance_input_samples);

    // Low recover is gated by its own switch (or by startup). Once enabled, the
    // entry into Refill is decided purely by the dedicated low-side threshold
    // `low_recover_entry_margin_ms`. It is intentionally NOT gated on
    // `is_far_band`: the far band uses the symmetric `high_recover_entry_margin`
    // (formerly "near/far threshold"), which for any realistic target latency
    // can only be reached on the HIGH side — so ANDing it here made runtime low
    // recover effectively unreachable. `is_far_band` still drives the high-side
    // actions (hard-recover-high, far-mode mute) below.
    let low_recover_enabled =
        adaptive_config.hard_recover_low_in_far_mode || state.startup_low_recover_active;

    if !far_mode_enabled || !low_recover_enabled {
        state.low_recover_phase = LowRecoverPhase::Inactive;
        state.low_recover_settle_stable_ms = 0.0;
        reset_low_recover_refill_tracking(state);
    } else {
        match state.low_recover_phase {
            LowRecoverPhase::Inactive => {
                if control_available < low_recover_entry_threshold {
                    state.low_recover_phase = LowRecoverPhase::Refill;
                    state.low_recover_settle_stable_ms = 0.0;
                    reset_low_recover_refill_tracking(state);
                }
            }
            LowRecoverPhase::Refill => {
                let refill_delta = state
                    .low_recover_refill_last_control_available
                    .map(|previous| control_available as isize - previous as isize)
                    .unwrap_or(0) as f32;
                if state.low_recover_refill_last_control_available.is_some() {
                    let alpha = adaptive_config.low_recover_refill_delta_alpha;
                    state.low_recover_refill_delta_ema = (state.low_recover_refill_delta_ema
                        * (1.0 - alpha))
                        + (refill_delta * alpha);
                }
                state.low_recover_refill_last_control_available = Some(control_available);
                let predicted_next_control =
                    (control_available as f32) + state.low_recover_refill_delta_ema.max(0.0);
                if control_available >= low_recover_exit_threshold
                    || predicted_next_control >= low_recover_exit_threshold as f32
                {
                    state.low_recover_phase = LowRecoverPhase::Settling;
                    state.low_recover_settle_stable_ms = 0.0;
                    reset_low_recover_refill_tracking(state);
                    // Re-seed the smoothed level to the current raw level so the
                    // Settling dwell (which reads `smoothed_control_available`)
                    // does not start from a value still lagging behind the refill
                    // ramp, which would spuriously bounce straight back to Refill.
                    state.iir_state.reset();
                    let lower_bound =
                        target_buffer_fill.saturating_sub(settle_tolerance_input_samples);
                    let upper_bound =
                        target_buffer_fill.saturating_add(settle_tolerance_input_samples);
                    if control_available < lower_bound {
                        state.low_recover_phase = LowRecoverPhase::Refill;
                    } else if control_available > upper_bound {
                        low_recover_trim_plan = compute_low_recover_settle_trim_plan(
                            control_available,
                            target_buffer_fill,
                            settle_tolerance_input_samples,
                            effective_resample_ratio,
                            channel_count,
                        );
                    }
                }
            }
            LowRecoverPhase::Settling => {
                // Dwell stability is judged on the smoothed level so the
                // input-burst / decoder-batching sawtooth does not keep
                // re-arming the settle timer. See the function doc comment.
                let lower_bound = target_buffer_fill.saturating_sub(settle_tolerance_input_samples);
                let upper_bound = target_buffer_fill.saturating_add(settle_tolerance_input_samples);
                if smoothed_control_available < lower_bound {
                    state.low_recover_phase = LowRecoverPhase::Refill;
                    state.low_recover_settle_stable_ms = 0.0;
                    reset_low_recover_refill_tracking(state);
                } else if smoothed_control_available > upper_bound {
                    state.low_recover_settle_stable_ms = 0.0;
                    low_recover_trim_plan = compute_low_recover_settle_trim_plan(
                        smoothed_control_available,
                        target_buffer_fill,
                        settle_tolerance_input_samples,
                        effective_resample_ratio,
                        channel_count,
                    );
                } else {
                    state.low_recover_settle_stable_ms += settle_callback_ms;
                    if state.low_recover_settle_stable_ms
                        >= adaptive_config.low_recover_settle_stable_ms
                    {
                        state.startup_low_recover_active = false;
                        state.low_recover_phase = LowRecoverPhase::Inactive;
                        state.low_recover_settle_stable_ms = 0.0;
                        reset_low_recover_refill_tracking(state);
                        state.recovery_reacquire_pending =
                            adaptive_config.force_silence_in_far_mode;
                    }
                }
            }
        }
    }

    let mute_low_recover_output = match state.low_recover_phase {
        LowRecoverPhase::Inactive => false,
        LowRecoverPhase::Refill => true,
        LowRecoverPhase::Settling => adaptive_config.force_silence_in_far_mode,
    };
    let previous_low_recover_output_was_muted = match previous_low_recover_phase {
        LowRecoverPhase::Inactive => false,
        LowRecoverPhase::Refill => true,
        LowRecoverPhase::Settling => adaptive_config.force_silence_in_far_mode,
    };
    let mute_far_output = far_mode_enabled
        && ((adaptive_config.force_silence_in_far_mode && is_far_band) || mute_low_recover_output);
    let hard_recover_high = far_mode_enabled
        && adaptive_config.hard_recover_high_in_far_mode
        && is_far_band
        && control_available > target_buffer_fill
        && state.low_recover_phase == LowRecoverPhase::Inactive;
    if hard_recover_high && !previous_hard_recover_high {
        reset_controller_integrator(state);
    }
    state.hard_recover_high_active = hard_recover_high;
    if previous_hard_recover_high && !hard_recover_high {
        state.recovery_reacquire_pending = true;
        reset_controller_integrator(state);
    }
    if previous_low_recover_phase != LowRecoverPhase::Inactive
        && state.low_recover_phase == LowRecoverPhase::Inactive
        && previous_low_recover_output_was_muted
    {
        state.recovery_reacquire_pending = true;
    }
    let recovery_reacquire_pending = state.recovery_reacquire_pending;

    if mute_far_output {
        state.far_mode_was_muted = true;
        state.far_mode_fade_remaining_frames = 0;
        state.far_mode_fade_total_frames = 0;
    } else if state.far_mode_was_muted {
        state.far_mode_was_muted = false;
        state.far_mode_fade_total_frames = ((output_sample_rate as u64
            * adaptive_config.far_mode_return_fade_in_ms as u64)
            / 1000) as usize;
        state.far_mode_fade_remaining_frames = state.far_mode_fade_total_frames;
    }

    FarModeDecision {
        mute_far_output,
        hard_recover_high,
        hold_low_recover: state.low_recover_phase != LowRecoverPhase::Inactive,
        consume_while_muted: state.low_recover_phase == LowRecoverPhase::Settling
            && mute_far_output,
        low_recover_trim_input_samples: low_recover_trim_plan.desired_consume_input_samples,
        low_recover_trim_output_samples: low_recover_trim_plan.desired_consume_output_samples,
        recovery_reacquire_pending,
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

pub fn compute_hard_recover_high_plan(
    callback_input_domain_samples: usize,
    control_available: usize,
    target_buffer_fill: usize,
    effective_resample_ratio: f64,
    channel_count: usize,
) -> HardRecoverPlan {
    let _ = callback_input_domain_samples;
    let desired_consume_input_samples = control_available.saturating_sub(target_buffer_fill);
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

/// Loop-invariant context for [`far_mode_step`].
pub struct FarModeStepCtx<'a> {
    pub adaptive_config: &'a crate::AdaptiveResamplingConfig,
    pub channel_count: usize,
    pub input_sample_rate: u32,
    pub output_sample_rate: u32,
    /// Published runtime-state code, for the diag plot.
    pub runtime_state_code: &'a std::sync::atomic::AtomicU8,
    pub latency: LatencyMetricTargets<'a>,
}

/// The per-callback figures the far-mode step works from.
pub struct FarModeStepInputs {
    pub is_far_band: bool,
    pub control_available: usize,
    pub smoothed_control_available: usize,
    pub target_buffer_fill: usize,
    pub callback_input_domain_samples: usize,
    /// The ratio this path works in: the resampler's effective ratio, or `1.0`
    /// when samples are copied straight through.
    pub resample_ratio: f64,
    /// The pacer's fixed contribution, folded into the *displayed* latency only.
    pub pacer_buffer_samples: usize,
    pub graph_latency_ms: f32,
}

/// Run the far-mode state machine for one callback, publish the runtime-state
/// code, and store the latency the metrics display.
///
/// The resampler path and the direct-copy path differ only in the ratio they
/// work in, but each used to carry its own copy of this, and the copies had
/// drifted apart:
///
/// * the resampler copy recovered the low-recover trim by converting the
///   decision's *output* figure back through the ratio. That round trip —
///   `align(round(input × ratio))` on the way out, `round(output / ratio)` on
///   the way back — does not return the input figure the state machine actually
///   decided once the ratio is far enough from 1. Both now read
///   `low_recover_trim_input_samples`, which is the authoritative one: the
///   output figure is derived from it, not the reverse.
/// * the displayed-latency store had been kept identical by hand.
///
/// It only skewed the *displayed* latency, never what was consumed — but it is
/// exactly the shape of bug that gets fixed on one path and left on the other.
pub fn far_mode_step(
    state: &mut AdaptiveRuntimeState,
    ctx: &FarModeStepCtx<'_>,
    inputs: FarModeStepInputs,
) -> FarModeDecision {
    let decision = update_far_mode_state(
        state,
        ctx.adaptive_config,
        inputs.is_far_band,
        inputs.control_available,
        inputs.smoothed_control_available,
        inputs.target_buffer_fill,
        inputs.callback_input_domain_samples,
        inputs.resample_ratio,
        ctx.channel_count,
        ctx.input_sample_rate,
        ctx.output_sample_rate,
    );
    ctx.runtime_state_code.store(
        crate::adaptive_runtime_state_code(adaptive_runtime_state_name(
            state.low_recover_phase,
            decision.hard_recover_high,
        )),
        Ordering::Relaxed,
    );

    // What the buffer level will be once this callback's action has been
    // applied — the servo reasons about the outcome, not the reading.
    let mut projected = inputs.control_available;
    if decision.recovery_reacquire_pending && decision.mute_far_output {
        projected = projected.saturating_sub(inputs.callback_input_domain_samples);
    } else if decision.hard_recover_high {
        let plan = compute_hard_recover_high_plan(
            inputs.callback_input_domain_samples,
            inputs.control_available,
            inputs.target_buffer_fill,
            inputs.resample_ratio,
            ctx.channel_count,
        );
        projected = projected.saturating_sub(plan.desired_consume_input_samples);
    } else if decision.hold_low_recover {
        let muted_consume_input_samples =
            if decision.mute_far_output && decision.consume_while_muted {
                inputs.callback_input_domain_samples
            } else {
                0
            };
        projected = projected.saturating_sub(
            decision
                .low_recover_trim_input_samples
                .saturating_add(muted_consume_input_samples),
        );
    }

    // Fold in the pacer's fixed contribution so the displayed control/measured
    // latency reflects the true end-to-end delay and stays comparable to the
    // target. The servo keeps using the pacer-excluded figure for its own
    // decisions.
    store_latency_metrics_from_control_available(
        projected.saturating_add(inputs.pacer_buffer_samples),
        ctx.channel_count,
        ctx.input_sample_rate,
        inputs.graph_latency_ms,
        LatencyMetricTargets {
            measured_latency_ms_bits: ctx.latency.measured_latency_ms_bits,
            control_latency_ms_bits: ctx.latency.control_latency_ms_bits,
        },
    );

    decision
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AdaptiveControllerState, AdaptiveResamplingConfig, compute_adaptive_step};

    // ── domain conversion & alignment ──────────────────────────────────────

    #[test]
    fn output_to_input_domain_scales_by_ratio() {
        assert_eq!(output_to_input_domain_samples(100, 1.0), 100);
        // The output domain runs at `ratio`× the input rate, so 100 output
        // samples correspond to 50 input samples at ratio 2.0.
        assert_eq!(output_to_input_domain_samples(100, 2.0), 50);
        assert_eq!(output_to_input_domain_samples(100, 0.5), 200);
        // Non-positive ratio is a guard: pass the value through unchanged.
        assert_eq!(output_to_input_domain_samples(100, 0.0), 100);
    }

    #[test]
    fn align_rounds_down_to_channel_multiple() {
        assert_eq!(align_samples_to_audio_frame(7, 2), 6);
        assert_eq!(align_samples_to_audio_frame(8, 2), 8);
        assert_eq!(align_samples_to_audio_frame(10, 4), 8);
        // channel_count 0 is a guard against a divide/modulo by zero.
        assert_eq!(align_samples_to_audio_frame(7, 0), 7);
    }

    #[test]
    fn paused_rate_adjust_reports_inverse_ratio() {
        assert!((paused_rate_adjust(1.0, 1.0) - 1.0).abs() < 1e-6);
        assert!((paused_rate_adjust(1.0, 2.0) - 0.5).abs() < 1e-6);
        // effective ratio 0 is a guard.
        assert!((paused_rate_adjust(1.0, 0.0) - 1.0).abs() < 1e-6);
    }

    // ── servo gating & band classification ─────────────────────────────────

    #[test]
    fn servo_runs_only_on_interval_with_enough_samples() {
        assert!(should_run_adaptive_servo(10, 5, 1000, 500));
        assert!(!should_run_adaptive_servo(11, 5, 1000, 500)); // not on interval
        assert!(!should_run_adaptive_servo(10, 5, 400, 500)); // too few samples
        // interval 0 is clamped to 1 (run every callback).
        assert!(should_run_adaptive_servo(7, 0, 1000, 0));
    }

    fn cfg_far(margin_ms: u32) -> AdaptiveResamplingConfig {
        AdaptiveResamplingConfig {
            enable_far_mode: true,
            high_recover_entry_margin_ms: margin_ms,
            ..Default::default()
        }
    }

    #[test]
    fn far_band_classification() {
        // far mode disabled → NONE regardless of latency.
        let disabled = AdaptiveResamplingConfig {
            enable_far_mode: false,
            ..cfg_far(10)
        };
        assert_eq!(
            far_mode_band_from_latency(&disabled, 5000, 1000, 48),
            ADAPTIVE_BAND_NONE
        );
        // margin 0 disables the "far" classification (guard) → NEAR.
        assert_eq!(
            far_mode_band_from_latency(&cfg_far(0), 9999, 0, 48),
            ADAPTIVE_BAND_NEAR
        );
        // 10 ms × 48 spm = 480 samples. Within margin → NEAR.
        assert_eq!(
            far_mode_band_from_latency(&cfg_far(10), 1000, 1100, 48),
            ADAPTIVE_BAND_NEAR
        );
        // Beyond margin, above the target → FAR.
        assert_eq!(
            far_mode_band_from_latency(&cfg_far(10), 2000, 1000, 48),
            ADAPTIVE_BAND_FAR
        );
        // Beyond margin, below the target (abs_diff is symmetric) → FAR.
        assert_eq!(
            far_mode_band_from_latency(&cfg_far(10), 0, 1000, 48),
            ADAPTIVE_BAND_FAR
        );
    }

    // ── state transitions ──────────────────────────────────────────────────

    #[test]
    fn reset_clears_controller_and_returns_base() {
        let mut s = AdaptiveRuntimeState::new(1.0);
        s.controller_state.accumulated_drift = 12.5;
        s.low_recover_phase = LowRecoverPhase::Settling;
        s.hard_recover_high_active = true;
        let out = reset_adaptive_runtime(&mut s, 1.25);
        assert_eq!(s.controller_state.accumulated_drift, 0.0);
        assert_eq!(s.low_recover_phase, LowRecoverPhase::Inactive);
        assert!(!s.hard_recover_high_active);
        assert_eq!(out.effective_resample_ratio, 1.25);
        assert!((out.displayed_rate_adjust - 1.0).abs() < 1e-6);
        assert_eq!(out.adaptive_band, ADAPTIVE_BAND_NONE);
    }

    #[test]
    fn reset_controller_integrator_zeroes_drift() {
        let mut s = AdaptiveRuntimeState::new(1.0);
        s.controller_state.accumulated_drift = -7.0;
        reset_controller_integrator(&mut s);
        assert_eq!(s.controller_state.accumulated_drift, 0.0);
    }

    // ── far_mode_step: the step both output paths share ──────────────────

    fn step_ctx<'a>(
        cfg: &'a crate::AdaptiveResamplingConfig,
        code: &'a std::sync::atomic::AtomicU8,
        measured: &'a Arc<AtomicU32>,
        control: &'a Arc<AtomicU32>,
    ) -> FarModeStepCtx<'a> {
        FarModeStepCtx {
            adaptive_config: cfg,
            channel_count: 8,
            input_sample_rate: 48_000,
            output_sample_rate: 48_000,
            runtime_state_code: code,
            latency: LatencyMetricTargets {
                measured_latency_ms_bits: measured,
                control_latency_ms_bits: control,
            },
        }
    }

    fn step_inputs(control_available: usize, ratio: f64) -> FarModeStepInputs {
        FarModeStepInputs {
            is_far_band: false,
            control_available,
            smoothed_control_available: control_available,
            target_buffer_fill: 48_000,
            callback_input_domain_samples: 1024,
            resample_ratio: ratio,
            pacer_buffer_samples: 0,
            graph_latency_ms: 0.0,
        }
    }

    /// The step publishes the runtime-state code the diag plot reads, which
    /// both output paths used to store by hand.
    #[test]
    fn far_mode_step_publishes_the_runtime_state_code() {
        let cfg = crate::AdaptiveResamplingConfig::default();
        let code = std::sync::atomic::AtomicU8::new(u8::MAX);
        let (m, c) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
        let mut state = AdaptiveRuntimeState::new(1.0);

        far_mode_step(
            &mut state,
            &step_ctx(&cfg, &code, &m, &c),
            step_inputs(48_000, 1.0),
        );
        assert_eq!(
            code.load(Ordering::Relaxed),
            crate::adaptive_runtime_state_code("stable")
        );
    }

    /// The step stores a latency for the metrics to display, and folds the
    /// pacer's fixed contribution into it.
    #[test]
    fn far_mode_step_stores_the_displayed_latency_including_the_pacer() {
        let cfg = crate::AdaptiveResamplingConfig::default();
        let code = std::sync::atomic::AtomicU8::new(0);
        let (m, c) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));

        let mut without = AdaptiveRuntimeState::new(1.0);
        far_mode_step(
            &mut without,
            &step_ctx(&cfg, &code, &m, &c),
            step_inputs(48_000, 1.0),
        );
        let latency_without = f32::from_bits(c.load(Ordering::Relaxed));

        let mut with = AdaptiveRuntimeState::new(1.0);
        let mut inputs = step_inputs(48_000, 1.0);
        inputs.pacer_buffer_samples = 48_000;
        far_mode_step(&mut with, &step_ctx(&cfg, &code, &m, &c), inputs);
        let latency_with = f32::from_bits(c.load(Ordering::Relaxed));

        assert!(
            latency_with > latency_without,
            "the pacer's contribution must show in the displayed latency \
             ({latency_with} vs {latency_without})"
        );
    }

    /// The low-recover trim is taken in the input domain the state machine
    /// decided, not recovered by converting its output figure back.
    ///
    /// The resampler path used to do the round trip — `align(round(input ×
    /// ratio))` out, `round(output / ratio)` back — which does not return the
    /// input figure once the ratio is far enough from 1. It only skewed the
    /// displayed latency, but the two paths disagreed about a number the state
    /// machine had already decided.
    #[test]
    fn the_low_recover_trim_does_not_round_trip_through_the_output_domain() {
        // A ratio far enough from 1 that the round trip loses samples, and an
        // overshoot large enough for the trim to be non-zero.
        let ratio = 1.05_f64;
        let plan = compute_low_recover_settle_trim_plan(60_000, 48_000, 1024, ratio, 8);
        assert!(
            plan.desired_consume_input_samples > 0,
            "the fixture must actually produce a trim"
        );
        let round_tripped =
            output_to_input_domain_samples(plan.desired_consume_output_samples, ratio);
        assert_ne!(
            round_tripped, plan.desired_consume_input_samples,
            "fixture is only meaningful if the round trip actually loses samples"
        );
    }

    #[test]
    fn advance_callback_counts_monotonically() {
        let mut s = AdaptiveRuntimeState::new(1.0);
        assert_eq!(s.advance_callback(), 1);
        assert_eq!(s.advance_callback(), 2);
        assert_eq!(s.callback_count, 2);
    }

    #[test]
    fn activate_startup_low_recover_enters_refill() {
        let mut s = AdaptiveRuntimeState::new(1.0);
        s.activate_startup_low_recover();
        assert!(s.startup_low_recover_active);
        assert_eq!(s.low_recover_phase, LowRecoverPhase::Refill);
    }

    #[test]
    fn state_name_mapping() {
        assert_eq!(
            adaptive_runtime_state_name(LowRecoverPhase::Inactive, false),
            "stable"
        );
        assert_eq!(
            adaptive_runtime_state_name(LowRecoverPhase::Refill, false),
            "low-recover"
        );
        assert_eq!(
            adaptive_runtime_state_name(LowRecoverPhase::Settling, false),
            "settling"
        );
        // hard-recover-high overrides whatever the low-recover phase is.
        assert_eq!(
            adaptive_runtime_state_name(LowRecoverPhase::Settling, true),
            "high-recover"
        );
    }

    // ── ring / buffer helpers ──────────────────────────────────────────────

    #[test]
    fn discard_ring_caps_at_available() {
        let q = ArrayQueue::new(8);
        for i in 0..5 {
            q.push(i as f32).unwrap();
        }
        // Requesting more than present drains everything and reports the real count.
        assert_eq!(discard_ring_samples(&q, 100), 5);
        assert!(q.is_empty());
    }

    #[test]
    fn discard_ring_drops_oldest_first() {
        let q = ArrayQueue::new(8);
        for i in 0..5 {
            q.push(i as f32).unwrap();
        }
        assert_eq!(discard_ring_samples(&q, 3), 3);
        assert_eq!(q.len(), 2);
        // FIFO: the three oldest (0,1,2) are gone, 3.0 is now at the front.
        assert_eq!(q.pop(), Some(3.0));
    }

    #[test]
    fn zero_pad_tail_zeros_after_written() {
        let mut buf = vec![1.0f32, 2.0, 3.0, 4.0];
        zero_pad_tail(&mut buf, 2);
        assert_eq!(buf, vec![1.0, 2.0, 0.0, 0.0]);
    }

    #[test]
    fn fade_is_noop_when_inactive() {
        let mut s = AdaptiveRuntimeState::new(1.0);
        let mut buf = vec![1.0f32; 8];
        apply_interleaved_fade(&mut buf, 2, &mut s); // remaining/total are 0
        assert!(buf.iter().all(|&x| x == 1.0));
    }

    #[test]
    fn fade_ramps_gain_per_frame_and_drains() {
        let mut s = AdaptiveRuntimeState::new(1.0);
        s.far_mode_fade_total_frames = 4;
        s.far_mode_fade_remaining_frames = 4;
        let mut buf = vec![1.0f32; 2 * 4]; // 4 frames, width 2
        apply_interleaved_fade(&mut buf, 2, &mut s);
        // gain = (total - remaining) / total, evaluated before each decrement:
        // 0/4, 1/4, 2/4, 3/4.
        assert_eq!(&buf[0..2], &[0.0, 0.0]);
        assert_eq!(&buf[2..4], &[0.25, 0.25]);
        assert_eq!(&buf[4..6], &[0.5, 0.5]);
        assert_eq!(&buf[6..8], &[0.75, 0.75]);
        assert_eq!(s.far_mode_fade_remaining_frames, 0);
    }

    #[test]
    fn postprocess_mutes_when_requested() {
        let mut s = AdaptiveRuntimeState::new(1.0);
        let mut buf = vec![0.5f32; 4];
        postprocess_interleaved_output(&mut buf, 2, true, &mut s);
        assert!(buf.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn refill_streak_increments_and_warns_once() {
        let mut s = AdaptiveRuntimeState::new(1.0);
        note_refill_or_underrun(&mut s, "i", "d", 10, 40);
        assert_eq!(s.refill_streak, 1);
        assert!(s.underrun_warned);
        note_refill_or_underrun(&mut s, "i", "d", 10, 40);
        assert_eq!(s.refill_streak, 2);
        assert!(s.underrun_warned); // stays latched
    }

    #[test]
    fn hard_recover_plan_consumes_excess_above_target() {
        // 2000 available, target 1000, ratio 1.0, 2 channels.
        let plan = compute_hard_recover_high_plan(0, 2000, 1000, 1.0, 2);
        assert_eq!(plan.desired_consume_input_samples, 1000);
        assert_eq!(plan.desired_consume_output_samples, 1000);
        // Below target → saturating_sub yields nothing to consume.
        let none = compute_hard_recover_high_plan(0, 500, 1000, 1.0, 2);
        assert_eq!(none.desired_consume_input_samples, 0);
    }

    #[test]
    fn store_latency_metrics_publishes_atomics() {
        let measured = Arc::new(AtomicU32::new(0));
        let control = Arc::new(AtomicU32::new(0));
        // 48000 samples available, 1 channel, 48 kHz → 1000 ms; +5 ms graph.
        store_latency_metrics_from_control_available(
            48_000,
            1,
            48_000,
            5.0,
            LatencyMetricTargets {
                measured_latency_ms_bits: &measured,
                control_latency_ms_bits: &control,
            },
        );
        let control_ms = f32::from_bits(control.load(Ordering::Relaxed));
        let measured_ms = f32::from_bits(measured.load(Ordering::Relaxed));
        assert!((control_ms - 1000.0).abs() < 1e-3);
        assert!((measured_ms - 1005.0).abs() < 1e-3);
    }

    // ── PI controller core (compute_adaptive_step) ─────────────────────────

    /// Config isolating the proportional term (ki = 0) with a 10 % adjust cap.
    fn p_only() -> AdaptiveResamplingConfig {
        AdaptiveResamplingConfig {
            kp_near: 100.0,
            ki: 0.0,
            max_adjust: 0.10,
            enable_far_mode: false,
            ..Default::default()
        }
    }

    #[test]
    fn servo_drains_when_buffer_above_target() {
        // available > target → positive drift → consume_adjust > 1 → ratio < base,
        // so the resampler consumes input faster and the buffer falls back to target.
        let mut st = AdaptiveControllerState::default();
        let step = compute_adaptive_step(
            &mut st,
            &p_only(),
            6000,
            5000,
            0,
            1.0,
            0,
            MAX_INTEGRAL_TERM,
            48.0,
        );
        assert!(step.drift > 0);
        assert!(
            step.consume_adjust > 1.0,
            "adjust = {}",
            step.consume_adjust
        );
        assert!(step.current_ratio < 1.0);
    }

    #[test]
    fn servo_fills_when_buffer_below_target() {
        let mut st = AdaptiveControllerState::default();
        let step = compute_adaptive_step(
            &mut st,
            &p_only(),
            4000,
            5000,
            0,
            1.0,
            0,
            MAX_INTEGRAL_TERM,
            48.0,
        );
        assert!(step.drift < 0);
        assert!(step.consume_adjust < 1.0);
        assert!(step.current_ratio > 1.0);
    }

    #[test]
    fn servo_saturates_consume_adjust_at_max_adjust() {
        let mut st = AdaptiveControllerState::default();
        let cfg = AdaptiveResamplingConfig {
            max_adjust: 0.05,
            ..p_only()
        };
        // Enormous drift → P term blows up but the adjust is clamped to 1 + max_adjust.
        let step = compute_adaptive_step(
            &mut st,
            &cfg,
            1_000_000,
            0,
            0,
            1.0,
            0,
            MAX_INTEGRAL_TERM,
            48.0,
        );
        assert!((step.consume_adjust - 1.05).abs() < 1e-9);
    }

    #[test]
    fn servo_is_frozen_when_max_adjust_is_zero() {
        let mut st = AdaptiveControllerState::default();
        let cfg = AdaptiveResamplingConfig {
            max_adjust: 0.0,
            ..p_only()
        };
        // max_adjust 0 collapses the clamp window to [1, 1]: ratio stays at base.
        let step =
            compute_adaptive_step(&mut st, &cfg, 9999, 0, 0, 1.0, 0, MAX_INTEGRAL_TERM, 48.0);
        assert!((step.consume_adjust - 1.0).abs() < 1e-9);
        assert!((step.current_ratio - 1.0).abs() < 1e-9);
    }

    #[test]
    fn servo_deadband_blocks_integral_accumulation() {
        let mut st = AdaptiveControllerState::default();
        let cfg = AdaptiveResamplingConfig {
            ki: 1000.0,
            ..p_only()
        };
        // |drift| = 10 samples < deadband 100 → the integrator must not wind up.
        let _ = compute_adaptive_step(
            &mut st,
            &cfg,
            5010,
            5000,
            0,
            1.0,
            100,
            MAX_INTEGRAL_TERM,
            48.0,
        );
        assert_eq!(st.accumulated_drift, 0.0);
    }

    #[test]
    fn servo_integral_windup_is_clamped() {
        // Sustained large drift with only the integral active: accumulated drift
        // saturates so the i_term never exceeds MAX_INTEGRAL_TERM.
        let mut st = AdaptiveControllerState::default();
        let cfg = AdaptiveResamplingConfig {
            ki: 5000.0,
            kp_near: 0.0,
            ..p_only()
        };
        for _ in 0..1000 {
            compute_adaptive_step(
                &mut st,
                &cfg,
                8000,
                5000,
                0,
                1.0,
                0,
                MAX_INTEGRAL_TERM,
                48.0,
            );
        }
        let i_term = st.accumulated_drift * cfg.ki / 1_000_000.0;
        assert!(
            i_term.abs() <= MAX_INTEGRAL_TERM + 1e-9,
            "i_term = {i_term}"
        );
    }

    #[test]
    fn run_adaptive_servo_threads_the_decision() {
        let mut s = AdaptiveRuntimeState::new(1.0);
        let cfg = AdaptiveResamplingConfig {
            high_recover_entry_margin_ms: 0,
            ..p_only()
        };
        let metrics = LatencyMetrics {
            total_available_input_domain: 6000,
            control_available: 6000,
            smoothed_control_available: 6000,
            control_latency_ms: 0.0,
            measured_latency_ms: 0.0,
        };
        let d = run_adaptive_servo(
            &mut s,
            &cfg,
            metrics,
            5000,
            1.0,
            0,
            MAX_INTEGRAL_TERM,
            48,
            48.0,
        );
        assert_eq!(d.effective_resample_ratio, d.step.current_ratio);
        assert_eq!(d.adaptive_band, d.step.band);
        assert!((d.displayed_rate_adjust as f64 - d.step.consume_adjust).abs() < 1e-5);
    }
}
