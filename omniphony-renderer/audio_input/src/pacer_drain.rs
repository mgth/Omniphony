//! Which clock drains the output pacer.
//!
//! With the post-rendering pacer on (`audio_output::pacer`), rendered samples
//! wait in a FIFO until a drain moves them to the ring the device reads. The
//! drain is a clock: it is called once per stretch of source time, with the
//! amount of output that stretch stands for. Two things can be that clock, on
//! two threads:
//!
//! - a **capture stream** (`crate::pipewire`), for each chunk it receives;
//! - the **token clock** (the standalone renderer's `pacer-bridge-drain`
//!   thread), for each token posted by the input-pipe decoder or by the
//!   speaker-test idle feed.
//!
//! Only one of them may drain at a time. With both, the FIFO is drawn at the
//! sum of two rates while the renderer fills it at one: it runs dry, and the
//! drain makes up the difference with silence.
//!
//! The rule: **a capture stream that is delivering chunks is the clock;
//! otherwise the token clock is.** The capture stream holds it itself. It is
//! not read from the input mode or from the applied input state: those say
//! what was asked for and what was last decoded, and both can read "pipe
//! bridge" while a capture stream is running.
//!
//! - A capture stream can only drain through its [`CaptureDrainClock`], which
//!   takes the drain before it moves a sample: from the first chunk the
//!   stream receives in the streaming state.
//! - It gives the drain back when the stream leaves the streaming state (its
//!   client paused or left), and when it is dropped with the stream.
//! - The token clock is only handed the pacer while no capture stream holds
//!   the drain ([`InputControl::token_drain_pacer`]), and it asks again with
//!   the drain's ends in hand ([`TokenDrainPacer::drain`]), so it cannot come
//!   after a capture drain it did not see.
//!
//! A capture stream that is connected but idle is therefore not the clock: it
//! has no chunk to clock anything with, and what plays then (the input pipe,
//! a speaker test) brings its own tokens.
//!
//! Only the `pw_stream` capture backend drains. The client-node backend
//! (upstream clock mode) has no drain, and never takes the clock.

use crate::InputControl;
use audio_output::PacerHandle;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// The output pacer as the input side holds it: the handle of the current
/// audio output, and whether a capture stream is its drain clock.
pub(crate) struct OutputPacerDrain {
    /// Installed by the writer lifecycle once the output exists; `None` while
    /// there is no output, or one built with pacing off.
    pacer: Mutex<Option<PacerHandle>>,
    /// Capture streams that are delivering chunks: the capture is the drain
    /// clock while this is not zero. A count rather than a flag, so that a
    /// stream standing down gives back its own hold and no other. (It does
    /// not go above one today: the live-input manager joins a capture stream
    /// before it starts the next.)
    ///
    /// It belongs to the input, not to the handle: a capture stream outlives
    /// the audio outputs that are rebuilt under it.
    delivering_captures: AtomicUsize,
}

impl OutputPacerDrain {
    pub(crate) fn new() -> Self {
        Self {
            pacer: Mutex::new(None),
            delivering_captures: AtomicUsize::new(0),
        }
    }

    pub(crate) fn install(&self, handle: PacerHandle) {
        if let Ok(mut guard) = self.pacer.lock() {
            *guard = Some(handle);
        }
    }

    pub(crate) fn clear(&self) {
        if let Ok(mut guard) = self.pacer.lock() {
            *guard = None;
        }
    }

    /// Clone the currently-installed pacer handle, if any.
    fn pacer(&self) -> Option<PacerHandle> {
        self.pacer.lock().ok().and_then(|guard| guard.clone())
    }

    fn capture_is_clock(&self) -> bool {
        self.delivering_captures.load(Ordering::Acquire) != 0
    }

    pub(crate) fn for_capture(self: &Arc<Self>) -> CaptureDrainClock {
        CaptureDrainClock {
            drain: Arc::clone(self),
            streaming: false,
            delivering: false,
        }
    }

    pub(crate) fn for_tokens(&self) -> Option<TokenDrainPacer<'_>> {
        if self.capture_is_clock() {
            return None;
        }
        let pacer = self.pacer()?;
        Some(TokenDrainPacer { pacer, drain: self })
    }
}

/// A capture stream's hold on the pacer drain. One per stream, owned by it:
/// the stream drains through this and nothing else, and dropping it with the
/// stream gives the drain back.
pub struct CaptureDrainClock {
    drain: Arc<OutputPacerDrain>,
    /// The stream is in the streaming state.
    streaming: bool,
    /// This stream is counted in `delivering_captures`.
    delivering: bool,
}

impl CaptureDrainClock {
    /// The stream entered the streaming state, or left it: its client paused
    /// or went away, the stream failed or is being disconnected. Leaving it
    /// gives the drain back to the token clock, until the stream delivers
    /// again.
    pub fn set_streaming(&mut self, streaming: bool) {
        self.streaming = streaming;
        if !streaming {
            self.stand_down();
        }
    }

    /// A chunk of `in_frames` frames at `in_rate_hz` arrived: this stream is
    /// delivering, so it is the drain clock from here on, and the pacer is
    /// drained by what the chunk lasts.
    ///
    /// Strict 1:1 between input-chunk duration and ring-write duration: the
    /// ring sees a smooth stream regardless of the decoder's burst pattern.
    /// The pacer zero-fills the ring on underrun (counted in its telemetry)
    /// and until it is primed.
    ///
    /// A chunk handed over after the stream left the streaming state (it was
    /// already queued) neither drains nor takes the drain back: nothing would
    /// give it back again before the stream next stops.
    ///
    /// Returns whether a drain went through. It does not for such a chunk,
    /// when the output has no pacer, or when a token drain was still in
    /// progress.
    pub fn chunk_arrived(&mut self, in_frames: u64, in_rate_hz: u32) -> bool {
        if !self.streaming {
            return false;
        }
        if !self.delivering {
            self.delivering = true;
            self.drain
                .delivering_captures
                .fetch_add(1, Ordering::Release);
            log::debug!("Output pacer drain clock: capture stream");
        }
        let Some(pacer) = self.drain.pacer() else {
            return false;
        };
        let drain_samples = (in_frames
            .saturating_mul(pacer.out_sample_rate as u64)
            .saturating_mul(pacer.out_channels as u64)
            / (in_rate_hz as u64).max(1)) as usize;
        pacer.drain(drain_samples)
    }

    fn stand_down(&mut self) {
        if std::mem::take(&mut self.delivering) {
            self.drain
                .delivering_captures
                .fetch_sub(1, Ordering::Release);
            log::debug!("Output pacer drain clock: capture stream stood down");
        }
    }
}

impl Drop for CaptureDrainClock {
    fn drop(&mut self) {
        self.stand_down();
    }
}

/// The output pacer, for the token clock to drain. Only handed out while no
/// capture stream is the clock: see [`InputControl::token_drain_pacer`].
pub struct TokenDrainPacer<'a> {
    pacer: PacerHandle,
    drain: &'a OutputPacerDrain,
}

impl TokenDrainPacer<'_> {
    /// Output sample rate (Hz), to turn a token's duration into frames.
    pub fn out_sample_rate(&self) -> u32 {
        self.pacer.out_sample_rate
    }

    /// Output channel count, to turn frames into samples.
    pub fn out_channels(&self) -> u32 {
        self.pacer.out_channels
    }

    /// Move `drain_samples` from the pacer FIFO to the ring, unless a capture
    /// stream has become the clock since this was handed out.
    ///
    /// That is checked with the drain's ends held. A capture stream takes the
    /// drain before its first transfer, so once one of its transfers has gone
    /// through, every token drain that comes after it sees the capture
    /// holding the drain, and moves nothing. Returns whether this one went
    /// through.
    pub fn drain(&self, drain_samples: usize) -> bool {
        self.pacer
            .drain_if(drain_samples, || !self.drain.capture_is_clock())
    }
}

impl InputControl {
    /// The drain clock of a capture stream, to be owned by that stream for as
    /// long as it exists. It holds nothing until the stream is streaming and
    /// delivers its first chunk.
    pub fn capture_drain_clock(&self) -> CaptureDrainClock {
        self.pacer_drain.for_capture()
    }

    /// The output pacer for the token clock to drain: `None` when the output
    /// has no pacer, and while a capture stream is the drain clock.
    pub fn token_drain_pacer(&self) -> Option<TokenDrainPacer<'_>> {
        self.pacer_drain.for_tokens()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InputBackend, InputMode};
    use audio_output::pacer::PacerDrainEnds;
    use audio_output::ring_buffer_io::{RingReader, RingWriter, sample_ring};
    use std::sync::atomic::AtomicU64;

    const RATE: u32 = 48_000;
    const CHANNELS: u32 = 2;
    /// 10 ms of output, in samples across channels.
    const QUANTUM: usize = (RATE as usize / 100) * CHANNELS as usize;
    /// 10 ms of capture, in frames at `RATE`.
    const CHUNK_FRAMES: u64 = RATE as u64 / 100;

    /// An output's pacer with the renderer's end of its FIFO and the device
    /// callback's end of its ring.
    struct Output {
        pacer: PacerHandle,
        fifo: RingWriter,
        ring: RingReader,
    }

    fn output() -> Output {
        let (fifo, fifo_reader) = sample_ring(1 << 14);
        let (ring_writer, ring) = sample_ring(1 << 14);
        let pacer = PacerHandle::new(
            PacerDrainEnds {
                fifo: fifo_reader,
                ring: ring_writer,
            },
            0,
            RATE,
            CHANNELS,
        );
        pacer.pre_roll_complete.store(true, Ordering::Relaxed);
        Output { pacer, fifo, ring }
    }

    /// An input with `output`'s pacer installed, as the writer lifecycle
    /// leaves it.
    fn input_with(output: &Output) -> InputControl {
        let control = InputControl::default();
        control.install_output_pacer(output.pacer.clone());
        control
    }

    fn render(output: &mut Output, samples: usize) {
        assert_eq!(output.fifo.push_slice(&vec![0.5; samples]), samples);
    }

    fn played(output: &mut Output) -> Vec<f32> {
        let mut out = vec![0.0; output.ring.available()];
        let count = output.ring.pop_slice(&mut out);
        out.truncate(count);
        out
    }

    fn total(counter: &AtomicU64) -> f64 {
        f64::from_bits(counter.load(Ordering::Relaxed))
    }

    /// The drain clock of a capture stream whose client is playing.
    fn streaming_capture(input: &InputControl) -> CaptureDrainClock {
        let mut capture = input.capture_drain_clock();
        capture.set_streaming(true);
        capture
    }

    /// No capture stream, one that is connected and waits for a client, or
    /// one whose client has not sent anything yet: the token clock drains.
    #[test]
    fn the_token_clock_drains_while_no_capture_stream_delivers() {
        let mut output = output();
        let input = input_with(&output);
        render(&mut output, QUANTUM);
        let tokens = input.token_drain_pacer().expect("no capture stream at all");
        assert_eq!((tokens.out_sample_rate(), tokens.out_channels()), (RATE, 2));
        assert!(tokens.drain(QUANTUM));
        assert_eq!(played(&mut output), vec![0.5; QUANTUM]);

        let mut capture = input.capture_drain_clock();
        for streaming in [false, true] {
            capture.set_streaming(streaming);
            render(&mut output, QUANTUM);
            let tokens = input.token_drain_pacer().expect("nothing delivered");
            assert!(tokens.drain(QUANTUM));
            assert_eq!(played(&mut output), vec![0.5; QUANTUM]);
        }
        assert_eq!(total(&output.pacer.diag_underrun_total), 0.0);
    }

    /// A capture stream that delivers is the only drain clock: the token
    /// clock is not handed the pacer.
    #[test]
    fn a_delivering_capture_stream_is_the_only_drain_clock() {
        let mut output = output();
        let input = input_with(&output);
        let mut capture = streaming_capture(&input);

        render(&mut output, QUANTUM);
        assert!(capture.chunk_arrived(CHUNK_FRAMES, RATE));
        assert_eq!(played(&mut output), vec![0.5; QUANTUM]);

        render(&mut output, QUANTUM);
        assert!(input.token_drain_pacer().is_none());
        assert_eq!(output.fifo.fill(), QUANTUM, "left for the capture clock");
        assert!(capture.chunk_arrived(CHUNK_FRAMES, RATE));
        assert_eq!(played(&mut output), vec![0.5; QUANTUM]);
        assert_eq!(total(&output.pacer.diag_drain_total), 2.0 * QUANTUM as f64);
        assert_eq!(total(&output.pacer.diag_underrun_total), 0.0);
    }

    /// The token clock was handed the pacer, and a capture stream delivered
    /// its first chunk before the token drain got to it: the token drain
    /// moves nothing.
    #[test]
    fn a_token_drain_overtaken_by_a_capture_stream_moves_nothing() {
        let mut output = output();
        let input = input_with(&output);
        let mut capture = streaming_capture(&input);
        render(&mut output, 2 * QUANTUM);

        let tokens = input.token_drain_pacer().expect("nothing delivered yet");
        assert!(capture.chunk_arrived(CHUNK_FRAMES, RATE));
        assert!(!tokens.drain(QUANTUM));

        assert_eq!(output.fifo.fill(), QUANTUM, "one drain went through");
        assert_eq!(total(&output.pacer.diag_drain_total), QUANTUM as f64);
        assert_eq!(played(&mut output).len(), QUANTUM);
    }

    /// The drain goes back to the token clock when the stream leaves the
    /// streaming state, returns to the capture with the first chunk after it
    /// streams again, and goes back for good when the stream is gone.
    #[test]
    fn a_capture_stream_gives_the_drain_back_when_it_stops_streaming() {
        let output = output();
        let input = input_with(&output);
        let mut capture = streaming_capture(&input);

        capture.chunk_arrived(CHUNK_FRAMES, RATE);
        assert!(input.token_drain_pacer().is_none());
        capture.set_streaming(false);
        assert!(input.token_drain_pacer().is_some(), "client paused");
        capture.set_streaming(true);
        assert!(input.token_drain_pacer().is_some(), "nothing delivered yet");
        capture.chunk_arrived(CHUNK_FRAMES, RATE);
        assert!(input.token_drain_pacer().is_none(), "client resumed");
        drop(capture);
        assert!(input.token_drain_pacer().is_some(), "stream torn down");
    }

    /// A chunk that was queued when the stream paused is handed over after
    /// the pause. It must not take the drain back: no state change would
    /// follow to release it, and the token clock would stay out for the whole
    /// pause.
    #[test]
    fn a_chunk_left_over_from_before_a_pause_does_not_take_the_drain_back() {
        let mut output = output();
        let input = input_with(&output);
        let mut capture = streaming_capture(&input);
        capture.chunk_arrived(CHUNK_FRAMES, RATE);
        capture.set_streaming(false);
        let drained = total(&output.pacer.diag_drain_total);

        render(&mut output, QUANTUM);
        assert!(!capture.chunk_arrived(CHUNK_FRAMES, RATE));
        assert_eq!(total(&output.pacer.diag_drain_total), drained);
        let tokens = input.token_drain_pacer().expect("still the token clock's");
        assert!(tokens.drain(QUANTUM));
    }

    /// Should a capture stream ever be started before the one it replaces is
    /// gone, the one going away gives back its own hold, not its successor's.
    /// And giving back twice gives back once.
    #[test]
    fn a_stream_going_away_leaves_its_successor_the_drain() {
        let output = output();
        let input = input_with(&output);
        let mut old = streaming_capture(&input);
        let mut new = streaming_capture(&input);

        old.chunk_arrived(CHUNK_FRAMES, RATE);
        new.chunk_arrived(CHUNK_FRAMES, RATE);
        old.set_streaming(false);
        old.set_streaming(false);
        drop(old);
        assert!(input.token_drain_pacer().is_none(), "the new one delivers");
        new.set_streaming(false);
        assert!(input.token_drain_pacer().is_some());
    }

    /// The hold is the input's, not the output's: it is taken without a
    /// pacer to drain, and stands through the outputs rebuilt under the
    /// stream.
    #[test]
    fn the_capture_keeps_the_drain_across_audio_outputs() {
        let input = InputControl::default();
        let mut capture = streaming_capture(&input);
        assert!(
            !capture.chunk_arrived(CHUNK_FRAMES, RATE),
            "no pacer to drain"
        );

        let mut first = output();
        input.install_output_pacer(first.pacer.clone());
        assert!(input.token_drain_pacer().is_none());
        render(&mut first, QUANTUM);
        assert!(capture.chunk_arrived(CHUNK_FRAMES, RATE));
        assert_eq!(played(&mut first).len(), QUANTUM);

        input.clear_output_pacer();
        assert!(!capture.chunk_arrived(CHUNK_FRAMES, RATE));
        let mut second = output();
        input.install_output_pacer(second.pacer.clone());
        assert!(input.token_drain_pacer().is_none());
        render(&mut second, QUANTUM);
        assert!(capture.chunk_arrived(CHUNK_FRAMES, RATE));
        assert_eq!(played(&mut second).len(), QUANTUM);
        assert_eq!(total(&first.pacer.diag_drain_total), QUANTUM as f64);
    }

    /// A chunk is drained for what it lasts, whatever the carrier's rate and
    /// the output's shape: 10 ms of a 192 kHz carrier is 10 ms of output.
    #[test]
    fn a_chunk_is_drained_for_what_it_lasts() {
        let mut output = output();
        let input = input_with(&output);
        let mut capture = streaming_capture(&input);
        render(&mut output, QUANTUM);
        assert!(capture.chunk_arrived(1_920, 192_000));
        assert_eq!(played(&mut output).len(), QUANTUM);
        assert_eq!(total(&output.pacer.diag_drain_total), QUANTUM as f64);
    }

    /// The applied input state has no say. It reads "pipe bridge" in PipeWire
    /// mode from the first bitstream frame decoded from the capture on, and
    /// "pipewire" before a capture stream delivers anything.
    #[test]
    fn the_applied_input_state_does_not_pick_the_clock() {
        let output = output();
        let input = input_with(&output);
        let mut capture = streaming_capture(&input);

        capture.chunk_arrived(CHUNK_FRAMES, RATE);
        input.set_input_state(
            InputMode::Bridge,
            None,
            Some(8),
            Some(48_000),
            None,
            None,
            Some("bridge-decoded".to_string()),
        );
        assert!(input.token_drain_pacer().is_none());

        capture.set_streaming(false);
        input.set_input_state(
            InputMode::Pipewire,
            Some(InputBackend::Pipewire),
            Some(2),
            Some(192_000),
            Some("omniphony".to_string()),
            Some("Omniphony Bridge Input".to_string()),
            Some("pipewire-iec61937".to_string()),
        );
        assert!(input.token_drain_pacer().is_some());
    }
}
