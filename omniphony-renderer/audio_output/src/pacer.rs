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
//!
//! A handle exists only for an output built with pacing on
//! (`AdaptiveResamplingConfig::use_output_pacing`). Pacing decides *which
//! thread produces into the ring* — the drain when on, the renderer when off
//! — and the ring has a single producer, so it is fixed for the lifetime of
//! the audio output: the ring's writing end is handed either to the writer
//! or to this handle when the output is built, and changing the setting
//! takes effect at the next output start.

use crate::ring_buffer_io::{RingReader, RingWriter};
use parking_lot::Mutex;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The drain's ends of the two rings it moves samples between. Both rings are
/// single-producer single-consumer, so these are the only reading end of the
/// FIFO and the only writing end of the ring.
pub struct PacerDrainEnds {
    /// Producer: renderer thread (via `PipewireWriter::write_samples`).
    /// Consumer: the drain.
    pub fifo: RingReader,
    /// The audio ring the DAC callback consumes. Drain target.
    pub ring: RingWriter,
}

/// Shared state allowing the PipeWire input thread to drain rendered samples
/// from the output-side pacer FIFO into the ring buffer in lockstep with
/// the IEC958 input chunk arrival cadence.
#[derive(Clone)]
pub struct PacerHandle {
    /// The drain's ends of the pacer FIFO and of the ring, for one drain at a
    /// time.
    ///
    /// [`drain`](Self::drain) has two callers on two threads — the PipeWire
    /// input callback, and the pipe-bridge drain thread — which are meant to
    /// alternate with the input mode and are not kept apart by anything else.
    /// So the ends are taken with `try_lock`: a drain that finds another one
    /// in progress moves nothing and returns, and neither waits. Nothing
    /// takes this lock blocking, and the device callback never touches it.
    pub(crate) ends: Arc<Mutex<PacerDrainEnds>>,
    /// `false` until the pacer FIFO has accumulated at least
    /// `pre_roll_threshold_samples` — until then the input-thread drain
    /// pushes silence into the ring instead of popping from the FIFO,
    /// guaranteeing the FIFO is never drained below the minimum safety
    /// margin once "real" draining begins.
    pub pre_roll_complete: Arc<AtomicBool>,
    /// Samples (across all channels) that must accumulate in the pacer FIFO
    /// before the drain switches from silence to real audio. Sized to
    /// exceed one full decoded AU (~32 ms at 48 kHz × 8 ch ≈ 12 288 samples)
    /// with comfortable margin.
    pub pre_roll_threshold_samples: usize,
    /// Output sample rate (Hz) used to compute the per-chunk drain quantum.
    pub out_sample_rate: u32,
    /// Output channel count used to compute the per-chunk drain quantum.
    pub out_channels: u32,
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
    /// `drain` is the FIFO's only consumer and has to stay that way: the FIFO
    /// is a single-consumer ring. It used to be popped from three threads —
    /// the drain, the OSC thread on a pacing toggle, and the DAC callback on
    /// recovery — which left it no single owner, and put an unbounded
    /// `while pop()` inside the audio callback.
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
    /// zero-fills on underrun. Both rings move whole frames only, so the
    /// quantum is rounded down to one, and what a full ring refuses is
    /// dropped from a frame boundary: the ring's channels never shift.
    ///
    /// Both the PipeWire input RT callback (Pipewire mode) and the pure
    /// pipe-bridge drain thread share this single drain implementation; the
    /// difference is only how each computes `drain_samples` and what clock
    /// drives the call.
    ///
    /// Returns `false`, having moved nothing, when another drain is in
    /// progress on another thread. It never waits: no lock is taken blocking,
    /// and the transfer is a few block copies whatever `drain_samples` is.
    pub fn drain(&self, drain_samples: usize) -> bool {
        let Some(mut ends) = self.ends.try_lock() else {
            return false;
        };
        let PacerDrainEnds { fifo, ring } = &mut *ends;
        let drain_samples = drain_samples - drain_samples % ring.frame_len();
        // Honour a deferred flush first, so no stale sample reaches the ring.
        // Done here because this is the FIFO's only consumer — see
        // [`request_flush_and_rearm`](Self::request_flush_and_rearm).
        if self.flush_requested.swap(false, Ordering::Acquire) {
            fifo.discard(usize::MAX);
            self.pre_roll_complete.store(false, Ordering::Relaxed);
        }
        let mut primed = self.pre_roll_complete.load(Ordering::Relaxed);
        if !primed && fifo.available() >= self.pre_roll_threshold_samples {
            self.pre_roll_complete.store(true, Ordering::Relaxed);
            primed = true;
        }
        // While priming, nothing is drawn from the FIFO and the whole quantum
        // is silence; once primed, silence only makes up for what the FIFO
        // does not hold.
        let from_fifo = if primed {
            drain_samples.min(fifo.available())
        } else {
            0
        };
        // The FIFO is drawn down by the clock whatever the ring takes: what a
        // full ring refuses is dropped, from the first frame it refuses.
        let moved = ring.transfer_from(fifo, from_fifo);
        let ring_full = moved < from_fifo;
        if ring_full {
            fifo.discard(from_fifo - moved);
        }
        let underruns = drain_samples - from_fifo;
        if underruns > 0 && !ring_full {
            ring.push_silence(underruns);
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
            .store((fifo.available() as f64).to_bits(), Ordering::Relaxed);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring_buffer_io::sample_ring;

    const CHANNELS: u32 = 2;

    /// A pacer with its handle, the renderer's end of the FIFO and the device
    /// callback's end of the ring.
    struct Pacer {
        handle: PacerHandle,
        fifo: RingWriter,
        ring: RingReader,
    }

    fn pacer(pre_roll: usize) -> Pacer {
        pacer_with_ring_capacity(pre_roll, 4096)
    }

    /// `ring_capacity` in samples, a whole number of frames.
    fn pacer_with_ring_capacity(pre_roll: usize, ring_capacity: usize) -> Pacer {
        let channels = CHANNELS as usize;
        let (fifo_writer, fifo_reader) = sample_ring(4096 / channels, channels);
        let (ring_writer, ring_reader) = sample_ring(ring_capacity / channels, channels);
        let handle = PacerHandle {
            ends: Arc::new(Mutex::new(PacerDrainEnds {
                fifo: fifo_reader,
                ring: ring_writer,
            })),
            pre_roll_complete: Arc::new(AtomicBool::new(true)),
            pre_roll_threshold_samples: pre_roll,
            out_sample_rate: 48_000,
            out_channels: CHANNELS,
            diag_drain_total: Arc::new(AtomicU64::new(0)),
            diag_underrun_total: Arc::new(AtomicU64::new(0)),
            diag_fifo_level: Arc::new(AtomicU64::new(0)),
            flush_requested: Arc::new(AtomicBool::new(false)),
        };
        Pacer {
            handle,
            fifo: fifo_writer,
            ring: ring_reader,
        }
    }

    fn fill(p: &mut Pacer, values: &[f32]) {
        assert_eq!(p.fifo.push_slice(values), values.len(), "fifo has room");
    }

    fn drain_ring(p: &mut Pacer) -> Vec<f32> {
        let mut out = vec![0.0; p.ring.available()];
        let count = p.ring.pop_slice(&mut out);
        out.truncate(count);
        out
    }

    fn diag(counter: &AtomicU64) -> f64 {
        f64::from_bits(counter.load(Ordering::Relaxed))
    }

    /// The request only records the intent: the FIFO is the drain's to empty,
    /// and the requester is a different thread.
    #[test]
    fn requesting_a_flush_does_not_touch_the_fifo() {
        let mut p = pacer(0);
        fill(&mut p, &[1.0, 2.0, 3.0, 4.0]);
        p.handle.request_flush_and_rearm();
        assert_eq!(p.fifo.fill(), 4, "the requester must not consume");
        assert!(p.handle.flush_requested.load(Ordering::Relaxed));
        assert!(
            !p.handle.pre_roll_complete.load(Ordering::Relaxed),
            "pre-roll re-armed"
        );
    }

    /// The deferred flush happens at the next drain, and nothing queued before
    /// it reaches the ring — which is the whole point of flushing.
    #[test]
    fn the_next_drain_flushes_before_transferring() {
        let mut p = pacer(0);
        fill(&mut p, &[1.0, 2.0, 3.0, 4.0]);
        p.handle.request_flush_and_rearm();

        assert!(p.handle.drain(4));
        assert_eq!(p.fifo.fill(), 0, "the drain emptied the FIFO");
        assert!(
            !p.handle.flush_requested.load(Ordering::Relaxed),
            "request consumed"
        );
        assert!(
            drain_ring(&mut p).iter().all(|s| *s == 0.0),
            "stale samples must not reach the ring"
        );

        // Fresh audio queued after the flush goes through normally.
        p.handle.pre_roll_complete.store(true, Ordering::Relaxed);
        fill(&mut p, &[0.5, -0.5]);
        p.handle.drain(2);
        assert_eq!(drain_ring(&mut p), vec![0.5, -0.5]);
    }

    /// A second drain must not re-flush: the request is consumed once.
    #[test]
    fn the_flush_request_is_consumed_once() {
        let mut p = pacer(0);
        p.handle.request_flush_and_rearm();
        p.handle.drain(0);
        p.handle.pre_roll_complete.store(true, Ordering::Relaxed);
        fill(&mut p, &[7.0, 8.0]);
        p.handle.drain(2);
        assert_eq!(
            drain_ring(&mut p),
            vec![7.0, 8.0],
            "a stale request would have eaten these"
        );
    }

    /// Pre-roll still gates real audio: until the FIFO is primed the drain
    /// emits silence rather than draining the FIFO below its safety margin.
    #[test]
    fn pre_roll_still_holds_back_real_audio() {
        let mut p = pacer(8);
        p.handle.pre_roll_complete.store(false, Ordering::Relaxed);
        fill(&mut p, &[1.0, 2.0]);
        p.handle.drain(2);
        assert_eq!(drain_ring(&mut p), vec![0.0, 0.0], "silence while priming");
        assert_eq!(p.fifo.fill(), 2, "the FIFO was not drawn down");
        assert_eq!(diag(&p.handle.diag_underrun_total), 2.0);

        // Primed: the same drain call moves the audio.
        fill(&mut p, &[3.0, 4.0, 5.0, 6.0, 7.0, 8.0]);
        p.handle.drain(4);
        assert!(p.handle.pre_roll_complete.load(Ordering::Relaxed));
        assert_eq!(drain_ring(&mut p), vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(diag(&p.handle.diag_fifo_level), 4.0);
    }

    /// A FIFO that holds less than the quantum: what it holds, then silence,
    /// so the ring still receives a whole quantum.
    #[test]
    fn an_underrun_is_made_up_with_silence() {
        let mut p = pacer(0);
        fill(&mut p, &[1.0, 2.0]);
        p.handle.drain(6);
        assert_eq!(drain_ring(&mut p), vec![1.0, 2.0, 0.0, 0.0, 0.0, 0.0]);
        assert_eq!(diag(&p.handle.diag_drain_total), 6.0);
        assert_eq!(diag(&p.handle.diag_underrun_total), 4.0);
        assert_eq!(diag(&p.handle.diag_fifo_level), 0.0);
    }

    /// The transfer follows the samples around the end of both rings.
    #[test]
    fn a_transfer_wraps_around_the_end_of_both_rings() {
        let mut p = pacer(0);
        let block: Vec<f32> = (0..3000).map(|i| i as f32).collect();
        for round in 0..5 {
            fill(&mut p, &block);
            p.handle.drain(3000);
            assert_eq!(drain_ring(&mut p), block, "round {round}");
        }
        assert_eq!(diag(&p.handle.diag_underrun_total), 0.0);
    }

    /// The FIFO is drawn down by the clock even when the ring is full: what
    /// the ring cannot take is dropped, not kept for later.
    #[test]
    fn a_full_ring_drops_what_it_cannot_take() {
        let mut p = pacer_with_ring_capacity(0, 4);
        fill(&mut p, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        p.handle.drain(6);
        assert_eq!(p.fifo.fill(), 0, "all six left the FIFO");
        assert_eq!(drain_ring(&mut p), vec![1.0, 2.0, 3.0, 4.0]);
    }

    /// The drain has two callers on two threads. One that arrives while the
    /// other is in the middle of a transfer returns at once, and moves
    /// nothing: the ring keeps a single producer and the FIFO a single
    /// consumer, and neither thread waits for the other.
    #[test]
    fn a_drain_does_not_wait_for_another_one_in_progress() {
        let mut p = pacer(0);
        fill(&mut p, &[1.0, 2.0, 3.0, 4.0]);

        // Another thread's drain, held where a preempted thread can be.
        let in_progress = p.handle.ends.try_lock().unwrap();
        assert!(!p.handle.clone().drain(2), "refused, not queued behind it");
        assert_eq!(diag(&p.handle.diag_drain_total), 0.0);
        drop(in_progress);
        assert_eq!(p.fifo.fill(), 4, "nothing moved");
        assert_eq!(drain_ring(&mut p), Vec::<f32>::new());

        // It finishes: the next drain goes through.
        assert!(p.handle.drain(2));
        assert_eq!(drain_ring(&mut p), vec![1.0, 2.0]);
    }

    /// A quantum that falls inside a frame is rounded down to one: half a
    /// frame would leave every later sample a channel off.
    #[test]
    fn a_quantum_inside_a_frame_moves_whole_frames() {
        let mut p = pacer(0);
        fill(&mut p, &[1.0, 2.0, 3.0, 4.0]);
        p.handle.drain(3);
        assert_eq!(drain_ring(&mut p), vec![1.0, 2.0]);
        assert_eq!(diag(&p.handle.diag_drain_total), 2.0);
        p.handle.drain(2);
        assert_eq!(drain_ring(&mut p), vec![3.0, 4.0]);
    }

    /// What a full ring refuses goes from a frame boundary, and the next
    /// drain starts on the first channel.
    #[test]
    fn a_full_ring_keeps_the_channels_aligned() {
        let mut p = pacer_with_ring_capacity(0, 6);
        // Left and right tagged 1 and 2, frame by frame.
        let frames = |n: usize| -> Vec<f32> { (0..n).flat_map(|_| [1.0, 2.0]).collect() };
        fill(&mut p, &frames(2));
        p.handle.drain(4);
        fill(&mut p, &frames(3));
        p.handle.drain(6);
        assert_eq!(p.fifo.fill(), 0, "the refused frames left the FIFO");
        assert_eq!(drain_ring(&mut p), frames(3), "one frame of room");
        fill(&mut p, &frames(2));
        p.handle.drain(4);
        assert_eq!(drain_ring(&mut p), frames(2));
    }
}
