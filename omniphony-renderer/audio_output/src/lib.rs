pub mod control;
pub mod adaptive_runtime;
pub mod resampler_fifo;
pub mod ring_buffer_io;

#[derive(Debug, Clone)]
pub struct AdaptiveResamplingConfig {
    pub enable_far_mode: bool,
    pub force_silence_in_far_mode: bool,
    pub hard_recover_in_far_mode: bool,
    pub far_mode_return_fade_in_ms: u32,
    pub kp_near: f64,
    pub ki: f64,
    pub max_adjust: f64,
    pub update_interval_callbacks: u32,
    pub near_far_threshold_ms: u32,
    /// When true the PI controller is frozen: the current ratio is held as-is.
    pub paused: bool,
}

impl Default for AdaptiveResamplingConfig {
    fn default() -> Self {
        Self {
            enable_far_mode: true,
            force_silence_in_far_mode: false,
            hard_recover_in_far_mode: false,
            far_mode_return_fade_in_ms: 0,
            // kp and ki are in ppm/ms (parts-per-million of ratio correction per ms of error).
            // kp: proportional gain — ppm of correction per ms of current drift.
            // ki: integral gain — ppm of correction per ms of accumulated drift.
            kp_near: 10.0,
            ki: 50.0,
            max_adjust: 0.01,
            update_interval_callbacks: 10,
            near_far_threshold_ms: 120,
            paused: false,
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
    near_far_threshold_samples: usize,
    base_ratio: f64,
    deadband_samples: usize,
    max_integral_term: f64,
    samples_per_ms: f64,
) -> AdaptiveControlStep {
    let drift = available_samples as i64 - target_buffer_fill as i64;
    let drift_ms = if samples_per_ms > 0.0 { drift as f64 / samples_per_ms } else { drift as f64 };
    let max_adjust = config.max_adjust.max(0.0);
    let min_consume_adjust = (1.0 - max_adjust).max(0.000_001);
    let max_consume_adjust = 1.0 + max_adjust;
    let p_term = drift_ms * config.kp_near / 1_000_000.0;

    if drift.unsigned_abs() as usize > deadband_samples {
        // When the error crosses the target, dump most of the integral energy so the
        // controller does not keep pushing in the old direction for several callbacks.
        if state.accumulated_drift != 0.0 && drift_ms != 0.0
            && state.accumulated_drift.signum() != drift_ms.signum()
        {
            state.accumulated_drift *= 0.25;
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
        && near_far_threshold_samples > 0
        && (drift.unsigned_abs() as usize) >= near_far_threshold_samples;
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

pub mod asio;
#[cfg(target_os = "linux")]
pub mod pipewire;

pub use control::{
    AppliedAudioOutputState, AudioControl, OutputDeviceOption, RequestedAudioOutputConfig,
};

#[cfg(target_os = "linux")]
pub use pipewire::{PipewireBufferConfig, PipewireWriter, list_pipewire_output_devices};

#[cfg(target_os = "linux")]
pub type PipewireAdaptiveResamplingConfig = AdaptiveResamplingConfig;

#[cfg(target_os = "windows")]
pub use asio::{AsioWriter, list_asio_devices};
