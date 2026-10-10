//! The output callback body, the same for every backend.
//!
//! A backend adapter calls [`OutputCore::process`] once per device cycle with
//! the cycle's timing and the device buffer. The core:
//!
//! 1. asks the [`Servo`] what to do, from the timing, the resampler's exact
//!    position, the ring's fill and the capture side's clock;
//! 2. executes the plan: leading silence, a skip (whose passed-over frames
//!    are discarded from the ring unread), then exactly the planned frames
//!    through the [`DriftResampler`] at the planned ratio;
//! 3. applies the fades the plan asks for and maps the ring's channels onto
//!    the device's;
//! 4. publishes the servo's telemetry.
//!
//! Nothing here allocates, locks or blocks after construction.

use std::sync::Arc;

use audio_rt::{Consumer, Design, DriftResampler};
use audio_sync::{CallbackInput, Servo, ServoConfig};

use super::source_tap::SourceTap;
use super::telemetry::SyncTelemetry;

/// What a backend knows about one device cycle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DeviceTiming {
    /// Start of the cycle on the reference clock (s).
    pub t_s: f64,
    /// Device position at `t_s` (output frames, monotonic).
    pub position_frames: f64,
    /// From `t_s` until the first frame produced now is heard (s).
    pub heard_delay_s: f64,
}

/// Construction parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OutputCoreConfig {
    /// Channels in the ring (the renderer's layout).
    pub channels: usize,
    /// Largest device cycle the backend will ever ask for (frames).
    pub max_frames: usize,
    pub servo: ServoConfig,
    pub design: Design,
}

impl OutputCoreConfig {
    /// Defaults for `channels` at the given nominal rates.
    pub fn new(
        channels: usize,
        source_rate_hz: u32,
        output_rate_hz: u32,
        max_frames: usize,
    ) -> Self {
        let design = Design::for_rates(source_rate_hz, output_rate_hz);
        let mut servo = ServoConfig {
            source_rate_hz: source_rate_hz as f64,
            output_rate_hz: output_rate_hz as f64,
            ..ServoConfig::default()
        };
        servo.resampler_lookahead_frames = (design.taps / 2) as f64;
        Self {
            channels,
            max_frames,
            servo,
            design,
        }
    }
}

/// See the [module docs](self).
pub struct OutputCore {
    servo: Servo,
    resampler: DriftResampler,
    ring: Consumer,
    tap: Arc<SourceTap>,
    telemetry: Arc<SyncTelemetry>,
    channels: usize,
    max_frames: usize,
    /// Resampler output before channel mapping and fades.
    scratch: Vec<f32>,
    fade_in_done: usize,
    fade_in_total: usize,
}

impl OutputCore {
    pub fn new(
        config: OutputCoreConfig,
        ring: Consumer,
        tap: Arc<SourceTap>,
        telemetry: Arc<SyncTelemetry>,
    ) -> Self {
        let channels = config.channels.max(1);
        let max_frames = config.max_frames.max(1);
        let nominal = config.servo.source_rate_hz / config.servo.output_rate_hz;
        // Room for the largest ratio the servo can ask for.
        let max_step = nominal
            * (1.0 + config.servo.max_feedforward_deviation)
            * (1.0 + config.servo.max_correction)
            * 1.001;
        let mut resampler = DriftResampler::new(channels, &config.design, max_frames, max_step);
        resampler.reset(ring.read() as i64);
        resampler.set_ratio(nominal);
        Self {
            servo: Servo::new(config.servo),
            resampler,
            ring,
            tap,
            telemetry,
            channels,
            max_frames,
            scratch: vec![0.0; max_frames * channels],
            fade_in_done: 0,
            fade_in_total: 0,
        }
    }

    pub fn servo(&self) -> &Servo {
        &self.servo
    }

    /// Fill `out` (interleaved, `device_channels` wide) for one cycle and
    /// return the frames written. Device channels beyond the ring's are
    /// zeroed; ring channels beyond the device's are dropped.
    pub fn process(
        &mut self,
        timing: DeviceTiming,
        out: &mut [f32],
        device_channels: usize,
    ) -> usize {
        let dch = device_channels.max(1);
        let frames = (out.len() / dch).min(self.max_frames);
        let out = &mut out[..frames * dch];
        let input = CallbackInput {
            t: timing.t_s,
            device_position: timing.position_frames,
            frames,
            heard_delay_s: timing.heard_delay_s,
            play_position: self.resampler.position().as_f64(),
            available: self.ring.written() as f64,
            source: self.tap.latest(),
            source_offset: self.tap.offset(),
            source_breaks: self.tap.breaks(),
        };
        let plan = self.servo.plan(&input);
        let silence = plan.silence_frames.min(frames);
        let played = frames - silence;
        out[..silence * dch].fill(0.0);

        if played > 0 && !self.render(plan.skip_source_frames, plan.ratio, played) {
            out.fill(0.0);
            self.telemetry
                .publish(self.servo.phase(), self.servo.telemetry());
            return frames;
        }
        if played > 0 {
            if plan.fade_in_frames > 0 {
                self.fade_in_done = 0;
                self.fade_in_total = plan.fade_in_frames;
            }
            self.apply_fades(played, plan.fade_out);
            map_channels(
                &self.scratch[..played * self.channels],
                self.channels,
                &mut out[silence * dch..],
                dch,
            );
        }
        self.telemetry
            .publish(self.servo.phase(), self.servo.telemetry());
        frames
    }

    /// Skip, read and resample `played` frames into `scratch`. On a short
    /// ring (the servo plans within what is there, so this means the producer
    /// misbehaved) the resampler restarts at the ring's read front, which the
    /// servo then sees as a latency jump and realigns.
    fn render(&mut self, skip: f64, ratio: f64, played: usize) -> bool {
        if skip > 0.0 {
            let discard = self.resampler.skip(skip);
            if self.ring.discard(discard) < discard {
                return self.short_read();
            }
        }
        self.resampler.prepare(played, ratio);
        if !self.ring.read_exact(self.resampler.input_slot()) {
            return self.short_read();
        }
        self.resampler
            .render(&mut self.scratch[..played * self.channels]);
        true
    }

    fn short_read(&mut self) -> bool {
        self.telemetry.note_short_read();
        self.resampler.reset(self.ring.read() as i64);
        false
    }

    fn apply_fades(&mut self, played: usize, fade_out: bool) {
        let c = self.channels;
        let scratch = &mut self.scratch[..played * c];
        if self.fade_in_done < self.fade_in_total {
            let total = self.fade_in_total as f32;
            for frame in scratch.chunks_exact_mut(c) {
                if self.fade_in_done >= self.fade_in_total {
                    break;
                }
                let g = self.fade_in_done as f32 / total;
                frame.iter_mut().for_each(|s| *s *= g);
                self.fade_in_done += 1;
            }
        }
        if fade_out {
            let n = played as f32;
            for (i, frame) in scratch.chunks_exact_mut(c).enumerate() {
                let g = 1.0 - (i + 1) as f32 / n;
                frame.iter_mut().for_each(|s| *s *= g);
            }
        }
    }
}

/// Copy whole frames from a `src_ch`-wide buffer to a `dst_ch`-wide one.
fn map_channels(src: &[f32], src_ch: usize, dst: &mut [f32], dst_ch: usize) {
    let shared = src_ch.min(dst_ch);
    for (s, d) in src.chunks_exact(src_ch).zip(dst.chunks_exact_mut(dst_ch)) {
        d[..shared].copy_from_slice(&s[..shared]);
        d[shared..].fill(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channels_map_onto_wider_and_narrower_devices() {
        let src = [1.0, 2.0, 3.0, 4.0];
        let mut wide = [9.0; 6];
        map_channels(&src, 2, &mut wide, 3);
        assert_eq!(wide, [1.0, 2.0, 0.0, 3.0, 4.0, 0.0]);
        let mut narrow = [9.0; 2];
        map_channels(&src, 2, &mut narrow, 1);
        assert_eq!(narrow, [1.0, 3.0]);
    }
}
