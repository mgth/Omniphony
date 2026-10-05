//! The output callback, once, for every device backend.
//!
//! PipeWire and cpal (ASIO, CoreAudio) each used to carry their own copy of
//! everything between "the device wants N frames" and "here they are": the
//! latency measurement, the adaptive servo, the low-recover / far-mode state
//! machine and the recovery glue around it. The copies had drifted — cpal fed
//! the servo a constant in place of the resampler FIFO level, never
//! acknowledged a recovery reacquire, and reset its resampler mid-playback when
//! the startup settle ended — and only the PipeWire one was ever listened to.
//!
//! [`OutputCallbackCore::process`] is that work, written once against a plain
//! `&mut [f32]`. A backend's callback is left with what is genuinely its own:
//! getting the device buffer, the downstream latency it can measure, and for
//! PipeWire the stream controls it drives. The core needs no device, so the
//! tests below run the cpal path (a device wider than the audio) and the
//! PipeWire paths (resampled, and copied straight through) on synthetic input.
//!
//! Everything here runs on the device's realtime thread: no lock is waited on,
//! nothing is logged or allocated in steady state. `tests/realtime_callbacks.rs`
//! scans this module whole.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

use parking_lot::Mutex;
use rubato::Resampler;

use crate::callback_log::{CallbackLog, CallbackLogReader, callback_event};
use crate::callback_state::{CallbackState, ResamplerState};
use crate::output_telemetry::{LatencySample, OutputTelemetry};
use crate::ring_buffer_io::RingReader;
use crate::{
    ADAPTIVE_BAND_FAR, AdaptiveResamplingConfig, adaptive_band_name,
    adaptive_runtime::{
        AdaptiveRuntimeState, FarModeDecision, FarModeStepCtx, FarModeStepInputs,
        LatencyMetricTargets, LatencyMetrics, LowRecoverPhase, MAX_INTEGRAL_TERM,
        PRE_BRIDGE_CALIBRATION_CALLBACKS, compute_hard_recover_high_plan,
        far_mode_band_from_latency, far_mode_step, note_refill_or_underrun,
        output_to_input_domain_samples, paused_rate_adjust, postprocess_interleaved_output,
        reset_adaptive_runtime, run_adaptive_servo, should_run_adaptive_servo,
        update_latency_metrics, zero_pad_tail,
    },
    adaptive_runtime_state_name_from_code, clamp_ratio_for_local_resampler,
    resampler_fifo::ResamplerFifoEngine,
};

// The latency servo of the direct-copy path, which steers the stream's own
// rate instead of a local resampler. Not aggressive correction, only holding
// the ring close to its target without audible glitches.
const LATENCY_SERVO_P_GAIN: f64 = 0.000004;
const LATENCY_SERVO_I_GAIN: f64 = 0.0000002;
const LATENCY_SERVO_MAX_RATE_ADJUST: f64 = 0.03;

/// What the callback reads from the control side and publishes to it. The
/// writer keeps a clone; every field is either an atomic or, for the config,
/// a mutex the callback only ever `try_lock`s.
#[derive(Clone)]
pub struct CallbackShared {
    /// Written by the control thread; the callback keeps its own copy and
    /// refreshes it without waiting.
    pub live_config: Arc<Mutex<AdaptiveResamplingConfig>>,
    /// Snap the ratio back to its configured value and reset the integrator.
    pub reset_ratio_requested: Arc<AtomicBool>,
    /// The servo's consume adjust as displayed, `f32` bits; 1.0 at rest.
    pub rate_adjust: Arc<AtomicU32>,
    /// 0 = none, 1 = near, 2 = far.
    pub adaptive_band: Arc<AtomicU8>,
    /// `adaptive_runtime_state_code` of the state machine's phase.
    pub runtime_state: Arc<AtomicU8>,
    /// The consume rate the stream itself should run at, `f32` bits. Only the
    /// direct-copy path steers it; the backend applies it off the callback.
    pub native_rate: Arc<AtomicU32>,
    pub telemetry: OutputTelemetry,
}

impl CallbackShared {
    pub fn new(config: AdaptiveResamplingConfig) -> Self {
        Self {
            live_config: Arc::new(Mutex::new(config)),
            reset_ratio_requested: Arc::new(AtomicBool::new(false)),
            rate_adjust: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            adaptive_band: Arc::new(AtomicU8::new(0)),
            runtime_state: Arc::new(AtomicU8::new(0)),
            native_rate: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            telemetry: OutputTelemetry::new(),
        }
    }

    pub fn rate_adjust(&self) -> f32 {
        f32::from_bits(self.rate_adjust.load(Ordering::Relaxed))
    }

    pub fn adaptive_band(&self) -> Option<&'static str> {
        adaptive_band_name(self.adaptive_band.load(Ordering::Relaxed))
    }

    pub fn adaptive_runtime_state(&self) -> Option<&'static str> {
        adaptive_runtime_state_name_from_code(self.runtime_state.load(Ordering::Relaxed))
    }

    pub fn request_ratio_reset(&self) {
        self.reset_ratio_requested.store(true, Ordering::Relaxed);
    }
}

/// The post-rendering pacer as the callback sees it: a fixed share of the
/// latency target, and the flags that make its drain re-prime after a
/// recovery.
pub struct PacerLink {
    pub pre_roll_complete: Arc<AtomicBool>,
    pub flush_requested: Arc<AtomicBool>,
    /// The pacer's capacity, input-domain samples. Taken off the ring's target
    /// so that ring + pacer lands on the configured latency.
    pub buffer_samples: usize,
}

/// What is fixed for the life of the stream.
pub struct CallbackContext {
    /// Channels of the rendered audio, as the ring holds them.
    pub channel_count: usize,
    /// Channels of a device frame, `>= channel_count`; the extra ones are
    /// silent. The direct-copy path needs the two equal.
    pub dest_channels: usize,
    pub input_sample_rate: u32,
    pub output_sample_rate: u32,
    /// The servo's setpoint, input-domain samples, pacer included.
    pub target_buffer_fill: usize,
    /// Drift, in samples, the servo leaves alone.
    pub servo_deadband_samples: usize,
    /// Run the PI servo on the local resampler's ratio.
    pub adaptive_resampling: bool,
    pub pacer: Option<PacerLink>,
    /// The capture side's source clock, µs as `f64` bits, for
    /// `use_pre_bridge_clock`. `None` where there is no capture side.
    pub input_clock_us: Option<Arc<AtomicU64>>,
}

/// One device callback's worth of output, from the ring to the device buffer.
pub struct OutputCallbackCore<R> {
    ctx: CallbackContext,
    shared: CallbackShared,
    ring: RingReader,
    log: CallbackLog,
    state: CallbackState<R>,
    /// `target_buffer_fill` less the pacer's share: what the ring aims for.
    ring_target: usize,
    samples_per_ms: usize,
}

/// What the measurement half of a callback hands the rendering half.
struct Measured {
    callback_count: u64,
    metrics: LatencyMetrics,
    available: usize,
    callback_input_domain_samples: usize,
    downstream_latency_ms: f32,
    is_pi_paused: bool,
}

impl<R: Resampler<f32>> OutputCallbackCore<R> {
    /// `resampler` is `None` for the direct-copy path: input and output rates
    /// equal, no adaptive resampling, the stream's own rate steered instead.
    /// `config` is what `shared.live_config` holds now, the callback's first
    /// copy of it. The returned reader is the other end of the callback's log,
    /// for a [`crate::callback_log::CallbackLogDrain`]; `log_target` is the
    /// target its records are logged under.
    pub fn new(
        ctx: CallbackContext,
        shared: CallbackShared,
        ring: RingReader,
        resampler: Option<R>,
        config: AdaptiveResamplingConfig,
        log_target: &'static str,
    ) -> (Self, CallbackLogReader) {
        assert!(ctx.channel_count > 0, "an output stream has channels");
        assert!(
            ctx.dest_channels >= ctx.channel_count,
            "the device frame holds the audio frame"
        );
        assert!(
            resampler.is_some() || ctx.dest_channels == ctx.channel_count,
            "the direct-copy path copies whole frames"
        );
        let configured_ratio = ctx.output_sample_rate as f64 / ctx.input_sample_rate as f64;
        let state = CallbackState::new(resampler, ctx.channel_count, configured_ratio, config);
        let pacer_samples = ctx.pacer.as_ref().map_or(0, |p| p.buffer_samples);
        let (log, reader) = CallbackLog::new(log_target);
        let core = Self {
            ring_target: ctx.target_buffer_fill.saturating_sub(pacer_samples),
            samples_per_ms: (ctx.input_sample_rate as usize).saturating_mul(ctx.channel_count)
                / 1000,
            ctx,
            shared,
            ring,
            log,
            state,
        };
        (core, reader)
    }

    /// Callbacks run so far.
    pub fn callback_count(&self) -> u64 {
        self.state.runtime.callback_count
    }

    /// The callback's log, for the events a backend reports itself.
    pub fn log(&mut self) -> &mut CallbackLog {
        &mut self.log
    }

    /// Fill `dest`, whole device frames of interleaved `f32`, from the ring.
    /// Every sample is written. `downstream_latency_ms` is what lies between
    /// the device buffer and the listener as far as the backend can tell; it
    /// only enters the published latency.
    pub fn process(&mut self, dest: &mut [f32], downstream_latency_ms: f32) {
        let whole = dest.len() - dest.len() % self.ctx.dest_channels;
        let (dest, partial) = dest.split_at_mut(whole);
        partial.fill(0.0);
        let measured = self.measure(dest.len() / self.ctx.dest_channels, downstream_latency_ms);
        if self.state.resampler.engine.is_some() {
            self.render_resampled(dest, &measured);
        } else {
            self.render_direct(dest, &measured);
        }
        let telemetry = &self.shared.telemetry;
        telemetry.runtime_state_code_bits.store(
            (self.shared.runtime_state.load(Ordering::Relaxed) as f64).to_bits(),
            Ordering::Relaxed,
        );
        let ratio_ppm = (self.state.resampler.effective_ratio - 1.0) * 1_000_000.0;
        telemetry
            .output_effective_ratio_ppm_bits
            .store(ratio_ppm.to_bits(), Ordering::Relaxed);
    }

    /// Everything up to the servo: timing, test controls, the config refresh,
    /// the buffer levels and the latency they make.
    fn measure(&mut self, frames: usize, downstream_latency_ms: f32) -> Measured {
        let Self {
            ctx,
            shared,
            ring,
            state,
            ring_target,
            samples_per_ms,
            ..
        } = self;
        let telemetry = &shared.telemetry;
        let channels = ctx.channel_count;

        let now = Instant::now();
        let dt_us = state.last_callback_at.map_or(0, |prev| {
            now.saturating_duration_since(prev).as_micros() as u64
        });
        state.last_callback_at = Some(now);
        telemetry
            .output_callback_dt_us_bits
            .store((dt_us as f64).to_bits(), Ordering::Relaxed);

        // A flush that gave up leaves the rest for this end to drop.
        ring.apply_requested_discard();
        let available = ring.available();
        telemetry
            .output_ring_input_samples_bits
            .store((available as f64).to_bits(), Ordering::Relaxed);
        let callback_count = state.runtime.advance_callback();

        // Test control: snap the ratio back and reset the integrator.
        if shared.reset_ratio_requested.swap(false, Ordering::Relaxed) {
            let rs = &mut state.resampler;
            if let Some(resampler) = rs.engine.as_mut() {
                let _ = resampler.set_resample_ratio(rs.configured_ratio, false);
            }
            let reset = reset_adaptive_runtime(&mut state.runtime, rs.configured_ratio);
            rs.effective_ratio = reset.effective_resample_ratio;
            shared
                .rate_adjust
                .store(reset.displayed_rate_adjust.to_bits(), Ordering::Relaxed);
            shared
                .native_rate
                .store(1.0f32.to_bits(), Ordering::Relaxed);
            shared
                .adaptive_band
                .store(reset.adaptive_band, Ordering::Relaxed);
        }
        // The one config read of this callback, without waiting: on contention
        // the previous copy stands until the next callback. Everything below
        // reads the copy, so the callback also sees one consistent config.
        if let Some(config) = shared.live_config.try_lock() {
            state.adaptive_config.clone_from(&config);
        }
        let config = &state.adaptive_config;
        let fifo = &state.resampler.fifo;
        let effective_ratio = state.resampler.effective_ratio;

        // The resampler FIFO's raw level oscillates with the chunk cycle; the
        // servo sees it through the control smoothing, and the components
        // plot sees it as is.
        let fifo_input_domain = output_to_input_domain_samples(fifo.output_len(), effective_ratio);
        let pending = fifo.pending_input_samples();
        telemetry
            .output_fifo_input_domain_samples_bits
            .store((fifo_input_domain as f64).to_bits(), Ordering::Relaxed);
        telemetry
            .output_resampler_pending_input_samples_bits
            .store((pending as f64).to_bits(), Ordering::Relaxed);

        let callback_input_domain_samples =
            output_to_input_domain_samples(frames * channels, effective_ratio);
        // Cumulative flow (written − drained), published for observation only:
        // as the servo's input it deadlocked the bootstrap, since the callback
        // drains before anything is written (LATENCY_DAC_SAWTOOTH_REPORT.md).
        let drained = telemetry
            .cumulative_drained_input_samples
            .fetch_add(callback_input_domain_samples as u64, Ordering::Relaxed)
            + callback_input_domain_samples as u64;
        let written = telemetry
            .cumulative_written_input_samples
            .load(Ordering::Relaxed);
        telemetry.cumulative_flow_control_available_bits.store(
            (written.saturating_sub(drained) as f64).to_bits(),
            Ordering::Relaxed,
        );

        let callback_dt_s = if dt_us > 0 {
            dt_us as f64 / 1_000_000.0
        } else {
            frames as f64 / ctx.output_sample_rate.max(1) as f64
        };
        let pacer_samples = ctx.pacer.as_ref().map_or(0, |p| p.buffer_samples);
        let mut metrics = update_latency_metrics(
            &mut state.runtime,
            available,
            fifo_input_domain,
            pending,
            pacer_samples,
            callback_input_domain_samples,
            channels,
            ctx.input_sample_rate,
            downstream_latency_ms,
            config.control_smoothing_cutoff_hz,
            config.control_smoothing_order,
            callback_dt_s,
            LatencyMetricTargets {
                measured_latency_ms_bits: &telemetry.measured_latency_ms_bits,
                control_latency_ms_bits: &telemetry.control_latency_ms_bits,
            },
        );

        // Pre-bridge clock: once the capture side runs, the servo follows the
        // drift between its source clock and what this callback drained, which
        // the decoder's batching does not ripple. The first
        // PRE_BRIDGE_CALIBRATION_CALLBACKS readings are averaged into the
        // offset that puts that signal on the target; the state machine and
        // the published latency keep the ring-based figures.
        let clock_us = ctx
            .input_clock_us
            .as_ref()
            .map(|clock| f64::from_bits(clock.load(Ordering::Relaxed)));
        let pre_bridge_requested = config.use_pre_bridge_clock && clock_us.is_some();
        let clock_us = clock_us.unwrap_or(0.0);
        let pre_bridge_ready = pre_bridge_requested && clock_us > 0.0;
        if pre_bridge_ready {
            let clock_samples =
                (clock_us / 1_000_000.0 * ctx.input_sample_rate as f64 * channels as f64) as i64;
            let runtime = &mut state.runtime;
            if !runtime.pre_bridge_offset_initialized {
                runtime.pre_bridge_offset_accum += (clock_samples - drained as i64) as i128;
                runtime.pre_bridge_offset_count += 1;
                if runtime.pre_bridge_offset_count >= PRE_BRIDGE_CALIBRATION_CALLBACKS {
                    runtime.pre_bridge_offset_samples = (runtime.pre_bridge_offset_accum
                        / runtime.pre_bridge_offset_count as i128)
                        as i64;
                    runtime.pre_bridge_offset_initialized = true;
                }
            } else {
                let drift = clock_samples - drained as i64 - runtime.pre_bridge_offset_samples;
                metrics.smoothed_control_available = (*ring_target as i64 + drift).max(0) as usize;
            }
        }
        // Freeze the PI only until the first source chunk arrives; during the
        // calibration window the ring-based servo keeps the ring on target, so
        // the offset is averaged around the steady state.
        let is_pi_paused = config.paused || (pre_bridge_requested && !pre_bridge_ready);

        // The displayed smoothed latency includes the pacer's fixed share, as
        // the control latency does; the servo keeps the pacer-free figure.
        telemetry.publish_latency(
            &LatencySample {
                smoothed_control_available: metrics
                    .smoothed_control_available
                    .saturating_add(pacer_samples),
                control_latency_ms: metrics.control_latency_ms,
                rate_adjust: shared.rate_adjust(),
                avail_input_samples: available,
                output_fifo_input_domain_samples: fifo_input_domain,
                resampler_pending_input_samples: pending,
            },
            channels as u32,
            ctx.input_sample_rate,
        );
        // Band classification on the raw level, so the hysteresis bands act on
        // the real buffer; the servo gets the smoothed one.
        let band = far_mode_band_from_latency(
            config,
            metrics.control_available,
            *ring_target,
            *samples_per_ms,
        );
        shared.adaptive_band.store(band, Ordering::Relaxed);

        Measured {
            callback_count,
            metrics,
            available,
            callback_input_domain_samples,
            downstream_latency_ms,
            is_pi_paused,
        }
    }

    /// Run the far-mode state machine for this callback.
    fn far_mode(&mut self, m: &Measured, resample_ratio: f64) -> FarModeDecision {
        let telemetry = &self.shared.telemetry;
        far_mode_step(
            &mut self.state.runtime,
            &FarModeStepCtx {
                adaptive_config: &self.state.adaptive_config,
                channel_count: self.ctx.channel_count,
                input_sample_rate: self.ctx.input_sample_rate,
                output_sample_rate: self.ctx.output_sample_rate,
                runtime_state_code: &self.shared.runtime_state,
                latency: LatencyMetricTargets {
                    measured_latency_ms_bits: &telemetry.measured_latency_ms_bits,
                    control_latency_ms_bits: &telemetry.control_latency_ms_bits,
                },
            },
            FarModeStepInputs {
                is_far_band: self.shared.adaptive_band.load(Ordering::Relaxed) == ADAPTIVE_BAND_FAR,
                control_available: m.metrics.control_available,
                smoothed_control_available: m.metrics.smoothed_control_available,
                target_buffer_fill: self.ring_target,
                callback_input_domain_samples: m.callback_input_domain_samples,
                resample_ratio,
                pacer_buffer_samples: self.ctx.pacer.as_ref().map_or(0, |p| p.buffer_samples),
                graph_latency_ms: m.downstream_latency_ms,
            },
        )
    }

    /// The local resampler between the ring and the device: the PI servo
    /// steers its ratio.
    fn render_resampled(&mut self, dest: &mut [f32], m: &Measured) {
        let channels = self.ctx.channel_count;
        let width = self.ctx.dest_channels;
        let needed = dest.len() / width * channels;

        if self.ctx.adaptive_resampling
            && !m.is_pi_paused
            && self.state.runtime.low_recover_phase == LowRecoverPhase::Inactive
        {
            self.run_resampler_servo(m);
        }

        let low_recover_was_active =
            self.state.runtime.low_recover_phase != LowRecoverPhase::Inactive;
        let effective_ratio = self.state.resampler.effective_ratio;
        let far = self.far_mode(m, effective_ratio);

        let Self {
            ctx,
            shared,
            ring,
            log,
            state,
            ring_target,
            ..
        } = self;
        let CallbackState {
            resampler: rs,
            runtime,
            ..
        } = state;
        let ResamplerState {
            engine,
            fifo,
            effective_ratio,
            configured_ratio,
        } = rs;
        let Some(resampler) = engine.as_mut() else {
            return;
        };

        if far.hold_low_recover {
            // The ratio goes back to nominal, and so does the rate on display.
            hold_servo_at_rest(shared);
            if !low_recover_was_active {
                resampler.reset();
                let _ = resampler.set_resample_ratio(*configured_ratio, false);
                fifo.reset();
            } else if effective_ratio.to_bits() != configured_ratio.to_bits() {
                let _ = resampler.set_resample_ratio(*configured_ratio, false);
            }
            *effective_ratio = *configured_ratio;
        }

        if far.recovery_reacquire_pending {
            // Back from a muted recovery: start over from a clean resampler.
            resampler.reset();
            let _ = resampler.set_resample_ratio(*configured_ratio, false);
            fifo.reset();
            *effective_ratio = *configured_ratio;
            acknowledge_reacquire(runtime, shared, ctx.pacer.as_ref());
            let refilled = fifo.ensure_output_samples(ring, resampler, needed);
            if let Err(e) = refilled {
                callback_event!(log, Error, "resampler error during recovery reacquire"; e);
                dest.fill(0.0);
            } else if far.mute_far_output {
                fifo.discard_samples(needed);
                dest.fill(0.0);
            } else {
                play_from_fifo(
                    fifo,
                    dest,
                    channels,
                    width,
                    false,
                    runtime,
                    log,
                    "resampler output underrun: zero-padding the remainder",
                );
            }
            return;
        }

        if far.hold_low_recover {
            let muted_consume = if far.mute_far_output && far.consume_while_muted {
                needed
            } else {
                0
            };
            let prepared = if far.mute_far_output {
                muted_consume
            } else {
                needed
            }
            .saturating_add(far.low_recover_trim_output_samples);
            if prepared > 0 {
                if let Err(e) = fifo.ensure_output_samples(ring, resampler, prepared) {
                    callback_event!(log, Error, "resampler error"; e);
                } else {
                    fifo.discard_samples(far.low_recover_trim_output_samples);
                    fifo.discard_samples(muted_consume);
                }
            }
        } else if let Err(e) = fifo.ensure_output_samples(ring, resampler, needed) {
            callback_event!(log, Error, "resampler error"; e);
        }

        if far.hard_recover_high {
            let plan = compute_hard_recover_high_plan(
                m.callback_input_domain_samples,
                m.metrics.control_available,
                *ring_target,
                *effective_ratio,
                channels,
            );
            if let Err(e) =
                fifo.ensure_output_samples(ring, resampler, plan.desired_consume_output_samples)
            {
                callback_event!(log, Error, "resampler error"; e);
            }
            fifo.discard_samples(plan.desired_consume_output_samples);
            dest.fill(0.0);
        } else if far.hold_low_recover && far.mute_far_output {
            dest.fill(0.0);
        } else {
            play_from_fifo(
                fifo,
                dest,
                channels,
                width,
                far.mute_far_output,
                runtime,
                log,
                "resampler underrun: zero-padding the remainder",
            );
        }
    }

    /// One PI step on the local resampler's ratio, when the interval is due.
    fn run_resampler_servo(&mut self, m: &Measured) {
        let Self {
            ctx,
            shared,
            log,
            state,
            ring_target,
            samples_per_ms,
            ..
        } = self;
        let CallbackState {
            resampler: rs,
            runtime,
            adaptive_config: config,
            adaptive_update_interval,
            ..
        } = state;
        let Some(resampler) = rs.engine.as_mut() else {
            return;
        };
        *adaptive_update_interval = config.update_interval_callbacks.max(1) as u64;
        if !should_run_adaptive_servo(
            m.callback_count,
            *adaptive_update_interval as u32,
            m.metrics.total_available_input_domain,
            ctx.channel_count,
        ) {
            return;
        }
        let mut decision = run_adaptive_servo(
            runtime,
            config,
            m.metrics,
            *ring_target,
            rs.configured_ratio,
            ctx.servo_deadband_samples,
            config.max_adjust.max(0.000_001),
            *samples_per_ms,
            *samples_per_ms as f64,
        );
        shared
            .adaptive_band
            .store(decision.adaptive_band, Ordering::Relaxed);

        let clamped =
            clamp_ratio_for_local_resampler(rs.configured_ratio, decision.step.current_ratio);
        decision.step.current_ratio = clamped;
        decision.step.consume_adjust = rs.configured_ratio / clamped;
        decision.displayed_rate_adjust = paused_rate_adjust(rs.configured_ratio, clamped);

        if let Err(e) = resampler.set_resample_ratio(clamped, true) {
            callback_event!(log, Warn, "failed to set the resampler ratio"; e);
        } else {
            rs.effective_ratio = clamped;
            if clamped.to_bits() != runtime.last_logged_ratio_bits {
                callback_event!(
                    log,
                    Debug,
                    "adaptive ratio applied",
                    base = rs.configured_ratio,
                    effective = clamped,
                    relative = clamped / rs.configured_ratio,
                    consume = decision.step.consume_adjust,
                    drift = decision.step.drift,
                    buf = m.metrics.control_available,
                    target = *ring_target
                );
            }
            runtime.last_logged_ratio_bits = clamped.to_bits();
        }
        shared
            .rate_adjust
            .store(decision.displayed_rate_adjust.to_bits(), Ordering::Relaxed);

        if m.callback_count % 100 == 0 {
            callback_event!(
                log,
                Debug,
                "adaptive",
                buf = m.metrics.control_available,
                target = *ring_target,
                drift = decision.step.drift,
                ratio = decision.step.current_ratio,
                base = rs.configured_ratio,
                p = decision.step.p_term,
                i = decision.step.i_term
            );
        }
    }

    /// No local resampler: the ring is copied straight to the device and the
    /// latency servo steers the stream's own rate through `native_rate`.
    fn render_direct(&mut self, dest: &mut [f32], m: &Measured) {
        if !m.is_pi_paused
            && m.callback_count % self.state.adaptive_update_interval == 0
            && self.state.runtime.low_recover_phase == LowRecoverPhase::Inactive
        {
            self.run_native_rate_servo(m);
        }

        let far = self.far_mode(m, 1.0);

        let Self {
            ctx,
            shared,
            ring,
            log,
            state,
            ring_target,
            ..
        } = self;
        let channels = ctx.channel_count;
        let to_read = (m.available / channels).min(dest.len() / channels) * channels;
        let runtime = &mut state.runtime;
        let discards = &mut state.recovery_discard_total;
        let telemetry = &shared.telemetry;

        if far.hold_low_recover {
            hold_servo_at_rest(shared);
        }

        if far.recovery_reacquire_pending {
            acknowledge_reacquire(runtime, shared, ctx.pacer.as_ref());
            if far.mute_far_output {
                let dropped = ring.discard(to_read);
                count_discards(discards, telemetry, dropped);
                if dropped < to_read {
                    callback_event!(
                        log,
                        Debug,
                        "recovery reacquire underfed while re-priming output",
                        consumed = dropped,
                        wanted = to_read
                    );
                }
                dest.fill(0.0);
            } else {
                play_from_ring(ring, dest, to_read, channels, false, runtime, log);
            }
        } else if far.hard_recover_high {
            let plan = compute_hard_recover_high_plan(
                m.callback_input_domain_samples,
                m.metrics.control_available,
                *ring_target,
                1.0,
                channels,
            );
            let dropped = ring.discard(plan.desired_consume_input_samples);
            count_discards(discards, telemetry, dropped);
            if dropped < plan.desired_consume_input_samples {
                callback_event!(
                    log,
                    Debug,
                    "far hard recover underfed while targeting exact recovery",
                    consumed = dropped,
                    wanted = plan.desired_consume_input_samples
                );
            }
            dest.fill(0.0);
        } else if far.hold_low_recover {
            let muted_consume = if far.mute_far_output && far.consume_while_muted {
                to_read
            } else {
                0
            };
            let to_discard = muted_consume.saturating_add(far.low_recover_trim_input_samples);
            if to_discard > 0 {
                let dropped = ring.discard(to_discard);
                count_discards(discards, telemetry, dropped);
                if dropped < to_discard {
                    callback_event!(
                        log,
                        Debug,
                        "low-recover muted consume underfed while stabilizing resume latency",
                        consumed = dropped,
                        wanted = to_discard
                    );
                }
            }
            if far.mute_far_output {
                dest.fill(0.0);
            } else {
                play_from_ring(ring, dest, to_read, channels, false, runtime, log);
            }
        } else {
            play_from_ring(
                ring,
                dest,
                to_read,
                channels,
                far.mute_far_output,
                runtime,
                log,
            );
        }
    }

    /// One step of the gentle latency servo on the stream's own rate.
    fn run_native_rate_servo(&mut self, m: &Measured) {
        let Self {
            shared,
            log,
            state,
            ring_target,
            samples_per_ms,
            ctx,
            ..
        } = self;
        let runtime = &mut state.runtime;
        let drift = m.metrics.smoothed_control_available as i64 - *ring_target as i64;
        if drift.unsigned_abs() as usize > ctx.servo_deadband_samples {
            let controller = &mut runtime.controller_state;
            controller.accumulated_drift += drift as f64;
            let integral = controller.accumulated_drift * LATENCY_SERVO_I_GAIN;
            if integral.abs() > MAX_INTEGRAL_TERM {
                controller.accumulated_drift =
                    (MAX_INTEGRAL_TERM / LATENCY_SERVO_I_GAIN) * integral.signum();
            }
        }
        let band = far_mode_band_from_latency(
            &state.adaptive_config,
            m.metrics.control_available,
            *ring_target,
            *samples_per_ms,
        );
        shared.adaptive_band.store(band, Ordering::Relaxed);
        let p_term = drift as f64 * LATENCY_SERVO_P_GAIN / 100.0;
        let i_term = runtime.controller_state.accumulated_drift * LATENCY_SERVO_I_GAIN;
        let consume_adjust = (1.0 + p_term + i_term).clamp(
            1.0 - LATENCY_SERVO_MAX_RATE_ADJUST,
            1.0 + LATENCY_SERVO_MAX_RATE_ADJUST,
        );
        shared
            .rate_adjust
            .store((consume_adjust as f32).to_bits(), Ordering::Relaxed);
        shared
            .native_rate
            .store((consume_adjust as f32).to_bits(), Ordering::Relaxed);

        if m.callback_count % 100 == 0 {
            callback_event!(
                log,
                Trace,
                "latency servo",
                buf = m.metrics.control_available,
                target = *ring_target,
                drift = drift,
                consume = consume_adjust
            );
        }
    }
}

/// A recovery has ended and its output was muted: return the servo to rest,
/// recalibrate the pre-bridge offset (the drained counter ran on while the
/// source clock may not have), and make the pacer drop what it queued before
/// and re-prime.
fn acknowledge_reacquire(
    runtime: &mut AdaptiveRuntimeState,
    shared: &CallbackShared,
    pacer: Option<&PacerLink>,
) {
    hold_servo_at_rest(shared);
    runtime.recovery_reacquire_pending = false;
    runtime.pre_bridge_offset_initialized = false;
    runtime.pre_bridge_offset_accum = 0;
    runtime.pre_bridge_offset_count = 0;
    if let Some(pacer) = pacer {
        pacer.flush_requested.store(true, Ordering::Release);
        pacer.pre_roll_complete.store(false, Ordering::Relaxed);
    }
}

/// A low recover holds the stream at its nominal rate: publish that, so the
/// display does not keep a correction no longer applied.
fn hold_servo_at_rest(shared: &CallbackShared) {
    shared
        .rate_adjust
        .store(1.0f32.to_bits(), Ordering::Relaxed);
    shared
        .native_rate
        .store(1.0f32.to_bits(), Ordering::Relaxed);
}

fn count_discards(total: &mut u64, telemetry: &OutputTelemetry, dropped: usize) {
    *total = total.saturating_add(dropped as u64);
    telemetry
        .recovery_discard_count_bits
        .store((*total as f64).to_bits(), Ordering::Relaxed);
}

/// Play what the FIFO holds into `dest`, device frames `width` wide of which
/// the first `channels` carry audio; silence the rest and note the shortfall.
fn play_from_fifo(
    fifo: &mut ResamplerFifoEngine,
    dest: &mut [f32],
    channels: usize,
    width: usize,
    mute: bool,
    runtime: &mut AdaptiveRuntimeState,
    log: &mut CallbackLog,
    underrun: &'static str,
) {
    let needed = dest.len() / width * channels;
    let held = fifo.output_len();
    let frames = fifo.drain_frames_into(dest, channels, width);
    zero_pad_tail(dest, frames * width);
    if held < needed {
        note_refill_or_underrun(runtime, log, underrun, held, needed);
    }
    postprocess_interleaved_output(dest, width, mute, runtime);
}

/// Copy `to_read` samples straight from the ring into `dest`, silence the
/// rest and note the shortfall.
fn play_from_ring(
    ring: &mut RingReader,
    dest: &mut [f32],
    to_read: usize,
    channels: usize,
    mute: bool,
    runtime: &mut AdaptiveRuntimeState,
    log: &mut CallbackLog,
) {
    let count = ring.pop_slice(&mut dest[..to_read]);
    zero_pad_tail(dest, count);
    if count < dest.len() {
        note_refill_or_underrun(
            runtime,
            log,
            "buffer underrun: zero-padding the remainder",
            count,
            dest.len(),
        );
    }
    postprocess_interleaved_output(dest, channels, mute, runtime);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resampler_fifo::new_output_resampler;
    use crate::ring_buffer_io::{RingWriter, sample_ring};
    use rubato::SincFixedIn;

    const CHANNELS: usize = 2;
    const RATE: u32 = 48_000;
    /// 100 ms, interleaved.
    const TARGET: usize = 4_800 * CHANNELS;
    /// A device callback, in frames.
    const FRAMES: usize = 256;

    type Core = OutputCallbackCore<SincFixedIn<f32>>;

    /// No far mode: no startup recovery, no muting, the ring plays as it is.
    fn plain() -> AdaptiveResamplingConfig {
        AdaptiveResamplingConfig {
            enable_far_mode: false,
            ..Default::default()
        }
    }

    struct Rig {
        core: Core,
        ring: RingWriter,
        shared: CallbackShared,
        /// `dest_channels` of the core.
        width: usize,
    }

    fn rig(
        resampled: bool,
        dest_channels: usize,
        config: AdaptiveResamplingConfig,
        pacer: Option<PacerLink>,
    ) -> Rig {
        let (ring, reader) = sample_ring(1 << 20);
        let shared = CallbackShared::new(config.clone());
        let resampler = resampled.then(|| new_output_resampler(1.0, CHANNELS).unwrap());
        let (core, _log) = Core::new(
            CallbackContext {
                channel_count: CHANNELS,
                dest_channels,
                input_sample_rate: RATE,
                output_sample_rate: RATE,
                target_buffer_fill: TARGET,
                servo_deadband_samples: 480,
                adaptive_resampling: resampled,
                pacer,
                input_clock_us: None,
            },
            shared.clone(),
            reader,
            resampler,
            config,
            "test",
        );
        Rig {
            core,
            ring,
            shared,
            width: dest_channels,
        }
    }

    impl Rig {
        fn push_frames(&mut self, frames: usize, value: impl Fn(usize, usize) -> f32) {
            let samples: Vec<f32> = (0..frames * CHANNELS)
                .map(|i| value(i / CHANNELS, i % CHANNELS))
                .collect();
            assert_eq!(self.ring.push_slice(&samples), samples.len());
        }

        /// Top the level the core measures — the ring, plus what the
        /// resampler holds (ratio 1 here) — up to `level` samples, with a
        /// steady signal.
        fn top_up(&mut self, level: usize) {
            let fifo = &self.core.state.resampler.fifo;
            let held = self.ring.fill() + fifo.output_len() + fifo.pending_input_samples();
            let missing = level.saturating_sub(held) / CHANNELS;
            self.push_frames(missing, |_, ch| 0.25 + ch as f32 * 0.25);
        }

        /// One callback of `FRAMES` device frames, prefilled with garbage so
        /// a sample left unwritten shows.
        fn callback(&mut self) -> Vec<f32> {
            let mut dest = vec![9.0; FRAMES * self.width];
            self.core.process(&mut dest, 0.0);
            dest
        }

        /// Hold the ring a callback above its target and run callbacks until
        /// one is audible; how many it took.
        fn run_until_playing(&mut self, target: usize) -> usize {
            for n in 1..=1_000 {
                self.top_up(target + FRAMES * CHANNELS);
                if self.callback().iter().any(|&s| s != 0.0) {
                    return n;
                }
            }
            panic!("still silent after 1000 callbacks");
        }
    }

    #[test]
    fn the_direct_path_plays_the_ring_sample_for_sample() {
        let mut rig = rig(false, CHANNELS, plain(), None);
        rig.push_frames(4 * FRAMES, |frame, ch| (frame * CHANNELS + ch) as f32);
        let mut played = Vec::new();
        for _ in 0..4 {
            played.extend(rig.callback());
        }
        let expected: Vec<f32> = (0..4 * FRAMES * CHANNELS).map(|i| i as f32).collect();
        assert_eq!(played, expected);
        assert_eq!(rig.ring.fill(), 0);
    }

    #[test]
    fn an_underrun_is_padded_with_silence_and_a_partial_frame_too() {
        let mut rig = rig(false, CHANNELS, plain(), None);
        rig.push_frames(100, |_, _| 1.0);
        // One sample more than whole frames: it is not left as it was.
        let mut dest = vec![9.0; FRAMES * CHANNELS + 1];
        rig.core.process(&mut dest, 0.0);
        assert!(dest[..100 * CHANNELS].iter().all(|&s| s == 1.0));
        assert!(dest[100 * CHANNELS..].iter().all(|&s| s == 0.0));
    }

    /// The cpal path: a device frame wider than the audio frame. Each audio
    /// channel lands on its own device channel, and the extra ones are silent.
    #[test]
    fn a_device_wider_than_the_audio_gets_its_extra_channels_silent() {
        const WIDTH: usize = 6;
        let mut rig = rig(true, WIDTH, plain(), None);
        rig.push_frames(16 * FRAMES, |_, ch| if ch == 0 { 0.25 } else { -0.5 });
        let mut last = Vec::new();
        for _ in 0..12 {
            last = rig.callback();
        }
        for frame in last.chunks_exact(WIDTH) {
            assert!((frame[0] - 0.25).abs() < 1e-3, "{frame:?}");
            assert!((frame[1] + 0.5).abs() < 1e-3, "{frame:?}");
            assert!(frame[2..].iter().all(|&s| s == 0.0), "{frame:?}");
        }
    }

    /// With the defaults, a stream starts in a muted refill and only plays
    /// once the ring has held its target long enough, on either path.
    #[test]
    fn the_startup_recovery_holds_the_output_until_the_ring_settles() {
        for (resampled, width) in [(false, CHANNELS), (true, CHANNELS), (true, 6)] {
            let mut rig = rig(resampled, width, AdaptiveResamplingConfig::default(), None);
            // Well short of the target: silent, and nothing is consumed.
            rig.push_frames(FRAMES, |_, _| 0.5);
            assert!(rig.callback().iter().all(|&s| s == 0.0));
            assert_eq!(rig.ring.fill(), FRAMES * CHANNELS);
            assert_eq!(rig.shared.adaptive_runtime_state(), Some("low-recover"));

            // 200 ms of settling at 5.3 ms a callback.
            let callbacks = rig.run_until_playing(TARGET);
            assert!(callbacks > 30, "played after {callbacks} callbacks");
            assert_eq!(rig.shared.adaptive_runtime_state(), Some("stable"));
        }
    }

    #[test]
    fn an_overfull_ring_at_startup_is_trimmed_to_its_target() {
        let mut rig = rig(false, CHANNELS, AdaptiveResamplingConfig::default(), None);
        rig.top_up(3 * TARGET);
        rig.callback();
        // Trimmed to the settle margin above the target (6 ms), then this
        // callback's share consumed while muted.
        let settle_margin = 6 * RATE as usize / 1000 * CHANNELS;
        let fill = rig.ring.fill();
        assert!(fill <= TARGET + settle_margin, "{fill}");
        assert!(fill + FRAMES * CHANNELS >= TARGET, "{fill}");
    }

    #[test]
    fn a_far_overfill_is_dropped_back_to_target_in_silence() {
        let mut rig = rig(false, CHANNELS, AdaptiveResamplingConfig::default(), None);
        rig.run_until_playing(TARGET);
        let discarded = |rig: &Rig| {
            f64::from_bits(
                rig.shared
                    .telemetry
                    .recovery_discard_count_bits
                    .load(Ordering::Relaxed),
            )
        };
        let before = discarded(&rig);
        // Two seconds over: past the 1000 ms far margin.
        rig.top_up(TARGET + 2 * RATE as usize * CHANNELS);
        assert!(rig.callback().iter().all(|&s| s == 0.0));
        assert_eq!(rig.shared.adaptive_runtime_state(), Some("high-recover"));
        let fill = rig.ring.fill();
        assert!(fill.abs_diff(TARGET) <= FRAMES * CHANNELS, "{fill}");
        // Down to the target, from the level less half a callback (the
        // midpoint the control level is taken at).
        let overfill = 2 * RATE as usize * CHANNELS;
        assert!(discarded(&rig) - before >= (overfill - FRAMES * CHANNELS) as f64);
    }

    /// The ring aims for the target less the pacer's share, and a muted
    /// recovery that ends makes the pacer drop what it holds and re-prime.
    #[test]
    fn the_pacer_shares_the_target_and_re_primes_after_a_recovery() {
        let pacer_samples = 1_200 * CHANNELS;
        let pre_roll_complete = Arc::new(AtomicBool::new(true));
        let flush_requested = Arc::new(AtomicBool::new(false));
        let mut rig = rig(
            false,
            CHANNELS,
            AdaptiveResamplingConfig::default(),
            Some(PacerLink {
                pre_roll_complete: Arc::clone(&pre_roll_complete),
                flush_requested: Arc::clone(&flush_requested),
                buffer_samples: pacer_samples,
            }),
        );
        rig.run_until_playing(TARGET - pacer_samples);
        assert!(flush_requested.load(Ordering::Relaxed));
        assert!(!pre_roll_complete.load(Ordering::Relaxed));
    }

    /// A low recover holds the ratio at nominal, so the rate it publishes is
    /// nominal too: a correction left on display would be one no longer
    /// applied.
    #[test]
    fn a_low_recover_publishes_the_servo_at_rest() {
        for (resampled, width) in [(false, CHANNELS), (true, CHANNELS), (true, 6)] {
            let config = AdaptiveResamplingConfig {
                hard_recover_low_in_far_mode: true,
                ..Default::default()
            };
            let mut rig = rig(resampled, width, config, None);
            rig.run_until_playing(TARGET);
            // A correction in force, as the servo leaves it.
            rig.shared
                .rate_adjust
                .store(0.9999f32.to_bits(), Ordering::Relaxed);
            if resampled {
                rig.core.state.resampler.effective_ratio = 1.0001;
            }
            // The ring runs dry, through the low-recover entry.
            for _ in 0..200 {
                rig.callback();
                if rig.shared.adaptive_runtime_state() == Some("low-recover") {
                    break;
                }
            }
            assert_eq!(rig.shared.adaptive_runtime_state(), Some("low-recover"));
            assert_eq!(rig.shared.rate_adjust(), 1.0, "resampled={resampled}");
            assert_eq!(rig.core.state.resampler.effective_ratio, 1.0);
        }
    }

    #[test]
    fn a_ratio_reset_request_returns_the_servo_to_rest() {
        let mut rig = rig(true, CHANNELS, plain(), None);
        rig.shared
            .rate_adjust
            .store(1.004f32.to_bits(), Ordering::Relaxed);
        rig.shared
            .native_rate
            .store(0.99f32.to_bits(), Ordering::Relaxed);
        rig.core.state.resampler.effective_ratio = 1.002;
        rig.shared.request_ratio_reset();
        rig.callback();
        assert!(!rig.shared.reset_ratio_requested.load(Ordering::Relaxed));
        assert_eq!(rig.shared.rate_adjust(), 1.0);
        assert_eq!(
            f32::from_bits(rig.shared.native_rate.load(Ordering::Relaxed)),
            1.0
        );
        assert_eq!(rig.core.state.resampler.effective_ratio, 1.0);
    }

    /// The callback never waits for the config: while the control side holds
    /// it, the callback plays on the copy it has, and picks the new one up at
    /// the first callback after.
    #[test]
    fn a_held_config_lock_does_not_hold_the_callback() {
        let mut rig = rig(false, CHANNELS, plain(), None);
        rig.push_frames(2 * FRAMES, |_, _| 1.0);
        let config = Arc::clone(&rig.shared.live_config);
        {
            let mut held = config.lock();
            held.paused = true;
            assert!(rig.callback().iter().all(|&s| s == 1.0));
            assert!(!rig.core.state.adaptive_config.paused);
        }
        rig.callback();
        assert!(rig.core.state.adaptive_config.paused);
    }
}
