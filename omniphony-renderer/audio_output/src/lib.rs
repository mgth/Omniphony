pub mod adaptive_runtime;
pub mod control;
pub mod file_sink;
pub mod iir;
pub mod output_telemetry;
pub mod pacer;
pub mod resampler_fifo;
pub mod ring_buffer_io;

pub use control::{
    AppliedAudioOutputState, AudioControl, OutputDeviceOption, RequestedAudioOutputConfig,
};
pub use file_sink::{CafChannelDesc, FileAudioWriter, FileSinkFormat};
pub use pacer::PacerHandle;

#[derive(Debug, Clone)]
pub struct AdaptiveResamplingConfig {
    pub enable_far_mode: bool,
    pub force_silence_in_far_mode: bool,
    pub hard_recover_high_in_far_mode: bool,
    pub hard_recover_low_in_far_mode: bool,
    pub far_mode_return_fade_in_ms: u32,
    pub kp_near: f64,
    pub ki: f64,
    /// Fraction of accumulated integral drift retained when the error changes sign.
    /// 0.0 fully resets the integrator, 1.0 keeps it unchanged.
    pub integral_discharge_ratio: f64,
    pub max_adjust: f64,
    pub update_interval_callbacks: u32,
    pub high_recover_entry_margin_ms: u32,
    /// Duration the control buffer must stay within tolerance to exit the
    /// `Settling` phase and resume the steady-state servo.
    pub low_recover_settle_stable_ms: f32,
    /// Tolerance (ms) for entering the low-recover refill phase.
    pub low_recover_entry_margin_ms: f32,
    /// Tolerance (ms) for exiting the low-recover refill phase.
    pub low_recover_exit_margin_ms: f32,
    /// Tolerance (ms) during the `Settling` phase.
    pub low_recover_settle_margin_ms: f32,
    /// EMA factor for tracking the refill delta across callbacks.
    pub low_recover_refill_delta_alpha: f32,
    /// Cutoff frequency (Hz) for the IIR low-pass that filters the
    /// control-path buffer level seen by the PI servo. Replaces the old
    /// fixed-α EMA — parametrising the filter in physical units makes the
    /// tuning intuitive (0.5 Hz cleanly suppresses the ~3 Hz decoder
    /// batching ripple while still tracking sub-Hz hardware drift).
    pub control_smoothing_cutoff_hz: f64,
    /// IIR filter order: 1 = single pole (6 dB/oct rolloff), 2 = Butterworth
    /// biquad (12 dB/oct). Higher order rejects out-of-band ripple more
    /// aggressively at the cost of slightly more phase lag near the cutoff.
    pub control_smoothing_order: u32,
    /// When true the PI controller is frozen: the current ratio is held as-is.
    pub paused: bool,
    /// When true the PI servo consumes the pre-bridge clock signal
    /// (`input_clock_us` − cumulative drained, in samples) instead of the
    /// post-decode ring + FIFO + pending-resampler level. The pre-bridge
    /// signal is smooth by construction because the IEC958 source clock is
    /// not affected by the decoder's batched delivery, so the EMA can be
    /// effectively bypassed and the PI reacts directly to genuine clock
    /// drift. The ring buffer is still observed for underrun/overrun safety
    /// and for the low-recover phase. Default `false` (legacy behaviour).
    pub use_pre_bridge_clock: bool,
    /// When true the post-rendering output pacer is active: rendered
    /// speaker PCM goes into an intermediate FIFO which the PipeWire input
    /// thread drains into the ring buffer in lockstep with IEC958 chunk
    /// arrival. The ring buffer therefore sees a smooth flow regardless
    /// of the decoder's burst pattern, and the PI sees a clean
    /// `control_available` signal without needing the pre-bridge clock
    /// override. Default `false`.
    pub use_output_pacing: bool,
    /// When true, `write_samples` never blocks the renderer waiting for the
    /// output buffer to drain: it pushes what fits below the back-pressure
    /// threshold and drops the overflow immediately. This decouples the
    /// producer from the DAC consumption clock — the source (e.g. mpv over a
    /// pipe) is no longer throttled by the ring filling up. Diagnostic toggle:
    /// it removes the back-pressure relaxation sawtooth at the cost of dropped
    /// samples on overflow. Default `false` (back-pressure active).
    pub disable_backpressure: bool,
}

impl Default for AdaptiveResamplingConfig {
    fn default() -> Self {
        Self {
            enable_far_mode: true,
            force_silence_in_far_mode: true,
            hard_recover_high_in_far_mode: true,
            hard_recover_low_in_far_mode: false,
            far_mode_return_fade_in_ms: 500,
            // kp and ki are in ppm/ms (parts-per-million of ratio correction per ms of error).
            // kp: proportional gain — ppm of correction per ms of current drift.
            // ki: integral gain — ppm of correction per ms of accumulated drift.
            kp_near: 1.0,
            ki: 1.0,
            integral_discharge_ratio: 0.25,
            max_adjust: 0.01,
            update_interval_callbacks: 1,
            high_recover_entry_margin_ms: 1000,
            low_recover_settle_stable_ms: 200.0,
            low_recover_entry_margin_ms: 18.0,
            low_recover_exit_margin_ms: 6.0,
            low_recover_settle_margin_ms: 6.0,
            low_recover_refill_delta_alpha: 0.5,
            control_smoothing_cutoff_hz: 0.5,
            control_smoothing_order: 1,
            paused: false,
            use_pre_bridge_clock: false,
            use_output_pacing: false,
            disable_backpressure: false,
        }
    }
}

pub const ADAPTIVE_BAND_NONE: u8 = 0;
pub const ADAPTIVE_BAND_NEAR: u8 = 1;
pub const ADAPTIVE_BAND_FAR: u8 = 2;
pub const LOCAL_RESAMPLER_MAX_RELATIVE_RATIO: f64 = 2.0;

pub fn local_resampler_ratio_bounds(base_ratio: f64) -> (f64, f64) {
    let relative_ratio = LOCAL_RESAMPLER_MAX_RELATIVE_RATIO.max(1.0);
    (base_ratio / relative_ratio, base_ratio * relative_ratio)
}

pub fn clamp_ratio_for_local_resampler(base_ratio: f64, ratio: f64) -> f64 {
    let (min_ratio, max_ratio) = local_resampler_ratio_bounds(base_ratio);
    ratio.clamp(min_ratio, max_ratio)
}

pub fn adaptive_band_name(band: u8) -> Option<&'static str> {
    match band {
        ADAPTIVE_BAND_NEAR => Some("near"),
        ADAPTIVE_BAND_FAR => Some("far"),
        _ => None,
    }
}

pub fn adaptive_runtime_state_code(name: &str) -> u8 {
    match name {
        "stable" => 0,
        "low-recover" => 1,
        "settling" => 2,
        "high-recover" => 3,
        _ => 255,
    }
}

pub fn adaptive_runtime_state_name_from_code(code: u8) -> Option<&'static str> {
    match code {
        0 => Some("stable"),
        1 => Some("low-recover"),
        2 => Some("settling"),
        3 => Some("high-recover"),
        _ => None,
    }
}

#[derive(Debug, Clone, Default)]
pub struct AdaptiveControllerState {
    pub accumulated_drift: f64,
}

#[derive(Debug, Clone, Copy)]
pub struct AdaptiveControlStep {
    pub drift: i64,
    pub p_term: f64,
    pub i_term: f64,
    pub consume_adjust: f64,
    pub current_ratio: f64,
    pub band: u8,
}

/// Compute one PI controller step.
///
/// `kp_near` and `ki` are expressed in **ppm/ms** (parts-per-million of ratio correction
/// per millisecond of error). This makes them independent of sample rate and channel count.
///
/// `samples_per_ms` converts the sample-domain drift to milliseconds before the gains are
/// applied. Pass `sample_rate * channel_count / 1000` (as f64) at the call site.
pub fn compute_adaptive_step(
    state: &mut AdaptiveControllerState,
    config: &AdaptiveResamplingConfig,
    available_samples: usize,
    target_buffer_fill: usize,
    high_recover_entry_margin_samples: usize,
    base_ratio: f64,
    deadband_samples: usize,
    max_integral_term: f64,
    samples_per_ms: f64,
) -> AdaptiveControlStep {
    let drift = available_samples as i64 - target_buffer_fill as i64;
    let drift_ms = if samples_per_ms > 0.0 {
        drift as f64 / samples_per_ms
    } else {
        drift as f64
    };
    let max_adjust = config.max_adjust.max(0.0);
    let min_consume_adjust = (1.0 - max_adjust).max(0.000_001);
    let max_consume_adjust = 1.0 + max_adjust;
    let p_term = drift_ms * config.kp_near / 1_000_000.0;

    if drift.unsigned_abs() as usize > deadband_samples {
        // When the error crosses the target, dump most of the integral energy so the
        // controller does not keep pushing in the old direction for several callbacks.
        if state.accumulated_drift != 0.0
            && drift_ms != 0.0
            && state.accumulated_drift.signum() != drift_ms.signum()
        {
            state.accumulated_drift *= config.integral_discharge_ratio.clamp(0.0, 1.0);
        }

        let current_i_term = state.accumulated_drift * config.ki / 1_000_000.0;
        let unsaturated_consume_adjust = 1.0 + p_term + current_i_term;
        let saturated_high = unsaturated_consume_adjust >= max_consume_adjust;
        let saturated_low = unsaturated_consume_adjust <= min_consume_adjust;
        let pushes_further_into_saturation =
            (saturated_high && drift_ms > 0.0) || (saturated_low && drift_ms < 0.0);

        if !pushes_further_into_saturation {
            // accumulated_drift is in ms
            state.accumulated_drift += drift_ms;
            let integral_contribution = state.accumulated_drift * config.ki / 1_000_000.0;
            if integral_contribution.abs() > max_integral_term && config.ki > 0.0 {
                state.accumulated_drift =
                    (max_integral_term * 1_000_000.0 / config.ki) * integral_contribution.signum();
            }
        }
    }

    let is_far = config.enable_far_mode
        && high_recover_entry_margin_samples > 0
        && (drift.unsigned_abs() as usize) >= high_recover_entry_margin_samples;
    let band = if is_far {
        ADAPTIVE_BAND_FAR
    } else {
        ADAPTIVE_BAND_NEAR
    };
    let i_term = state.accumulated_drift * config.ki / 1_000_000.0;
    let consume_adjust = (1.0 + p_term + i_term).clamp(min_consume_adjust, max_consume_adjust);
    let current_ratio = base_ratio / consume_adjust;

    AdaptiveControlStep {
        drift,
        p_term,
        i_term,
        consume_adjust,
        current_ratio,
        band,
    }
}

// cpal-backed realtime output, shared by the Windows (ASIO) and macOS
// (CoreAudio) backends. Not built on Linux (PipeWire handles output there).
#[cfg(any(target_os = "windows", target_os = "macos"))]
pub mod cpal_output;
#[cfg(target_os = "linux")]
pub mod pipewire;

#[cfg(target_os = "linux")]
pub use pipewire::{PipewireBufferConfig, PipewireWriter, list_pipewire_output_devices};

#[cfg(target_os = "linux")]
pub type PipewireAdaptiveResamplingConfig = AdaptiveResamplingConfig;

// On Windows the cpal writer is the ASIO backend; on macOS it is CoreAudio.
// Both share `cpal_output::CpalWriter`; expose them under platform-specific
// aliases so the CLI keeps stable, descriptive names.
#[cfg(target_os = "windows")]
pub use cpal_output::{CpalWriter as AsioWriter, list_output_devices as list_asio_devices};

#[cfg(target_os = "macos")]
pub use cpal_output::{
    CpalWriter as CoreAudioWriter, list_output_devices as list_coreaudio_devices,
};

#[cfg(target_os = "macos")]
pub type CoreAudioAdaptiveResamplingConfig = AdaptiveResamplingConfig;
