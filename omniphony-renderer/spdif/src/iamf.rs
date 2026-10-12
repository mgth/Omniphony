//! Cutting an IAMF sequence into the bursts of the private IEC 61937 type
//! (`docs/iamf-transport-plan.md`): the descriptors, the temporal units, and
//! the carrier period one unit lasts.
//!
//! This reads no more of IAMF than a sender needs: OBU framing, which OBUs
//! are descriptors, where a temporal unit ends, and the frame length and
//! sample rate the codec config states. Decoding is the bridge's.

use std::ops::Range;

use crate::burst::{BurstError, write_iamf_burst};

const OBU_CODEC_CONFIG: u8 = 0;
const OBU_MIX_PRESENTATION: u8 = 2;
const OBU_PARAMETER_BLOCK: u8 = 3;
const OBU_TEMPORAL_DELIMITER: u8 = 4;
const OBU_AUDIO_FRAME: u8 = 5;
const OBU_AUDIO_FRAME_ID0: u8 = 6;
const OBU_AUDIO_FRAME_ID17: u8 = 23;
const OBU_SEQUENCE_HEADER: u8 = 31;

/// `obu_redundant_copy` in an OBU header.
const REDUNDANT_COPY: u8 = 0x04;
const TRIMMING_STATUS: u8 = 0x02;
const EXTENSION: u8 = 0x01;

/// Why a stream cannot be carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IamfError {
    /// Not whole OBUs, or a field that runs past its OBU.
    Malformed(&'static str),
    /// No sequence header at the start.
    NoSequenceHeader,
    /// No codec config among the descriptors.
    NoCodecConfig,
    /// A second IA sequence: one stream carries one.
    SeveralSequences,
    /// The codec config does not state its sample rate (AAC): the caller
    /// has to.
    UnknownSampleRate,
    /// A unit does not last a whole number of carrier frames.
    FractionalPeriod {
        unit_samples: u32,
        sample_rate: u32,
        carrier_rate: u32,
    },
    Burst(BurstError),
}

impl std::fmt::Display for IamfError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(what) => write!(f, "malformed IAMF stream: {what}"),
            Self::NoSequenceHeader => {
                f.write_str("the stream does not open with an IA sequence header")
            }
            Self::NoCodecConfig => f.write_str("the sequence has no codec config"),
            Self::SeveralSequences => f.write_str("the stream holds more than one IA sequence"),
            Self::UnknownSampleRate => {
                f.write_str("the codec config does not state its sample rate: give it")
            }
            Self::FractionalPeriod {
                unit_samples,
                sample_rate,
                carrier_rate,
            } => write!(
                f,
                "a unit of {unit_samples} samples at {sample_rate} Hz is not a whole number of frames at {carrier_rate} Hz"
            ),
            Self::Burst(err) => err.fmt(f),
        }
    }
}

impl std::error::Error for IamfError {}

impl From<BurstError> for IamfError {
    fn from(err: BurstError) -> Self {
        Self::Burst(err)
    }
}

/// A leb128 of at most 8 bytes (IAMF §2.4) at the start of `data`: the value
/// and the bytes it took.
fn leb128(data: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for (index, &byte) in data.iter().take(8).enumerate() {
        value |= u64::from(byte & 0x7F) << (7 * index);
        if byte & 0x80 == 0 {
            return Some((value, index + 1));
        }
    }
    None
}

/// One OBU of a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Obu {
    obu_type: u8,
    flags: u8,
    /// The whole OBU in the stream.
    range: Range<usize>,
    /// What follows the header and the size field.
    body: Range<usize>,
}

fn obus(stream: &[u8]) -> Result<Vec<Obu>, IamfError> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < stream.len() {
        let header = stream[pos];
        let (size, size_len) =
            leb128(&stream[pos + 1..]).ok_or(IamfError::Malformed("obu_size"))?;
        let body = pos + 1 + size_len;
        let end = usize::try_from(size)
            .ok()
            .and_then(|size| body.checked_add(size))
            .filter(|&end| end <= stream.len())
            .ok_or(IamfError::Malformed("an OBU runs past the end"))?;
        out.push(Obu {
            obu_type: header >> 3,
            flags: header & 0x07,
            range: pos..end,
            body: body..end,
        });
        pos = end;
    }
    Ok(out)
}

/// The substream an audio frame OBU belongs to.
fn substream(stream: &[u8], obu: &Obu) -> Result<u64, IamfError> {
    if obu.obu_type != OBU_AUDIO_FRAME {
        return Ok(u64::from(obu.obu_type - OBU_AUDIO_FRAME_ID0));
    }
    let malformed = IamfError::Malformed("audio frame header");
    let mut body = &stream[obu.body.clone()];
    if obu.flags & TRIMMING_STATUS != 0 {
        for _ in 0..2 {
            let (_, len) = leb128(body).ok_or(malformed.clone())?;
            body = &body[len..];
        }
    }
    if obu.flags & EXTENSION != 0 {
        let (size, len) = leb128(body).ok_or(malformed.clone())?;
        body = body.get(len + size as usize..).ok_or(malformed.clone())?;
    }
    leb128(body).map(|(id, _)| id).ok_or(malformed)
}

/// What a codec config states about a temporal unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnitTiming {
    /// `num_samples_per_frame`: every unit of the sequence lasts this.
    pub samples: u32,
    /// The codec's sample rate, when the config states it: Opus is 48 kHz,
    /// LPCM and FLAC say theirs, AAC's is not read here.
    pub sample_rate: Option<u32>,
}

fn unit_timing(body: &[u8]) -> Option<UnitTiming> {
    let (_, id_len) = leb128(body)?;
    let codec_id = body.get(id_len..id_len + 4)?;
    let (samples, samples_len) = leb128(&body[id_len + 4..])?;
    // audio_roll_distance, then the codec's own config.
    let config = body.get(id_len + 4 + samples_len + 2..)?;
    let sample_rate = match codec_id {
        b"Opus" => Some(48_000),
        // sample_format_flags, sample_size, sample_rate.
        b"ipcm" => config
            .get(2..6)
            .map(|rate| u32::from_be_bytes([rate[0], rate[1], rate[2], rate[3]])),
        // A METADATA_BLOCK header, then STREAMINFO: block and frame sizes
        // (10 bytes), then the rate on 20 bits.
        b"fLaC" => config.get(14..17).map(|rate| {
            (u32::from(rate[0]) << 12) | (u32::from(rate[1]) << 4) | u32::from(rate[2] >> 4)
        }),
        _ => None,
    };
    Some(UnitTiming {
        samples: u32::try_from(samples).ok()?,
        sample_rate,
    })
}

/// An IA sequence cut for the carrier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IamfSequence {
    /// The descriptor OBUs, as the stream has them.
    pub descriptors: Vec<u8>,
    /// Each temporal unit's OBUs, in the stream. Temporal delimiters are
    /// left out: a burst is the unit.
    pub units: Vec<Vec<u8>>,
    pub timing: UnitTiming,
}

impl IamfSequence {
    /// Cut a standalone IAMF stream (IAMF §3): one sequence, its
    /// descriptors first.
    ///
    /// A unit ends at a temporal delimiter. A stream without them ends a
    /// unit where a substream gets its second audio frame; the parameter
    /// blocks just before that frame open the next unit.
    pub fn parse(stream: &[u8]) -> Result<Self, IamfError> {
        let obus = obus(stream)?;
        if obus.first().map(|obu| obu.obu_type) != Some(OBU_SEQUENCE_HEADER) {
            return Err(IamfError::NoSequenceHeader);
        }
        let mut descriptors = Vec::new();
        let mut timing = None;
        let mut units: Vec<Vec<u8>> = Vec::new();
        let mut unit: Vec<u8> = Vec::new();
        // Substreams heard in the unit being built, and where the parameter
        // blocks since its last audio frame start in it.
        let mut heard: Vec<u64> = Vec::new();
        let mut trailing_params: Option<usize> = None;
        let mut in_data = false;
        for (index, obu) in obus.iter().enumerate() {
            let bytes = &stream[obu.range.clone()];
            match obu.obu_type {
                OBU_SEQUENCE_HEADER if index == 0 => descriptors.extend_from_slice(bytes),
                // A repeat of the descriptors inside the stream: the
                // sender places its own.
                OBU_SEQUENCE_HEADER | OBU_CODEC_CONFIG..=OBU_MIX_PRESENTATION
                    if obu.flags & REDUNDANT_COPY != 0 && in_data => {}
                OBU_SEQUENCE_HEADER => return Err(IamfError::SeveralSequences),
                OBU_CODEC_CONFIG..=OBU_MIX_PRESENTATION if !in_data => {
                    if obu.obu_type == OBU_CODEC_CONFIG && timing.is_none() {
                        let mut body = &stream[obu.body.clone()];
                        if obu.flags & EXTENSION != 0 {
                            let (size, len) =
                                leb128(body).ok_or(IamfError::Malformed("extension header"))?;
                            body = body
                                .get(len + size as usize..)
                                .ok_or(IamfError::Malformed("extension header"))?;
                        }
                        timing =
                            Some(unit_timing(body).ok_or(IamfError::Malformed("codec config"))?);
                    }
                    descriptors.extend_from_slice(bytes);
                }
                OBU_CODEC_CONFIG..=OBU_MIX_PRESENTATION => {
                    return Err(IamfError::Malformed(
                        "a descriptor among the temporal units",
                    ));
                }
                OBU_TEMPORAL_DELIMITER => {
                    in_data = true;
                    if !unit.is_empty() {
                        units.push(std::mem::take(&mut unit));
                    }
                    heard.clear();
                    trailing_params = None;
                }
                OBU_AUDIO_FRAME..=OBU_AUDIO_FRAME_ID17 => {
                    in_data = true;
                    let id = substream(stream, obu)?;
                    if heard.contains(&id) {
                        let next = unit.split_off(trailing_params.unwrap_or(unit.len()));
                        units.push(std::mem::replace(&mut unit, next));
                        heard.clear();
                    }
                    heard.push(id);
                    trailing_params = None;
                    unit.extend_from_slice(bytes);
                }
                OBU_PARAMETER_BLOCK => {
                    in_data = true;
                    trailing_params.get_or_insert(unit.len());
                    unit.extend_from_slice(bytes);
                }
                // Metadata and reserved OBUs ride with the unit they are in.
                _ if in_data => unit.extend_from_slice(bytes),
                _ => descriptors.extend_from_slice(bytes),
            }
        }
        if !unit.is_empty() {
            units.push(unit);
        }
        Ok(Self {
            descriptors,
            units,
            timing: timing.ok_or(IamfError::NoCodecConfig)?,
        })
    }

    /// The descriptors as the copy a sender repeats: every OBU with
    /// `obu_redundant_copy` set.
    pub fn redundant_descriptors(&self) -> Vec<u8> {
        let mut copy = self.descriptors.clone();
        // Framed once already by `parse`.
        for obu in obus(&copy).unwrap_or_default() {
            copy[obu.range.start] |= REDUNDANT_COPY;
        }
        copy
    }
}

/// A PCM-shaped carrier: 16-bit words, `channels` to a frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Carrier {
    pub rate_hz: u32,
    pub channels: u32,
}

impl Carrier {
    /// Bytes one carrier frame takes.
    pub fn frame_bytes(self) -> usize {
        self.channels as usize * 2
    }

    /// The bytes one unit of `timing` lasts on this carrier: the burst
    /// repetition period. `sample_rate` stands in when the codec config does
    /// not state one.
    pub fn period_bytes(
        self,
        timing: UnitTiming,
        sample_rate: Option<u32>,
    ) -> Result<usize, IamfError> {
        let sample_rate = timing
            .sample_rate
            .or(sample_rate)
            .filter(|&rate| rate > 0)
            .ok_or(IamfError::UnknownSampleRate)?;
        let ticks = u64::from(timing.samples) * u64::from(self.rate_hz);
        if ticks == 0 || !ticks.is_multiple_of(u64::from(sample_rate)) {
            return Err(IamfError::FractionalPeriod {
                unit_samples: timing.samples,
                sample_rate,
                carrier_rate: self.rate_hz,
            });
        }
        Ok((ticks / u64::from(sample_rate)) as usize * self.frame_bytes())
    }
}

/// Write `sequence` as a burst stream of `period_bytes` per unit: the
/// descriptors open the first burst, and a redundant copy opens every
/// `copy_every`-th one after it (0: never), so a receiver that joins late
/// locks within that many units.
pub fn write_bursts(
    out: &mut Vec<u8>,
    sequence: &IamfSequence,
    period_bytes: usize,
    copy_every: usize,
) -> Result<(), IamfError> {
    let copy = sequence.redundant_descriptors();
    let mut payload = Vec::new();
    for (index, unit) in sequence.units.iter().enumerate() {
        payload.clear();
        if index == 0 {
            payload.extend_from_slice(&sequence.descriptors);
        } else if copy_every != 0 && index % copy_every == 0 {
            payload.extend_from_slice(&copy);
        }
        payload.extend_from_slice(unit);
        write_iamf_burst(out, &payload, period_bytes)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::SpdifParser;

    fn obu(obu_type: u8, flags: u8, body: &[u8]) -> Vec<u8> {
        assert!(body.len() < 128);
        let mut out = vec![(obu_type << 3) | flags, body.len() as u8];
        out.extend_from_slice(body);
        out
    }

    fn header() -> Vec<u8> {
        obu(OBU_SEQUENCE_HEADER, 0, b"iamf\x00\x00")
    }

    /// Codec config 7, Opus, 960 samples a frame, roll -4, an Opus header.
    fn opus_config() -> Vec<u8> {
        let mut body = vec![7];
        body.extend_from_slice(b"Opus");
        body.extend_from_slice(&[0xC0, 0x07]); // 960
        body.extend_from_slice(&(-4i16).to_be_bytes());
        body.extend_from_slice(&[0; 11]);
        obu(OBU_CODEC_CONFIG, 0, &body)
    }

    fn lpcm_config(samples: u8, rate: u32) -> Vec<u8> {
        let mut body = vec![1];
        body.extend_from_slice(b"ipcm");
        body.push(samples);
        body.extend_from_slice(&0i16.to_be_bytes());
        body.extend_from_slice(&[1, 16]);
        body.extend_from_slice(&rate.to_be_bytes());
        obu(OBU_CODEC_CONFIG, 0, &body)
    }

    fn element() -> Vec<u8> {
        obu(1, 0, &[42, 0, 0, 7])
    }

    fn frame(id: u8, fill: u8) -> Vec<u8> {
        obu(OBU_AUDIO_FRAME_ID0 + id, 0, &[fill; 5])
    }

    fn param(fill: u8) -> Vec<u8> {
        obu(OBU_PARAMETER_BLOCK, 0, &[fill; 3])
    }

    fn delimiter() -> Vec<u8> {
        obu(OBU_TEMPORAL_DELIMITER, 0, &[])
    }

    fn descriptors() -> Vec<u8> {
        [header(), opus_config(), element()].concat()
    }

    #[test]
    fn units_end_at_temporal_delimiters() {
        let units = [
            [param(1), frame(0, 1), frame(1, 1)].concat(),
            [frame(0, 2), frame(1, 2)].concat(),
            [param(3), param(3), frame(0, 3), frame(1, 3)].concat(),
        ];
        let mut stream = descriptors();
        for unit in &units {
            stream.extend(delimiter());
            stream.extend_from_slice(unit);
        }
        let sequence = IamfSequence::parse(&stream).unwrap();
        assert_eq!(sequence.descriptors, descriptors());
        assert_eq!(sequence.units, units);
        assert_eq!(
            sequence.timing,
            UnitTiming {
                samples: 960,
                sample_rate: Some(48_000)
            }
        );
    }

    /// Without delimiters a unit ends where a substream comes round again,
    /// and the parameter blocks before that frame belong to the next unit.
    #[test]
    fn units_end_where_a_substream_comes_round() {
        let units = [
            [param(1), frame(0, 1), frame(1, 1)].concat(),
            [frame(0, 2), param(2), frame(1, 2)].concat(),
            [param(3), param(3), frame(0, 3), frame(1, 3)].concat(),
            [frame(0, 4), frame(1, 4)].concat(),
        ];
        let stream = [descriptors(), units.concat()].concat();
        assert_eq!(IamfSequence::parse(&stream).unwrap().units, units);
    }

    /// An audio frame OBU of type 5 names its substream after its trimming
    /// fields.
    #[test]
    fn an_explicit_substream_id_is_read_past_the_trimming() {
        let explicit =
            |id: u8, fill: u8| obu(OBU_AUDIO_FRAME, TRIMMING_STATUS, &[0, 0x90, 0x01, id, fill]);
        let units = [
            [explicit(40, 1), explicit(41, 1)].concat(),
            [explicit(40, 2), explicit(41, 2)].concat(),
        ];
        let stream = [descriptors(), units.concat()].concat();
        assert_eq!(IamfSequence::parse(&stream).unwrap().units, units);
    }

    #[test]
    fn the_codec_config_states_the_unit() {
        let stream = [header(), lpcm_config(64, 44_100), frame(0, 0)].concat();
        let timing = IamfSequence::parse(&stream).unwrap().timing;
        assert_eq!(
            timing,
            UnitTiming {
                samples: 64,
                sample_rate: Some(44_100)
            }
        );
        // 64 samples at 44.1 kHz are not whole frames at 48 kHz.
        let carrier = Carrier {
            rate_hz: 48_000,
            channels: 8,
        };
        assert!(matches!(
            carrier.period_bytes(timing, None),
            Err(IamfError::FractionalPeriod { .. })
        ));
        // A 20 ms Opus unit on 8 channels at 48 kHz: 960 frames of 16 bytes.
        let opus = UnitTiming {
            samples: 960,
            sample_rate: Some(48_000),
        };
        assert_eq!(carrier.period_bytes(opus, None), Ok(15_360));
        // On the classic 4x carrier it is 3840 frames.
        let quad = Carrier {
            rate_hz: 192_000,
            channels: 2,
        };
        assert_eq!(quad.period_bytes(opus, None), Ok(15_360));
        let unstated = UnitTiming {
            samples: 1024,
            sample_rate: None,
        };
        assert_eq!(
            carrier.period_bytes(unstated, None),
            Err(IamfError::UnknownSampleRate)
        );
        assert_eq!(carrier.period_bytes(unstated, Some(48_000)), Ok(16_384));
    }

    #[test]
    fn what_is_not_one_sequence_is_refused() {
        assert_eq!(
            IamfSequence::parse(&frame(0, 0)),
            Err(IamfError::NoSequenceHeader)
        );
        assert_eq!(
            IamfSequence::parse(&[header(), element(), frame(0, 0)].concat()),
            Err(IamfError::NoCodecConfig)
        );
        let twice = [descriptors(), frame(0, 0), descriptors(), frame(0, 1)].concat();
        assert_eq!(
            IamfSequence::parse(&twice),
            Err(IamfError::SeveralSequences)
        );
        let mut cut = descriptors();
        cut.extend_from_slice(&[OBU_AUDIO_FRAME_ID0 << 3, 9, 1, 2]);
        assert!(matches!(
            IamfSequence::parse(&cut),
            Err(IamfError::Malformed(_))
        ));
    }

    /// The stream a receiver reads back: the descriptors once, a flagged
    /// copy on schedule, every unit whole and on its period.
    #[test]
    fn the_burst_stream_carries_every_unit_on_its_period() {
        let units: Vec<Vec<u8>> = (0..7u8)
            .map(|n| [frame(0, n), frame(1, n)].concat())
            .collect();
        let stream = [descriptors(), units.concat()].concat();
        let sequence = IamfSequence::parse(&stream).unwrap();
        let carrier = Carrier {
            rate_hz: 48_000,
            channels: 2,
        };
        let period = carrier.period_bytes(sequence.timing, None).unwrap();
        assert_eq!(period, 3840);
        let mut carried = Vec::new();
        write_bursts(&mut carried, &sequence, period, 3).unwrap();
        assert_eq!(carried.len(), units.len() * period);

        let mut parser = SpdifParser::new();
        parser.push_bytes(&carried);
        let copy = sequence.redundant_descriptors();
        assert_eq!(copy.len(), sequence.descriptors.len());
        assert_ne!(copy, sequence.descriptors);
        for (index, unit) in units.iter().enumerate() {
            let burst = parser.get_next_packet().unwrap();
            assert_eq!(burst.start_byte, (index * period) as u64);
            let lead: &[u8] = match index {
                0 => &sequence.descriptors,
                3 | 6 => &copy,
                _ => &[],
            };
            assert_eq!(burst.payload, [lead, unit].concat(), "burst {index}");
        }
        assert_eq!(parser.get_next_packet(), None);
    }

    /// A redundant copy the stream itself repeats is the sender's to place:
    /// it is not carried twice.
    #[test]
    fn copies_inside_the_stream_are_left_out() {
        let mut copy = descriptors();
        for obu in obus(&copy.clone()).unwrap() {
            copy[obu.range.start] |= REDUNDANT_COPY;
        }
        let stream = [descriptors(), frame(0, 1), copy, frame(0, 2)].concat();
        let sequence = IamfSequence::parse(&stream).unwrap();
        assert_eq!(sequence.units, [frame(0, 1), frame(0, 2)]);
    }
}
