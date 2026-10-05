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
//! The drain has two clocks, on two threads: the capture stream, and the
//! token clock that stands in for it when no capture stream delivers. Which
//! of the two drains at a given time is the input side's to say (it is the
//! one that knows whether a capture stream is delivering): see
//! `audio_input::pacer_drain`. This module only makes sure a drain is never
//! joined by another one ([`PacerHandle::drain`]), and lets a clock check
//! that the drain is still its own once nobody else can be in it
//! ([`PacerHandle::drain_if`]).
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
    /// input callback, and the token-clock drain thread — which take turns
    /// (`audio_input::pacer_drain` says whose turn it is). At a handover one
    /// of them can still be in the middle of a transfer, so the ends are
    /// taken with `try_lock`: a drain that finds another one in progress
    /// moves nothing and returns, and neither waits. Nothing takes this lock
    /// blocking, and the device callback never touches it.
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
    /// A pacer over `ends`, not primed yet, with counters of its own.
    pub fn new(
        ends: PacerDrainEnds,
        pre_roll_threshold_samples: usize,
        out_sample_rate: u32,
        out_channels: u32,
    ) -> Self {
        Self {
            ends: Arc::new(Mutex::new(ends)),
            pre_roll_complete: Arc::new(AtomicBool::new(false)),
            pre_roll_threshold_samples,
            out_sample_rate,
            out_channels,
            diag_drain_total: Arc::new(AtomicU64::new(0)),
            diag_underrun_total: Arc::new(AtomicU64::new(0)),
            diag_fifo_level: Arc::new(AtomicU64::new(0)),
            flush_requested: Arc::new(AtomicBool::new(false)),
        }
    }

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
    /// Both the PipeWire input callback and the token-clock drain thread
    /// share this single drain implementation; the difference is only how
    /// each computes `drain_samples` and what clock drives the call.
    ///
    /// Returns `false`, having moved nothing, when another drain is in
    /// progress on another thread. It never waits: no lock is taken blocking,
    /// and the transfer is a few block copies whatever `drain_samples` is.
    pub fn drain(&self, drain_samples: usize) -> bool {
        self.drain_if(drain_samples, || true)
    }

    /// [`drain`](Self::drain), for a clock that may have lost the drain to
    /// the other one since it last looked.
    ///
    /// `still_mine` is asked once the drain's ends are taken, that is once no
    /// other drain can be in progress: a clock that lost the drain before the
    /// other one's transfer went through is refused here, however far it had
    /// got before taking the ends. Returns `false`, having moved nothing and
    /// counted nothing, when `still_mine` says no.
    ///
    /// `still_mine` runs with the ends held and must not wait: an atomic
    /// load, not a lock.
    pub fn drain_if(&self, drain_samples: usize, still_mine: impl FnOnce() -> bool) -> bool {
        let Some(mut ends) = self.ends.try_lock() else {
            return false;
        };
        if !still_mine() {
            return false;
        }
        let PacerDrainEnds { fifo, ring } = &mut *ends;
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
        // full ring refuses is dropped, from the first sample it refuses on.
        let mut ring_full = false;
        fifo.pop_with(from_fifo, |block| {
            if !ring_full {
                ring_full = ring.push_slice(block) < block.len();
            }
        });
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

    fn pacer_with_ring_capacity(pre_roll: usize, ring_capacity: usize) -> Pacer {
        let (fifo_writer, fifo_reader) = sample_ring(4096);
        let (ring_writer, ring_reader) = sample_ring(ring_capacity);
        let handle = PacerHandle::new(
            PacerDrainEnds {
                fifo: fifo_reader,
                ring: ring_writer,
            },
            pre_roll,
            48_000,
            CHANNELS,
        );
        handle.pre_roll_complete.store(true, Ordering::Relaxed);
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

    /// A clock that no longer has the drain moves nothing and counts nothing,
    /// flush request included: all of it is the other clock's now.
    #[test]
    fn a_clock_that_lost_the_drain_moves_nothing() {
        let mut p = pacer(0);
        fill(&mut p, &[1.0, 2.0, 3.0, 4.0]);
        p.handle.flush_requested.store(true, Ordering::Relaxed);

        assert!(!p.handle.drain_if(2, || false));
        assert_eq!(p.fifo.fill(), 4, "nothing moved, nothing flushed");
        assert_eq!(drain_ring(&mut p), Vec::<f32>::new());
        assert_eq!(diag(&p.handle.diag_drain_total), 0.0);
        assert_eq!(diag(&p.handle.diag_underrun_total), 0.0);
        assert!(p.handle.flush_requested.load(Ordering::Relaxed));

        // The clock that has it drains as usual.
        p.handle.flush_requested.store(false, Ordering::Relaxed);
        assert!(p.handle.drain_if(2, || true));
        assert_eq!(drain_ring(&mut p), vec![1.0, 2.0]);
    }

    /// The question is put with the ends held: whoever answers it knows that
    /// no other drain is in progress, and none can start before this one is
    /// done.
    #[test]
    fn the_clock_is_asked_once_no_other_drain_can_be_in_progress() {
        let mut p = pacer(0);
        fill(&mut p, &[1.0, 2.0]);
        let other = p.handle.clone();
        let mut asked = false;
        assert!(p.handle.drain_if(2, || {
            asked = true;
            assert!(!other.drain(2), "the ends are held while it answers");
            true
        }));
        assert!(asked);
        assert_eq!(drain_ring(&mut p), vec![1.0, 2.0]);

        // Not asked at all behind a drain in progress.
        let in_progress = p.handle.ends.try_lock().unwrap();
        assert!(
            !other.drain_if(2, || panic!("asked without the ends")),
            "refused before the question"
        );
        drop(in_progress);
    }
}
