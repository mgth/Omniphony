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
//!   takes the drain before it moves a sample, with each chunk the stream
//!   receives in the streaming state.
//! - Its hold lapses when the stream has delivered nothing for
//!   [`CAPTURE_SILENCE_LIMIT`]: a client can keep a stream streaming and send
//!   it nothing, and a stream that clocks nothing must not keep the tokens
//!   out. The next chunk takes the drain back.
//! - It gives the drain back at once when the stream leaves the streaming
//!   state (its client paused or left), and when it is dropped with the
//!   stream.
//! - The token clock is only handed the pacer while no capture stream holds
//!   the drain ([`InputControl::token_drain`]), and it asks again with the
//!   drain's ends in hand ([`TokenDrainPacer::drain`]), so it cannot come
//!   after a capture drain it did not see.
//! - A token refused under a hold that then lapses, with no chunk since,
//!   stood for audio nothing drained: the capture had stopped clocking. The
//!   token clock is told which hold refused it and when that hold lapses
//!   ([`CaptureHold`]), so that it can drain it then, without waiting for a
//!   token of its own ([`TokenDrainPacer::follows`]).
//!
//! A capture stream that is connected but idle is therefore not the clock: it
//! has no chunk to clock anything with, and what plays then (the input pipe,
//! a speaker test) brings its own tokens.
//!
//! Only the `pw_stream` capture backend drains. The client-node backend
//! (upstream clock mode) has no drain, and never takes the clock.

use crate::InputControl;
use audio_output::PacerHandle;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long a capture stream may hold the drain without receiving a chunk.
/// Past it the stream is not clocking anything, and the token clock drains.
///
/// Above the longest gap between two chunks of a stream that is delivering:
/// a chunk is a graph cycle, at most 8192 frames (PipeWire's default
/// `clock.quantum-limit`), 171 ms at 48 kHz. Below the 250 ms without a real
/// frame after which the speaker-test idle feed starts, so that a test
/// started in front of a silent stream is drained from its first token.
pub const CAPTURE_SILENCE_LIMIT: Duration = Duration::from_millis(200);

/// The output pacer as the input side holds it: the handle of the current
/// audio output, and whether a capture stream is its drain clock.
pub(crate) struct OutputPacerDrain {
    /// Installed by the writer lifecycle once the output exists; `None` while
    /// there is no output, or one built with pacing off.
    pacer: Mutex<Option<PacerHandle>>,
    /// Capture streams that hold the drain: streaming, and a chunk received
    /// since they last started to. The capture is the drain clock while this
    /// is not zero and `last_chunk_us` is recent. A count rather than a flag,
    /// so that a stream standing down gives back its own hold and no other.
    /// (It does not go above one today: the live-input manager joins a
    /// capture stream before it starts the next.)
    ///
    /// It belongs to the input, not to the handle: a capture stream outlives
    /// the audio outputs that are rebuilt under it.
    delivering_captures: AtomicUsize,
    /// When a capture stream last received a chunk, in microseconds since
    /// `epoch`. Written before `delivering_captures` is raised and before
    /// each transfer, so whoever sees a stream counted, or comes after one of
    /// its transfers, also sees the chunk that goes with it.
    last_chunk_us: AtomicU64,
    /// What `last_chunk_us` counts from.
    epoch: Instant,
}

impl OutputPacerDrain {
    pub(crate) fn new() -> Self {
        Self {
            pacer: Mutex::new(None),
            delivering_captures: AtomicUsize::new(0),
            last_chunk_us: AtomicU64::new(0),
            epoch: Instant::now(),
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

    fn micros(&self, at: Instant) -> u64 {
        at.saturating_duration_since(self.epoch).as_micros() as u64
    }

    /// The chunk a capture stream holds the drain from at `now_us`, if one
    /// does: it is counted as delivering, and its last chunk is not older
    /// than the silence limit. A chunk stamped after `now_us` was taken
    /// counts as just arrived.
    fn hold_at(&self, now_us: u64) -> Option<u64> {
        if self.delivering_captures.load(Ordering::Acquire) == 0 {
            return None;
        }
        let chunk_us = self.last_chunk_us.load(Ordering::Acquire);
        (now_us.saturating_sub(chunk_us) < CAPTURE_SILENCE_LIMIT.as_micros() as u64)
            .then_some(chunk_us)
    }

    fn capture_is_clock(&self, now_us: u64) -> bool {
        self.hold_at(now_us).is_some()
    }

    pub(crate) fn for_capture(self: &Arc<Self>) -> CaptureDrainClock {
        CaptureDrainClock {
            drain: Arc::clone(self),
            streaming: false,
            delivering: false,
        }
    }

    pub(crate) fn for_tokens(&self, now: Instant) -> TokenDrain<'_> {
        let Some(pacer) = self.pacer() else {
            return TokenDrain::NoPacer;
        };
        let now_us = self.micros(now);
        if let Some(chunk_us) = self.hold_at(now_us) {
            return TokenDrain::Held(CaptureHold {
                chunk_us,
                lapses_at: self.epoch + Duration::from_micros(chunk_us) + CAPTURE_SILENCE_LIMIT,
            });
        }
        TokenDrain::Granted(TokenDrainPacer {
            pacer,
            drain: self,
            now_us,
            last_chunk_us: self.last_chunk_us.load(Ordering::Acquire),
        })
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

    /// A chunk of `in_frames` frames at `in_rate_hz` arrived at `now`: this
    /// stream is delivering, so it is the drain clock from here on, and the
    /// pacer is drained by what the chunk lasts.
    ///
    /// Strict 1:1 between input-chunk duration and ring-write duration: the
    /// ring sees a smooth stream regardless of the decoder's burst pattern.
    /// The pacer zero-fills the ring on underrun (counted in its telemetry)
    /// and until it is primed.
    ///
    /// A chunk handed over after the stream left the streaming state (it was
    /// already queued) neither drains nor takes the drain back: the stream is
    /// not delivering again, and the hold would keep the tokens out until it
    /// lapsed.
    ///
    /// Returns whether a drain went through. It does not for such a chunk,
    /// when the output has no pacer, or when a token drain was still in
    /// progress.
    pub fn chunk_arrived(&mut self, now: Instant, in_frames: u64, in_rate_hz: u32) -> bool {
        if !self.streaming {
            return false;
        }
        // Before the count and before the transfer: see `last_chunk_us`.
        self.drain
            .last_chunk_us
            .store(self.drain.micros(now), Ordering::Release);
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

/// Whether the token clock may drain the output pacer, as
/// [`InputControl::token_drain`] answers it.
pub enum TokenDrain<'a> {
    /// It may: no capture stream holds the drain.
    Granted(TokenDrainPacer<'a>),
    /// A capture stream holds the drain.
    Held(CaptureHold),
    /// There is nothing to drain: no audio output, or one built with pacing
    /// off.
    NoPacer,
}

impl<'a> TokenDrain<'a> {
    /// The pacer, if the token clock may drain it.
    pub fn granted(self) -> Option<TokenDrainPacer<'a>> {
        match self {
            Self::Granted(pacer) => Some(pacer),
            Self::Held(_) | Self::NoPacer => None,
        }
    }
}

/// A capture stream's hold on the drain, as the token clock was refused by
/// it: which chunk it dates from, and when it lapses if no other chunk comes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureHold {
    /// The chunk the hold dates from, in the drain's microseconds.
    chunk_us: u64,
    lapses_at: Instant,
}

impl CaptureHold {
    /// When this hold lapses, if the stream delivers nothing more.
    pub fn lapses_at(&self) -> Instant {
        self.lapses_at
    }
}

/// The output pacer, for the token clock to drain. Only handed out while no
/// capture stream is the clock: see [`InputControl::token_drain`].
pub struct TokenDrainPacer<'a> {
    pacer: PacerHandle,
    drain: &'a OutputPacerDrain,
    /// When this was handed out, in the drain's microseconds.
    now_us: u64,
    /// The last chunk a capture stream received when this was handed out.
    last_chunk_us: u64,
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
    /// stream has become the clock since this was handed out: a first chunk,
    /// or the chunk that ends a silence.
    ///
    /// That is checked with the drain's ends held. A capture stream takes the
    /// drain before each transfer, so once one of its transfers has gone
    /// through, every token drain that comes after it sees the capture
    /// holding the drain, and moves nothing, until the stream has been silent
    /// for [`CAPTURE_SILENCE_LIMIT`] again. Returns whether this one went
    /// through.
    pub fn drain(&self, drain_samples: usize) -> bool {
        self.pacer
            .drain_if(drain_samples, || !self.drain.capture_is_clock(self.now_us))
    }

    /// Move what the pacer FIFO holds, up to `max_samples`, making up no
    /// silence (`PacerHandle::drain_available_if`): for the token clock
    /// catching up on what it was refused. Same check as
    /// [`drain`](Self::drain). Returns the samples moved, or `None` when the
    /// drain is not the token clock's or another drain is in progress.
    pub fn drain_available(&self, max_samples: usize) -> Option<usize> {
        self.pacer
            .drain_available_if(max_samples, || !self.drain.capture_is_clock(self.now_us))
    }

    /// Whether this was handed out once `hold` had lapsed, with no chunk
    /// since: the capture stream stopped clocking at the chunk `hold` dates
    /// from, so what the token clock was refused under it is still owed to
    /// the ring. Not after a later chunk: the stream clocked that time
    /// itself.
    pub fn follows(&self, hold: CaptureHold) -> bool {
        self.last_chunk_us == hold.chunk_us
    }
}

impl InputControl {
    /// The drain clock of a capture stream, to be owned by that stream for as
    /// long as it exists. It holds nothing until the stream is streaming and
    /// delivers its first chunk.
    pub fn capture_drain_clock(&self) -> CaptureDrainClock {
        self.pacer_drain.for_capture()
    }

    /// Whether the token clock may drain the output pacer at `now`: granted
    /// while the output has a pacer and no capture stream is the drain clock.
    pub fn token_drain(&self, now: Instant) -> TokenDrain<'_> {
        self.pacer_drain.for_tokens(now)
    }

    /// The output pacer, if the token clock may drain it at `now`: see
    /// [`token_drain`](Self::token_drain).
    pub fn token_drain_pacer(&self, now: Instant) -> Option<TokenDrainPacer<'_>> {
        self.token_drain(now).granted()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{InputBackend, InputMode};
    use audio_output::pacer::PacerDrainEnds;
    use audio_output::ring_buffer_io::{RingReader, RingWriter, sample_ring};

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
        let (fifo, fifo_reader) = sample_ring(1 << 13, CHANNELS as usize);
        let (ring_writer, ring) = sample_ring(1 << 13, CHANNELS as usize);
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
    /// leaves it, and the instant the test counts from. Every instant of a
    /// test is that one plus so many milliseconds: nothing here depends on
    /// how long the test takes to run.
    fn input_with(output: &Output) -> (InputControl, Instant) {
        let control = InputControl::default();
        control.install_output_pacer(output.pacer.clone());
        (control, Instant::now())
    }

    fn ms(count: u64) -> Duration {
        Duration::from_millis(count)
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
        let (input, t0) = input_with(&output);
        render(&mut output, QUANTUM);
        let tokens = input.token_drain_pacer(t0).expect("no capture stream");
        assert_eq!((tokens.out_sample_rate(), tokens.out_channels()), (RATE, 2));
        assert!(tokens.drain(QUANTUM));
        assert_eq!(played(&mut output), vec![0.5; QUANTUM]);

        let mut capture = input.capture_drain_clock();
        for streaming in [false, true] {
            capture.set_streaming(streaming);
            render(&mut output, QUANTUM);
            let tokens = input.token_drain_pacer(t0).expect("nothing delivered");
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
        let (input, t0) = input_with(&output);
        let mut capture = streaming_capture(&input);

        render(&mut output, QUANTUM);
        assert!(capture.chunk_arrived(t0, CHUNK_FRAMES, RATE));
        assert_eq!(played(&mut output), vec![0.5; QUANTUM]);

        render(&mut output, QUANTUM);
        assert!(input.token_drain_pacer(t0 + ms(5)).is_none());
        assert_eq!(output.fifo.fill(), QUANTUM, "left for the capture clock");
        assert!(capture.chunk_arrived(t0 + ms(10), CHUNK_FRAMES, RATE));
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
        let (input, t0) = input_with(&output);
        let mut capture = streaming_capture(&input);
        render(&mut output, 2 * QUANTUM);

        let tokens = input.token_drain_pacer(t0).expect("nothing delivered yet");
        assert!(capture.chunk_arrived(t0 + ms(1), CHUNK_FRAMES, RATE));
        assert!(!tokens.drain(QUANTUM));

        assert_eq!(output.fifo.fill(), QUANTUM, "one drain went through");
        assert_eq!(total(&output.pacer.diag_drain_total), QUANTUM as f64);
        assert_eq!(played(&mut output).len(), QUANTUM);
    }

    /// The drain goes back to the token clock when the stream leaves the
    /// streaming state, returns to the capture with the first chunk after it
    /// streams again, and goes back for good when the stream is gone. None of
    /// it waits for the silence limit.
    #[test]
    fn a_capture_stream_gives_the_drain_back_when_it_stops_streaming() {
        let output = output();
        let (input, t0) = input_with(&output);
        let mut capture = streaming_capture(&input);

        capture.chunk_arrived(t0, CHUNK_FRAMES, RATE);
        assert!(input.token_drain_pacer(t0).is_none());
        capture.set_streaming(false);
        assert!(input.token_drain_pacer(t0).is_some(), "client paused");
        capture.set_streaming(true);
        assert!(
            input.token_drain_pacer(t0).is_some(),
            "nothing delivered yet"
        );
        capture.chunk_arrived(t0 + ms(1), CHUNK_FRAMES, RATE);
        assert!(input.token_drain_pacer(t0 + ms(1)).is_none(), "resumed");
        drop(capture);
        assert!(input.token_drain_pacer(t0 + ms(1)).is_some(), "torn down");
    }

    /// A client can keep its stream in the streaming state and send it
    /// nothing. The stream then clocks nothing, and must not keep out the
    /// tokens of what does play (the input pipe, a speaker test): past the
    /// silence limit they drain, and nothing is left in the FIFO.
    #[test]
    fn a_stream_that_stops_delivering_while_streaming_lets_the_tokens_drain() {
        let mut output = output();
        let (input, t0) = input_with(&output);
        let mut capture = streaming_capture(&input);
        render(&mut output, QUANTUM);
        assert!(capture.chunk_arrived(t0, CHUNK_FRAMES, RATE));
        assert_eq!(played(&mut output).len(), QUANTUM);

        // Within the limit the stream may just be between two chunks.
        let just_short = t0 + CAPTURE_SILENCE_LIMIT - ms(1);
        assert!(input.token_drain_pacer(just_short).is_none());

        // Past it, with no state change: 20 tokens of 10 ms each.
        for token in 0..20 {
            render(&mut output, QUANTUM);
            let at = t0 + CAPTURE_SILENCE_LIMIT + ms(10 * token);
            let tokens = input.token_drain_pacer(at).expect("stream silent");
            assert!(tokens.drain(QUANTUM), "token {token}");
            assert_eq!(played(&mut output), vec![0.5; QUANTUM], "token {token}");
        }
        assert_eq!(output.fifo.fill(), 0, "nothing left in the FIFO");
        assert_eq!(total(&output.pacer.diag_underrun_total), 0.0);
    }

    /// When the chunks come back the capture has the drain again from that
    /// chunk on: a token later than it is refused the pacer, and one that was
    /// handed the pacer during the silence and drains after the chunk moves
    /// nothing.
    #[test]
    fn a_stream_that_delivers_again_takes_the_drain_back_with_its_chunk() {
        let mut output = output();
        let (input, t0) = input_with(&output);
        let mut capture = streaming_capture(&input);
        capture.chunk_arrived(t0, CHUNK_FRAMES, RATE);
        let silent = t0 + CAPTURE_SILENCE_LIMIT + ms(300);
        let drained = total(&output.pacer.diag_drain_total);

        render(&mut output, 2 * QUANTUM);
        let handed_out_during_the_silence = input.token_drain_pacer(silent).expect("silent");
        assert!(capture.chunk_arrived(silent + ms(1), CHUNK_FRAMES, RATE));
        assert!(!handed_out_during_the_silence.drain(QUANTUM));
        assert!(input.token_drain_pacer(silent + ms(2)).is_none());
        assert_eq!(
            total(&output.pacer.diag_drain_total),
            drained + QUANTUM as f64,
            "the chunk's drain only"
        );

        // And it keeps it for as long as chunks keep coming, however long
        // after the first one.
        for chunk in 1..=50 {
            let at = silent + ms(1 + 10 * chunk);
            capture.chunk_arrived(at, CHUNK_FRAMES, RATE);
            assert!(input.token_drain_pacer(at + ms(9)).is_none(), "{chunk}");
        }
    }

    /// A refused token is told which hold refused it and when that hold
    /// lapses: the stream's last chunk plus the silence limit, the first
    /// instant the hold reads lapsed. A later chunk is a later hold.
    #[test]
    fn a_refused_token_is_told_when_the_hold_lapses() {
        let output = output();
        let (input, t0) = input_with(&output);
        let mut capture = streaming_capture(&input);
        capture.chunk_arrived(t0, CHUNK_FRAMES, RATE);

        let TokenDrain::Held(hold) = input.token_drain(t0 + ms(5)) else {
            panic!("the capture holds the drain");
        };
        // The first instant the hold reads lapsed: the chunk's time counts in
        // whole microseconds.
        let lapse = t0 + CAPTURE_SILENCE_LIMIT;
        assert!(hold.lapses_at() <= lapse);
        assert!(lapse - hold.lapses_at() < Duration::from_micros(1));
        let just_before = hold.lapses_at() - Duration::from_micros(1);
        assert!(matches!(
            input.token_drain(just_before),
            TokenDrain::Held(_)
        ));
        assert!(matches!(
            input.token_drain(hold.lapses_at()),
            TokenDrain::Granted(_)
        ));

        capture.chunk_arrived(t0 + ms(10), CHUNK_FRAMES, RATE);
        let TokenDrain::Held(later) = input.token_drain(t0 + ms(15)) else {
            panic!("still held");
        };
        assert_ne!(later, hold);
        assert!(later.lapses_at() > hold.lapses_at() + ms(9));
    }

    /// The pacer handed out once a hold has lapsed follows that hold if no
    /// chunk came since, whether the hold lapsed by silence or the stream
    /// stopped streaming; not once the stream has delivered again.
    #[test]
    fn a_grant_follows_the_hold_it_comes_after_only_with_no_chunk_since() {
        let output = output();
        let (input, t0) = input_with(&output);
        let mut capture = streaming_capture(&input);
        capture.chunk_arrived(t0, CHUNK_FRAMES, RATE);
        let TokenDrain::Held(hold) = input.token_drain(t0 + ms(5)) else {
            panic!("held");
        };

        let lapsed = input.token_drain_pacer(hold.lapses_at()).expect("lapsed");
        assert!(lapsed.follows(hold), "lapsed by silence");
        capture.set_streaming(false);
        let paused = input.token_drain_pacer(t0 + ms(6)).expect("paused");
        assert!(paused.follows(hold), "the stream stopped streaming");

        capture.set_streaming(true);
        capture.chunk_arrived(t0 + ms(50), CHUNK_FRAMES, RATE);
        let later = input
            .token_drain_pacer(t0 + ms(50) + CAPTURE_SILENCE_LIMIT)
            .expect("lapsed again");
        assert!(!later.follows(hold), "the stream clocked in between");
    }

    /// Catching up goes through the same check as a drain: a capture stream
    /// that delivers in the meantime keeps it out.
    #[test]
    fn catching_up_is_refused_once_the_capture_delivers_again() {
        let mut output = output();
        let (input, t0) = input_with(&output);
        let mut capture = streaming_capture(&input);
        render(&mut output, 2 * QUANTUM);

        let tokens = input.token_drain_pacer(t0).expect("nothing delivered yet");
        assert_eq!(tokens.drain_available(QUANTUM), Some(QUANTUM));
        capture.chunk_arrived(t0 + ms(1), CHUNK_FRAMES, RATE);
        assert_eq!(tokens.drain_available(QUANTUM), None);
        assert_eq!(
            played(&mut output).len(),
            2 * QUANTUM,
            "token's, then chunk's"
        );
    }

    /// No pacer, no hold: there is nothing for the token clock to owe.
    #[test]
    fn without_a_pacer_the_token_clock_is_neither_granted_nor_held() {
        let input = InputControl::default();
        let t0 = Instant::now();
        let mut capture = streaming_capture(&input);
        capture.chunk_arrived(t0, CHUNK_FRAMES, RATE);
        assert!(matches!(input.token_drain(t0), TokenDrain::NoPacer));
    }

    /// A chunk that was queued when the stream paused is handed over after
    /// the pause. It must not take the drain back: it is not the stream
    /// delivering again.
    #[test]
    fn a_chunk_left_over_from_before_a_pause_does_not_take_the_drain_back() {
        let mut output = output();
        let (input, t0) = input_with(&output);
        let mut capture = streaming_capture(&input);
        capture.chunk_arrived(t0, CHUNK_FRAMES, RATE);
        capture.set_streaming(false);
        let drained = total(&output.pacer.diag_drain_total);

        render(&mut output, QUANTUM);
        assert!(!capture.chunk_arrived(t0 + ms(1), CHUNK_FRAMES, RATE));
        assert_eq!(total(&output.pacer.diag_drain_total), drained);
        let tokens = input
            .token_drain_pacer(t0 + ms(2))
            .expect("still the token clock's");
        assert!(tokens.drain(QUANTUM));
    }

    /// Should a capture stream ever be started before the one it replaces is
    /// gone, the one going away gives back its own hold, not its successor's.
    /// And giving back twice gives back once.
    #[test]
    fn a_stream_going_away_leaves_its_successor_the_drain() {
        let output = output();
        let (input, t0) = input_with(&output);
        let mut old = streaming_capture(&input);
        let mut new = streaming_capture(&input);

        old.chunk_arrived(t0, CHUNK_FRAMES, RATE);
        new.chunk_arrived(t0, CHUNK_FRAMES, RATE);
        old.set_streaming(false);
        old.set_streaming(false);
        drop(old);
        assert!(
            input.token_drain_pacer(t0).is_none(),
            "the new one delivers"
        );
        new.set_streaming(false);
        assert!(input.token_drain_pacer(t0).is_some());
    }

    /// The hold is the input's, not the output's: it is taken without a
    /// pacer to drain, and stands through the outputs rebuilt under the
    /// stream.
    #[test]
    fn the_capture_keeps_the_drain_across_audio_outputs() {
        let input = InputControl::default();
        let t0 = Instant::now();
        let mut capture = streaming_capture(&input);
        assert!(
            !capture.chunk_arrived(t0, CHUNK_FRAMES, RATE),
            "no pacer to drain"
        );

        let mut first = output();
        input.install_output_pacer(first.pacer.clone());
        assert!(input.token_drain_pacer(t0).is_none());
        render(&mut first, QUANTUM);
        assert!(capture.chunk_arrived(t0 + ms(10), CHUNK_FRAMES, RATE));
        assert_eq!(played(&mut first).len(), QUANTUM);

        input.clear_output_pacer();
        assert!(!capture.chunk_arrived(t0 + ms(20), CHUNK_FRAMES, RATE));
        let mut second = output();
        input.install_output_pacer(second.pacer.clone());
        assert!(input.token_drain_pacer(t0 + ms(20)).is_none());
        render(&mut second, QUANTUM);
        assert!(capture.chunk_arrived(t0 + ms(30), CHUNK_FRAMES, RATE));
        assert_eq!(played(&mut second).len(), QUANTUM);
        assert_eq!(total(&first.pacer.diag_drain_total), QUANTUM as f64);
    }

    /// A chunk is drained for what it lasts, whatever the carrier's rate and
    /// the output's shape: 10 ms of a 192 kHz carrier is 10 ms of output.
    #[test]
    fn a_chunk_is_drained_for_what_it_lasts() {
        let mut output = output();
        let (input, t0) = input_with(&output);
        let mut capture = streaming_capture(&input);
        render(&mut output, QUANTUM);
        assert!(capture.chunk_arrived(t0, 1_920, 192_000));
        assert_eq!(played(&mut output).len(), QUANTUM);
        assert_eq!(total(&output.pacer.diag_drain_total), QUANTUM as f64);
    }

    /// A stream that delivers is never taken for a silent one: the limit is
    /// longer than the longest graph cycle, PipeWire's quantum limit at the
    /// lowest of the usual rates. (That it is shorter than the idle feed's
    /// holdoff is held next to that holdoff, in the renderer's `idle_feed`.)
    #[test]
    fn the_silence_limit_is_longer_than_a_graph_cycle() {
        let longest_cycle = Duration::from_secs_f64(8192.0 / 44_100.0);
        assert!(CAPTURE_SILENCE_LIMIT > longest_cycle);
    }

    /// The applied input state has no say. It reads "pipe bridge" in PipeWire
    /// mode from the first bitstream frame decoded from the capture on, and
    /// "pipewire" before a capture stream delivers anything.
    #[test]
    fn the_applied_input_state_does_not_pick_the_clock() {
        let output = output();
        let (input, t0) = input_with(&output);
        let mut capture = streaming_capture(&input);

        capture.chunk_arrived(t0, CHUNK_FRAMES, RATE);
        input.set_input_state(
            InputMode::Bridge,
            None,
            Some(8),
            Some(48_000),
            None,
            None,
            Some("bridge-decoded".to_string()),
        );
        assert!(input.token_drain_pacer(t0).is_none());

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
        assert!(input.token_drain_pacer(t0).is_some());
    }
}
