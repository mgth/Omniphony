//! A burst carrier on the sink's linear PCM
//! (`docs/iamf-transport-plan.md`, transport B).
//!
//! IAMF has no IEC 958 codec a PipeWire sink can negotiate, and a link that
//! only carries linear PCM (netjack2) has no encoded format at all. Its
//! bursts come as what they are on such a link: 16-bit words, one per sample,
//! in the float PCM of the sink. Played as the audio they are not, they are
//! noise near full scale on every speaker.
//!
//! The gate looks at each chunk of the sink's PCM and says what it is:
//!
//! - **Words**, while bursts are coming: the chunk as the 16-bit words it
//!   carries, for the deframer, on the transport count of an IEC 958 carrier.
//! - **Idle**, between two bursts of a carrier: nothing but the zeros that
//!   pad a burst to its period, or that a player which paused or stopped
//!   leaves behind. The carrier's time passes with nothing on it, as when a
//!   player on an IEC 958 format gives nothing for a cycle.
//! - **Audio**, on the sink that takes both, when no burst has started for
//!   the silence that ends a stream.
//! - **Nothing**, on the sink that takes a carrier only, between two burst
//!   streams: whatever it is given then is never played.
//!
//! Bursts start on a frame of the carrier, so they are found by the preamble
//! that opens a frame ([`spdif::pcm::iamf_bursts`]).

use spdif::pcm::{SAMPLE_BYTES, iamf_bursts, words_from_f32le};

use super::reader::END_OF_STREAM_S;

/// What a chunk of the sink's linear PCM is.
#[derive(Debug, PartialEq, Eq)]
pub enum Take {
    /// Audio, to render as it is.
    Audio,
    /// A burst carrier: its 16-bit little-endian words.
    Words(Vec<u8>),
    /// A burst carrier with nothing on it: its time passes.
    Idle,
    /// Not a stream.
    Nothing,
}

/// How long a carrier-only sink is given sound that holds no burst before
/// it is said (s).
const STRAY_WARN_S: f64 = 1.0;

/// A carrier that is coming.
#[derive(Debug, Clone, Copy)]
struct Coming {
    /// Carrier frames since the last burst started.
    since_burst: u64,
    /// Bytes of that burst (as words) still to come.
    burst_left: u64,
}

/// Tells a burst carrier from audio on the sink's linear PCM.
#[derive(Debug)]
pub struct CarrierGate {
    /// The sink takes a carrier and nothing else.
    only: bool,
    /// The carrier that is coming, if one is.
    coming: Option<Coming>,
    /// Samples of the carrier that were not 16-bit words since it started,
    /// and whether that was said.
    off_grid: u64,
    off_grid_said: bool,
    /// Frames of sound without a burst a carrier-only sink was given, and
    /// whether that was said.
    stray_frames: u64,
    stray_said: bool,
}

impl CarrierGate {
    /// A gate for the sink that takes a carrier and nothing else (`only`),
    /// or for the one that takes audio too.
    pub fn new(only: bool) -> Self {
        Self {
            only,
            coming: None,
            off_grid: 0,
            off_grid_said: false,
            stray_frames: 0,
            stray_said: false,
        }
    }

    /// The format changed, or the clock restarted: nothing carries over.
    pub fn reset(&mut self) {
        *self = Self::new(self.only);
    }

    /// What `samples` are: `f32` little-endian, `channels` to a frame, at
    /// `rate` frames a second.
    pub fn take(&mut self, samples: &[u8], channels: u32, rate: u32) -> Take {
        let channels = channels.max(1) as usize;
        let frame_words = (channels * 2) as u64;
        let frames = (samples.len() / (channels * SAMPLE_BYTES)) as u64;
        let last_burst = iamf_bursts(samples, channels).last();
        // Whether the deframer needs these samples: the end of a burst that
        // started before them, or a burst that starts in them.
        let in_burst =
            last_burst.is_some() || self.coming.is_some_and(|coming| coming.burst_left > 0);
        self.coming = match (last_burst, self.coming) {
            (Some((frame, burst_bytes)), coming) => {
                if coming.is_none() {
                    log::info!(
                        "sync host: IAMF bursts on the sink's linear PCM ({channels} channels at \
                         {rate} Hz): taken as a carrier, not as audio"
                    );
                    self.off_grid = 0;
                    self.off_grid_said = false;
                    self.stray_frames = 0;
                    self.stray_said = false;
                }
                let since_burst = frames - frame as u64;
                Some(Coming {
                    since_burst,
                    burst_left: (burst_bytes as u64).saturating_sub(since_burst * frame_words),
                })
            }
            // No burst for the silence that ends a stream: the carrier is
            // gone, and what comes now is the sink's own again.
            (None, Some(coming)) => Some(Coming {
                since_burst: coming.since_burst + frames,
                burst_left: coming.burst_left.saturating_sub(frames * frame_words),
            })
            .filter(|coming| (coming.since_burst as f64) < END_OF_STREAM_S * f64::from(rate)),
            (None, None) => None,
        };
        if self.coming.is_some() {
            // Between two bursts the carrier holds zeros, and so it does once
            // its player has paused or stopped: nothing for the deframer, and
            // nothing that says the source is still sending.
            if !in_burst && is_silent(samples) {
                return Take::Idle;
            }
            let mut words = Vec::new();
            self.off_grid += words_from_f32le(samples, &mut words) as u64;
            if self.off_grid > 0 && !self.off_grid_said {
                self.off_grid_said = true;
                log::warn!(
                    "sync host: the carrier is not bit-exact: {} of its samples are not 16-bit \
                     words. Something on the way changes them (a volume other than 100 %, a \
                     mixer, a resampler); the bursts it reaches are lost",
                    self.off_grid
                );
            }
            return Take::Words(words);
        }
        if !self.only {
            return Take::Audio;
        }
        self.note_stray(samples, frames, rate);
        Take::Nothing
    }

    /// A carrier-only sink was given `frames` that hold no burst: said once
    /// when they are not silence either, since a carrier that something
    /// changed on the way looks just like this.
    fn note_stray(&mut self, samples: &[u8], frames: u64, rate: u32) {
        if is_silent(samples) {
            self.stray_frames = 0;
            return;
        }
        self.stray_frames += frames;
        if !self.stray_said && self.stray_frames as f64 >= STRAY_WARN_S * f64::from(rate) {
            self.stray_said = true;
            log::warn!(
                "sync host: the carrier sink is given sound that holds no burst, and plays none \
                 of it. A carrier whose samples were changed on the way (a volume other than \
                 100 %, a mixer, a resampler) looks like this, and so does plain audio sent here"
            );
        }
    }
}

/// Whether every sample is zero.
fn is_silent(samples: &[u8]) -> bool {
    samples
        .as_chunks::<SAMPLE_BYTES>()
        .0
        .iter()
        .all(|s| f32::from_le_bytes(*s) == 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use spdif::SpdifParser;
    use spdif::burst::write_iamf_burst;

    const CHANNELS: u32 = 8;
    const RATE: u32 = 48_000;
    /// A graph cycle.
    const CYCLE_FRAMES: usize = 1024;
    /// A 20 ms unit on the carrier.
    const PERIOD_FRAMES: usize = 960;

    /// `count` bursts of `period_frames` each, with `payload` bytes of their
    /// number as payload.
    fn carrier_of(count: u8, payload: usize, period_frames: usize) -> Vec<u8> {
        let mut words = Vec::new();
        for n in 0..count {
            write_iamf_burst(
                &mut words,
                &vec![n; payload],
                period_frames * CHANNELS as usize * 2,
            )
            .unwrap();
        }
        words
    }

    fn carrier(count: u8) -> Vec<u8> {
        carrier_of(count, 300, PERIOD_FRAMES)
    }

    /// The float samples a carrier is on the link, at `gain`.
    fn samples(words: &[u8], gain: f32) -> Vec<u8> {
        words
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|w| (f32::from(i16::from_le_bytes(*w)) / 32768.0 * gain).to_le_bytes())
            .collect()
    }

    fn silence(frames: usize) -> Vec<u8> {
        vec![0u8; frames * CHANNELS as usize * SAMPLE_BYTES]
    }

    /// A tone on every channel.
    fn audio(frames: usize) -> Vec<u8> {
        (0..frames * CHANNELS as usize)
            .flat_map(|i| ((1.0 + i as f32 * 0.01).sin() * 0.25).to_le_bytes())
            .collect()
    }

    /// Feed `samples` in graph cycles and collect what the gate made of
    /// each.
    fn feed(gate: &mut CarrierGate, samples: &[u8]) -> Vec<Take> {
        samples
            .chunks(CYCLE_FRAMES * CHANNELS as usize * SAMPLE_BYTES)
            .map(|cycle| gate.take(cycle, CHANNELS, RATE))
            .collect()
    }

    /// What the deframer makes of the takes, fed as the host feeds it: an
    /// idle cycle is carrier time with nothing on it, which the parser never
    /// sees.
    fn payloads(takes: &[Take]) -> Vec<Vec<u8>> {
        let mut parser = SpdifParser::new();
        let mut payloads = Vec::new();
        for take in takes {
            if let Take::Words(words) = take {
                parser.push_bytes(words);
            }
            while let Some(packet) = parser.get_next_packet() {
                assert_eq!(packet.data_type, spdif::DATA_TYPE_IAMF_PRIVATE);
                payloads.push(packet.payload);
            }
        }
        payloads
    }

    fn carried(takes: &[Take]) -> bool {
        takes
            .iter()
            .all(|take| matches!(take, Take::Words(_) | Take::Idle))
    }

    /// On the sink that takes audio too: silence and sound are audio, a
    /// carrier is its words from the cycle its first burst starts in, and it
    /// is audio again once no burst has started for the silence that ends a
    /// stream. The bursts come out of the parser as they were written.
    #[test]
    fn a_carrier_among_audio_is_taken_as_words_and_never_as_audio() {
        let mut gate = CarrierGate::new(false);
        assert!(
            feed(&mut gate, &audio(4096))
                .iter()
                .all(|take| *take == Take::Audio)
        );
        // 100 frames of silence, then the carrier: the cycle the first burst
        // starts in is words already.
        let mut stream = silence(100);
        stream.extend_from_slice(&samples(&carrier(30), 1.0));
        let takes = feed(&mut gate, &stream);
        assert!(matches!(takes[0], Take::Words(_)) && carried(&takes));
        let payloads = payloads(&takes);
        assert_eq!(payloads.len(), 30);
        assert!(
            payloads
                .iter()
                .enumerate()
                .all(|(n, p)| *p == [n as u8; 300])
        );

        // The player stops: the carrier idles (nothing tells the host its
        // source is still sending) for the silence that ends a stream, less
        // what the last burst's own period took of it; then it is the sink's
        // audio again.
        let takes = feed(&mut gate, &silence(RATE as usize));
        let held = takes.iter().take_while(|t| **t == Take::Idle).count();
        assert_eq!(held, 22, "{} cycles of 1024 frames", takes.len());
        assert!(takes[held..].iter().all(|take| *take == Take::Audio));
    }

    /// A burst longer than a graph cycle is handed over whole, whatever it
    /// holds: zeros inside a payload are the payload's, not the carrier
    /// idling. The cycles of padding that follow it are idle.
    #[test]
    fn a_long_burst_is_whole_and_the_padding_after_it_idles() {
        // A unit of 4096 frames whose payload of zeros (silent LPCM) spans
        // two and a half cycles of the carrier.
        const LONG_PERIOD: usize = 4096;
        let payload = CYCLE_FRAMES * CHANNELS as usize * 2 * 5 / 2;
        let sent = carrier_of(6, payload, LONG_PERIOD);
        let mut zeros = sent.clone();
        for burst in zeros.chunks_mut(LONG_PERIOD * CHANNELS as usize * 2) {
            burst[8..8 + payload].fill(0);
        }
        let mut gate = CarrierGate::new(true);
        let takes = feed(&mut gate, &samples(&zeros, 1.0));
        // Per period of four cycles: three with the burst in them, one idle.
        assert_eq!(takes.len(), 24);
        for (cycle, take) in takes.iter().enumerate() {
            assert_eq!(
                *take == Take::Idle,
                cycle % 4 == 3,
                "cycle {cycle}: {take:?}"
            );
        }
        let payloads = payloads(&takes);
        assert_eq!(payloads.len(), 6);
        assert!(payloads.iter().all(|p| *p == vec![0u8; payload]));
    }

    /// On the sink that takes a carrier only, nothing but a carrier is a
    /// stream: silence, sound, and a carrier at another level are not.
    #[test]
    fn a_carrier_only_sink_plays_nothing_but_a_carrier() {
        let mut gate = CarrierGate::new(true);
        for not_a_carrier in [
            silence(4096),
            audio(RATE as usize * 2),
            samples(&carrier(60), 0.36),
        ] {
            assert!(
                feed(&mut gate, &not_a_carrier)
                    .iter()
                    .all(|take| *take == Take::Nothing)
            );
        }
        assert!(gate.stray_said, "sound without a burst is said");
        let takes = feed(&mut gate, &samples(&carrier(10), 1.0));
        assert!(carried(&takes));
        assert_eq!(payloads(&takes).len(), 10);
        assert!(!gate.stray_said && !gate.off_grid_said);
        let takes = feed(&mut gate, &silence(RATE as usize));
        assert_eq!(takes.first(), Some(&Take::Idle));
        assert_eq!(takes.last(), Some(&Take::Nothing));
    }

    /// A carrier that something starts to change while it runs keeps being
    /// taken as words (never as audio) until it has gone for the silence
    /// that ends a stream, and the damage is said.
    #[test]
    fn a_carrier_changed_on_the_way_is_said_and_not_played() {
        let mut gate = CarrierGate::new(false);
        let mut stream = samples(&carrier(10), 1.0);
        stream.extend_from_slice(&samples(&carrier(10), 0.5));
        let takes = feed(&mut gate, &stream);
        assert!(carried(&takes));
        assert!(gate.off_grid_said);
        // A new format or a clock restart starts it over.
        gate.reset();
        assert!(!gate.off_grid_said && gate.coming.is_none());
    }
}
