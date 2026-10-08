//! cpal adapter for the [`OutputCore`] (ASIO on Windows, CoreAudio on macOS).
//!
//! cpal 0.15/0.17 hands the callback neither the driver's sample position nor
//! its own latency figures (spike S2 §3–4), so this adapter builds the timing
//! from what it can see:
//!
//! - **time**: the reference clock read at callback entry. Its jitter is the
//!   callback's scheduling jitter; the device DLL's 0.01 Hz bandwidth filters
//!   it (S2: the estimate still holds with 1 ms-quantised timestamps).
//! - **position**: the frames handed to the device so far. An xrun the driver
//!   swallows shifts it, which the DLL sees as a phase step and absorbs.
//! - **heard delay**: `playback − callback` from cpal's timestamp. On both
//!   hosts that is one buffer, not the device's real latency; the remainder is
//!   a constant the latency target absorbs.
//!
//! Forwarding ASIO's `samplePosition`/`systemTime` and CoreAudio's
//! `mSampleTime`/`mHostTime` and latencies needs a small cpal patch; it is a
//! Phase 3 follow-up that has to be verified on Windows and macOS hardware.
//!
//! The stream is opened in the device's own sample format; anything but
//! `f32` is converted from a scratch buffer sized once here. The callback
//! never allocates: a cycle larger than that scratch is played as silence and
//! logged once.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Result, anyhow};
use cpal::traits::{DeviceTrait, StreamTrait};

use super::clock::reference_now_s;
use super::core::{DeviceTiming, OutputCore};

/// Largest device cycle the converting path sizes its scratch for (frames).
pub const MAX_CYCLE_FRAMES: usize = 8192;

/// A running cpal stream. Dropping it stops the stream.
pub struct CpalSyncOutput {
    stream: cpal::Stream,
}

impl CpalSyncOutput {
    /// Open `device` with `config` in `format` and start calling `core`.
    pub fn start(
        device: &cpal::Device,
        config: &cpal::StreamConfig,
        format: cpal::SampleFormat,
        core: OutputCore,
    ) -> Result<Self> {
        let channels = config.channels as usize;
        let rate = config.sample_rate.0 as f64;
        let stream = match format {
            cpal::SampleFormat::F32 => build::<f32>(device, config, core, channels, rate)?,
            cpal::SampleFormat::I32 => build::<i32>(device, config, core, channels, rate)?,
            cpal::SampleFormat::I16 => build::<i16>(device, config, core, channels, rate)?,
            cpal::SampleFormat::U16 => build::<u16>(device, config, core, channels, rate)?,
            cpal::SampleFormat::F64 => build::<f64>(device, config, core, channels, rate)?,
            other => return Err(anyhow!("unsupported output sample format {other:?}")),
        };
        stream.play()?;
        Ok(Self { stream })
    }

    /// Pause the device stream (it resumes on [`resume`](Self::resume)).
    pub fn pause(&self) -> Result<()> {
        self.stream.pause().map_err(Into::into)
    }

    pub fn resume(&self) -> Result<()> {
        self.stream.play().map_err(Into::into)
    }
}

/// What the callback keeps between cycles.
struct Callback {
    core: OutputCore,
    channels: usize,
    rate: f64,
    position: u64,
    oversize_logged: Arc<AtomicBool>,
}

impl Callback {
    fn cycle(&mut self, buf: &mut [f32], info: &cpal::OutputCallbackInfo) {
        let frames = buf.len() / self.channels.max(1);
        let ts = info.timestamp();
        let heard_delay_s = ts
            .playback
            .duration_since(&ts.callback)
            .map(|d| d.as_secs_f64())
            .unwrap_or(frames as f64 / self.rate);
        let timing = DeviceTiming {
            t_s: reference_now_s(),
            position_frames: self.position as f64,
            heard_delay_s,
        };
        self.core.process(timing, buf, self.channels);
        self.position += frames as u64;
    }

    fn silence_oversize(&mut self, frames: usize) {
        if !self.oversize_logged.swap(true, Ordering::Relaxed) {
            log::warn!(
                "output cycle of {frames} frames exceeds the {MAX_CYCLE_FRAMES}-frame scratch; playing silence"
            );
        }
        self.position += frames as u64;
    }
}

fn build<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    core: OutputCore,
    channels: usize,
    rate: f64,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + cpal::FromSample<f32> + 'static,
{
    let mut cb = Callback {
        core,
        channels,
        rate,
        position: 0,
        oversize_logged: Arc::new(AtomicBool::new(false)),
    };
    let err_fn = |err| log::error!("output stream error: {err}");
    let stream = if T::FORMAT == cpal::SampleFormat::F32 {
        device.build_output_stream(
            config,
            move |data: &mut [f32], info: &cpal::OutputCallbackInfo| cb.cycle(data, info),
            err_fn,
            None,
        )?
    } else {
        let mut scratch = vec![0.0f32; MAX_CYCLE_FRAMES * channels.max(1)];
        device.build_output_stream(
            config,
            move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
                if data.len() > scratch.len() {
                    cb.silence_oversize(data.len() / channels.max(1));
                    data.fill(T::from_sample(0.0f32));
                    return;
                }
                let buf = &mut scratch[..data.len()];
                cb.cycle(buf, info);
                for (out, &s) in data.iter_mut().zip(buf.iter()) {
                    *out = T::from_sample(s);
                }
            },
            err_fn,
            None,
        )?
    };
    Ok(stream)
}
