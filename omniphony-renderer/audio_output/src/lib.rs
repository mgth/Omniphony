#[derive(Debug, Clone)]
pub struct AdaptiveResamplingConfig {
    pub enable_far_mode: bool,
    pub force_silence_in_far_mode: bool,
    pub hard_recover_in_far_mode: bool,
    pub far_mode_return_fade_in_ms: u32,
    pub kp_near: f64,
    pub kp_far: f64,
    pub ki: f64,
    pub max_adjust: f64,
    pub max_adjust_far: f64,
    pub update_interval_callbacks: u32,
    pub near_far_threshold_ms: u32,
    pub measurement_smoothing_alpha: f64,
}

impl Default for AdaptiveResamplingConfig {
    fn default() -> Self {
        Self {
            enable_far_mode: true,
            force_silence_in_far_mode: false,
            hard_recover_in_far_mode: false,
            far_mode_return_fade_in_ms: 0,
            kp_near: 0.00001,
            kp_far: 0.00002,
            ki: 0.0000005,
            max_adjust: 0.01,
            max_adjust_far: 0.02,
            update_interval_callbacks: 10,
            near_far_threshold_ms: 120,
            measurement_smoothing_alpha: 0.15,
        }
    }
}

pub const ADAPTIVE_BAND_NONE: u8 = 0;
pub const ADAPTIVE_BAND_NEAR: u8 = 1;
pub const ADAPTIVE_BAND_FAR: u8 = 2;

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
    pub smoothed_control_available: f64,
    pub smoothed_total_available: f64,
    /// Set to `true` after the first control-latency EMA sample so that a real
    /// value of 0.0 is not mistaken for "uninitialised".
    pub control_ema_initialized: bool,
    /// Set to `true` after the first total-latency EMA sample so that a real
    /// value of 0.0 is not mistaken for "uninitialised".
    pub total_ema_initialized: bool,
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

pub fn apply_ema(previous: &mut f64, initialized: &mut bool, sample: f64, alpha: f64) -> f64 {
    let alpha = alpha.clamp(0.0, 1.0);
    if !*initialized {
        *previous = sample;
        *initialized = true;
    } else {
        *previous += alpha * (sample - *previous);
    }
    *previous
}

pub fn compute_adaptive_step(
    state: &mut AdaptiveControllerState,
    config: &AdaptiveResamplingConfig,
    available_samples: usize,
    target_buffer_fill: usize,
    near_far_threshold_samples: usize,
    base_ratio: f64,
    deadband_samples: usize,
    max_integral_term: f64,
) -> AdaptiveControlStep {
    let drift = available_samples as i64 - target_buffer_fill as i64;

    if drift.unsigned_abs() as usize > deadband_samples {
        state.accumulated_drift += drift as f64;
        let integral_contribution = state.accumulated_drift * config.ki;
        if integral_contribution.abs() > max_integral_term && config.ki > 0.0 {
            state.accumulated_drift =
                (max_integral_term / config.ki) * integral_contribution.signum();
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
    let kp = if is_far {
        config.kp_far
    } else {
        config.kp_near
    };
    let max_adjust = if is_far {
        config.max_adjust_far
    } else {
        config.max_adjust
    };
    let p_term = drift as f64 * kp / 100.0;
    let i_term = state.accumulated_drift * config.ki;
    let consume_adjust = (1.0 + p_term + i_term).clamp(1.0 - max_adjust, 1.0 + max_adjust);
    let current_ratio = (base_ratio / consume_adjust).clamp(
        base_ratio * (1.0 - max_adjust),
        base_ratio * (1.0 + max_adjust),
    );

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

#[cfg(target_os = "linux")]
pub use pipewire::{PipewireBufferConfig, PipewireWriter, list_pipewire_output_devices};

#[cfg(target_os = "linux")]
pub type PipewireAdaptiveResamplingConfig = AdaptiveResamplingConfig;

#[cfg(target_os = "windows")]
pub use asio::{AsioWriter, list_asio_devices};
