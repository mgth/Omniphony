//! Cross-crate shared handle for the post-rendering output pacer.
//!
//! The output side (this crate) owns the FIFOs and atomics. The input side
//! (`audio_input` crate, PipeWire input process callback) reads this handle
//! to drain pacer samples into the ring buffer at a cadence that exactly
//! matches the rate at which IEC958 chunks arrive — i.e. the source clock.
//! This breaks the decoder-batching burst pattern out of the ring buffer
//! signal that the PI servo consumes, without filtering anything downstream.
//!
//! Why a struct rather than passing the Arcs individually: the handle is
//! threaded through `InputControl::install_output_pacer`, which is called
//! by the decode lifecycle wiring once both the input PwStream and the
//! output PipewireWriter exist. Passing one struct keeps the wiring stable
//! as fields evolve.

use crossbeam::queue::ArrayQueue;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Shared state allowing the PipeWire input thread to drain rendered samples
/// from the output-side pacer FIFO into the ring buffer in lockstep with
/// the IEC958 input chunk arrival cadence.
#[derive(Clone)]
pub struct PacerHandle {
    /// Producer: renderer thread (via `PipewireWriter::write_samples`).
    /// Consumer: PipeWire input thread (drain step).
    pub pacer_fifo: Arc<ArrayQueue<f32>>,
    /// The audio ring the DAC callback consumes. Drain target.
    pub ring: Arc<ArrayQueue<f32>>,
    /// `false` until `pacer_fifo` has accumulated at least
    /// `pre_roll_threshold_samples` — until then the input-thread drain
    /// pushes silence into the ring instead of popping from the FIFO,
    /// guaranteeing the FIFO is never drained below the minimum safety
    /// margin once "real" draining begins.
    pub pre_roll_complete: Arc<AtomicBool>,
    /// Samples (across all channels) that must accumulate in `pacer_fifo`
    /// before the drain switches from silence to real audio. Sized to
    /// exceed one full decoded AU (~32 ms at 48 kHz × 8 ch ≈ 12 288 samples)
    /// with comfortable margin.
    pub pre_roll_threshold_samples: usize,
    /// Output sample rate (Hz) used to compute the per-chunk drain quantum.
    pub out_sample_rate: u32,
    /// Output channel count used to compute the per-chunk drain quantum.
    pub out_channels: u32,
    /// Whether pacing is in effect, from `AdaptiveResamplingConfig::
    /// use_output_pacing`. When `false`, the drain is a no-op and the writer
    /// pushes straight to `ring`.
    ///
    /// Fixed for the lifetime of the audio output, and a plain `bool` so that
    /// it cannot be otherwise. It decides *which thread produces into `ring`* —
    /// the drain when set, the renderer when not — and swapping producers under
    /// a running stream is exactly what a single-producer ring forbids. Changing
    /// it takes effect at the next output start, like the other settings that
    /// shape the audio path rather than tune it.
    pub enabled: bool,
    /// Diagnostic: cumulative number of samples drawn from the FIFO (real
    /// audio + zero fills). f64-encoded so the diag plot can read it like
    /// any other atomic metric.
    pub diag_drain_total: Arc<AtomicU64>,
    /// Diagnostic: cumulative number of zero-fill samples emitted because
    /// the FIFO underran. f64-encoded. If non-zero in steady state,
    /// `pre_roll_threshold_samples` is too low.
    pub diag_underrun_total: Arc<AtomicU64>,
    /// Diagnostic: instantaneous FIFO occupancy (in samples, f64-encoded).
    /// Should oscillate around `pre_roll_threshold_samples` in steady state
    /// — bottoms out near zero just before each decoder AU lands.
    pub diag_fifo_level: Arc<AtomicU64>,
    /// A flush has been asked for; [`drain`](Self::drain) empties the FIFO
    /// before its next transfer.
    ///
    /// The flush is deferred rather than done where it is requested because
    /// `drain` is the FIFO's only consumer and needs to stay that way. It used
    /// to be popped from three threads — the drain, the OSC thread on a pacing
    /// toggle, and the DAC callback on recovery — which the queue tolerates but
    /// which leaves no single owner, and put an unbounded `while pop()` inside
    /// the audio callback.
    pub flush_requested: Arc<AtomicBool>,
}

impl PacerHandle {
    /// Ask for the FIFO to be flushed and the pre-roll re-armed.
    ///
    /// Called on `recovery_reacquire_pending` consumption or codec switch from
    /// the DAC callback (output side), and on a pacing toggle from the OSC
    /// thread. The work happens on the next [`drain`](Self::drain); until then
    /// the stale samples sit in the FIFO, where nothing else reads them.
    ///
    /// Deferring is also what the caller wants on a toggle: flushing at the
    /// moment pacing is switched off leaves the FIFO to refill from the
    /// renderer before draining resumes, whereas flushing at the first drain
    /// guarantees nothing stale reaches the ring.
    pub fn request_flush_and_rearm(&self) {
        self.flush_requested
            .store(true, std::sync::atomic::Ordering::Release);
        self.pre_roll_complete
            .store(false, std::sync::atomic::Ordering::Relaxed);
    }

    /// Move `drain_samples` (across all channels) from the pacer FIFO into the
    /// ring. Honours pre-roll (pushes silence until the FIFO is primed) and
    /// zero-fills on underrun. `drain_samples` should be a whole number of
    /// frames (i.e. a multiple of the output channel count) so the ring's
    /// channel interleaving stays aligned.
    ///
    /// Callers gate on [`PacerHandle::enabled`] before invoking this. Both the
    /// PipeWire input RT callback (Pipewire mode) and the pure
    /// pipe-bridge drain thread share this single drain implementation; the
    /// difference is only how each computes `drain_samples` and what clock
    /// drives the call.
    pub fn drain(&self, drain_samples: usize) {
        let mut underruns = 0u64;
        // Honour a deferred flush first, so no stale sample reaches the ring.
        // Done here because this is the FIFO's only consumer — see
        // [`request_flush_and_rearm`](Self::request_flush_and_rearm).
        if self.flush_requested.swap(false, Ordering::Acquire) {
            while self.pacer_fifo.pop().is_some() {}
            self.pre_roll_complete.store(false, Ordering::Relaxed);
        }
        let priming = !self.pre_roll_complete.load(Ordering::Relaxed);
        if priming {
            if self.pacer_fifo.len() >= self.pre_roll_threshold_samples {
                self.pre_roll_complete.store(true, Ordering::Relaxed);
            } else {
                for _ in 0..drain_samples {
                    let _ = self.ring.push(0.0);
                }
                underruns += drain_samples as u64;
            }
        }
        if !priming || self.pre_roll_complete.load(Ordering::Relaxed) {
            for _ in 0..drain_samples {
                let value = self.pacer_fifo.pop().unwrap_or_else(|| {
                    underruns += 1;
                    0.0
                });
                let _ = self.ring.push(value);
            }
        }
        let prev_drain = f64::from_bits(self.diag_drain_total.load(Ordering::Relaxed));
        self.diag_drain_total.store(
            (prev_drain + drain_samples as f64).to_bits(),
            Ordering::Relaxed,
        );
        if underruns > 0 {
            let prev_under = f64::from_bits(self.diag_underrun_total.load(Ordering::Relaxed));
            self.diag_underrun_total
                .store((prev_under + underruns as f64).to_bits(), Ordering::Relaxed);
        }
        self.diag_fifo_level
            .store((self.pacer_fifo.len() as f64).to_bits(), Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNELS: u32 = 2;

    fn handle(pre_roll: usize) -> PacerHandle {
        PacerHandle {
            pacer_fifo: Arc::new(ArrayQueue::new(4096)),
            ring: Arc::new(ArrayQueue::new(4096)),
            pre_roll_complete: Arc::new(AtomicBool::new(true)),
            pre_roll_threshold_samples: pre_roll,
            out_sample_rate: 48_000,
            out_channels: CHANNELS,
            enabled: true,
            diag_drain_total: Arc::new(AtomicU64::new(0)),
            diag_underrun_total: Arc::new(AtomicU64::new(0)),
            diag_fifo_level: Arc::new(AtomicU64::new(0)),
            flush_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    fn fill(h: &PacerHandle, values: &[f32]) {
        for &v in values {
            h.pacer_fifo.push(v).expect("fifo has room");
        }
    }

    fn drain_ring(h: &PacerHandle) -> Vec<f32> {
        std::iter::from_fn(|| h.ring.pop()).collect()
    }

    /// The request only records the intent: the FIFO is the drain's to empty,
    /// and the requester is a different thread.
    #[test]
    fn requesting_a_flush_does_not_touch_the_fifo() {
        let h = handle(0);
        fill(&h, &[1.0, 2.0, 3.0, 4.0]);
        h.request_flush_and_rearm();
        assert_eq!(h.pacer_fifo.len(), 4, "the requester must not consume");
        assert!(h.flush_requested.load(Ordering::Relaxed));
        assert!(
            !h.pre_roll_complete.load(Ordering::Relaxed),
            "pre-roll re-armed"
        );
    }

    /// The deferred flush happens at the next drain, and nothing queued before
    /// it reaches the ring — which is the whole point of flushing.
    #[test]
    fn the_next_drain_flushes_before_transferring() {
        let h = handle(0);
        fill(&h, &[1.0, 2.0, 3.0, 4.0]);
        h.request_flush_and_rearm();

        h.drain(4);
        assert_eq!(h.pacer_fifo.len(), 0, "the drain emptied the FIFO");
        assert!(
            !h.flush_requested.load(Ordering::Relaxed),
            "request consumed"
        );
        assert!(
            drain_ring(&h).iter().all(|s| *s == 0.0),
            "stale samples must not reach the ring"
        );

        // Fresh audio queued after the flush goes through normally.
        h.pre_roll_complete.store(true, Ordering::Relaxed);
        fill(&h, &[0.5, -0.5]);
        h.drain(2);
        assert_eq!(drain_ring(&h), vec![0.5, -0.5]);
    }

    /// A second drain must not re-flush: the request is consumed once.
    #[test]
    fn the_flush_request_is_consumed_once() {
        let h = handle(0);
        h.request_flush_and_rearm();
        h.drain(0);
        h.pre_roll_complete.store(true, Ordering::Relaxed);
        fill(&h, &[7.0, 8.0]);
        h.drain(2);
        assert_eq!(
            drain_ring(&h),
            vec![7.0, 8.0],
            "a stale request would have eaten these"
        );
    }

    /// Pre-roll still gates real audio: until the FIFO is primed the drain
    /// emits silence rather than draining the FIFO below its safety margin.
    #[test]
    fn pre_roll_still_holds_back_real_audio() {
        let h = handle(8);
        h.pre_roll_complete.store(false, Ordering::Relaxed);
        fill(&h, &[1.0, 2.0]);
        h.drain(2);
        assert_eq!(drain_ring(&h), vec![0.0, 0.0], "silence while priming");
        assert_eq!(h.pacer_fifo.len(), 2, "the FIFO was not drawn down");
    }
}
