//! Writing IEC 61937 bursts, for the one data type Omniphony frames itself:
//! the private IAMF type of `docs/iamf-transport-plan.md`.
//!
//! The IEC assigned IAMF no data type. This one is private to a sender and a
//! receiver that are both ours; no other equipment knows it.
//!
//! A burst is the preamble (Pa, Pb, Pc, Pd as 16-bit words), the payload, and
//! zeros up to the repetition period. On the wire the stream is 16-bit
//! little-endian words, as the parser reads it, and a payload byte keeps its
//! place: byte `i` of the payload is byte `8 + i` of the burst. A payload of
//! odd length is followed by a zero the length does not count.

use crate::parser::{SYNCWORD_PA, SYNCWORD_PB};

/// The private IEC 61937 data type (`Pc` bits 0-4) of an IAMF burst.
///
/// Types 0x1B to 0x1E are not assigned by IEC 61937-2 as far as this project
/// could establish; 0x1F is its extension escape and must not be used. The
/// bridge states the same value (`bridge-family-iamf`, `iec61937`).
pub const DATA_TYPE_IAMF_PRIVATE: u8 = 0x1D;

/// Pa, Pb, Pc and Pd.
pub const BURST_HEADER_BYTES: usize = 8;

/// The longest IAMF payload a burst states: `Pd` holds the low 16 bits of the
/// byte count and `Pc` bits 8-12, which IEC 61937 leaves to the data type,
/// the next five. A temporal unit of FLAC or LPCM on many channels does not
/// fit the 65 535 bytes `Pd` alone counts.
pub const IAMF_MAX_PAYLOAD_BYTES: usize = (1 << 21) - 1;

/// The payload length an IAMF burst header states, from the high byte of
/// `Pc` and `Pd`.
#[inline]
pub fn iamf_payload_bytes(pc_high: u8, pd: u16) -> usize {
    (usize::from(pc_high & 0x1F) << 16) | usize::from(pd)
}

/// Why a payload cannot be written as a burst.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurstError {
    /// The payload is longer than a burst can state.
    PayloadTooLong { payload: usize },
    /// The header and the payload do not fit the repetition period: the
    /// carrier is too narrow for this stream.
    PeriodTooShort { needed: usize, period: usize },
    /// A period is a whole number of 16-bit words.
    OddPeriod { period: usize },
}

impl std::fmt::Display for BurstError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {
            Self::PayloadTooLong { payload } => write!(
                f,
                "a {payload}-byte payload is past the {IAMF_MAX_PAYLOAD_BYTES} bytes a burst states"
            ),
            Self::PeriodTooShort { needed, period } => write!(
                f,
                "a burst of {needed} bytes does not fit its {period}-byte period: the carrier needs more channels"
            ),
            Self::OddPeriod { period } => {
                write!(f, "a period of {period} bytes is not whole 16-bit words")
            }
        }
    }
}

impl std::error::Error for BurstError {}

/// Append one IAMF burst to `out`: the preamble, `payload`, and zeros up to
/// `period_bytes` from the preamble.
pub fn write_iamf_burst(
    out: &mut Vec<u8>,
    payload: &[u8],
    period_bytes: usize,
) -> Result<(), BurstError> {
    if payload.len() > IAMF_MAX_PAYLOAD_BYTES {
        return Err(BurstError::PayloadTooLong {
            payload: payload.len(),
        });
    }
    if !period_bytes.is_multiple_of(2) {
        return Err(BurstError::OddPeriod {
            period: period_bytes,
        });
    }
    let needed = BURST_HEADER_BYTES + payload.len();
    if needed > period_bytes {
        return Err(BurstError::PeriodTooShort {
            needed,
            period: period_bytes,
        });
    }
    let pc_high = ((payload.len() >> 16) & 0x1F) as u8;
    let pd = (payload.len() & 0xFFFF) as u16;
    let start = out.len();
    out.extend_from_slice(&SYNCWORD_PA.to_le_bytes());
    out.extend_from_slice(&SYNCWORD_PB.to_le_bytes());
    out.extend_from_slice(&[DATA_TYPE_IAMF_PRIVATE, pc_high]);
    out.extend_from_slice(&pd.to_le_bytes());
    out.extend_from_slice(payload);
    out.resize(start + period_bytes, 0);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::SpdifParser;

    fn parse(stream: &[u8], chunk: usize) -> Vec<(u8, Vec<u8>, u64)> {
        let mut parser = SpdifParser::new();
        let mut out = Vec::new();
        for piece in stream.chunks(chunk) {
            parser.push_bytes(piece);
            while let Some(packet) = parser.get_next_packet() {
                out.push((packet.data_type, packet.payload, packet.start_byte));
            }
        }
        out
    }

    #[test]
    fn a_burst_reads_back_at_its_period() {
        let payloads: [Vec<u8>; 3] = [
            (0..200u8).collect(),
            vec![0xAB; 3],
            (0..=255u8).cycle().take(1000).collect(),
        ];
        let period = 1024;
        let mut stream = Vec::new();
        for payload in &payloads {
            write_iamf_burst(&mut stream, payload, period).unwrap();
        }
        assert_eq!(stream.len(), payloads.len() * period);
        for chunk in [1, 7, 4096] {
            let bursts = parse(&stream, chunk);
            assert_eq!(bursts.len(), payloads.len());
            for (index, (data_type, payload, start)) in bursts.iter().enumerate() {
                assert_eq!(*data_type, DATA_TYPE_IAMF_PRIVATE);
                assert_eq!(payload, &payloads[index]);
                assert_eq!(*start, (index * period) as u64);
            }
        }
    }

    /// Past the 65 535 bytes `Pd` counts, the length goes on in `Pc`.
    #[test]
    fn a_long_payload_states_its_length_over_pc_and_pd() {
        let payload: Vec<u8> = (0..100_001u32).map(|n| (n % 251) as u8).collect();
        let period = 131_072;
        let mut stream = Vec::new();
        write_iamf_burst(&mut stream, &payload, period).unwrap();
        write_iamf_burst(&mut stream, &[1, 2, 3, 4], period).unwrap();
        assert_eq!(stream[5], 0x01);
        assert_eq!(iamf_payload_bytes(stream[5], 0x86A1), 100_001);
        let bursts = parse(&stream, 5000);
        assert_eq!(bursts.len(), 2);
        assert_eq!(bursts[0].1, payload);
        assert_eq!(bursts[1].1, [1, 2, 3, 4]);
        assert_eq!(bursts[1].2, period as u64);
    }

    /// A payload that happens to hold the preamble's bytes is not a burst:
    /// the parser reads the stated length through, then looks for the next.
    #[test]
    fn a_preamble_inside_a_payload_is_payload() {
        let mut payload = vec![0x11; 10];
        payload.extend_from_slice(&crate::parser::SYNC_BYTES);
        payload.extend_from_slice(&[0x16, 0x00, 0x04, 0x00, 1, 2, 3, 4]);
        let mut stream = Vec::new();
        write_iamf_burst(&mut stream, &payload, 64).unwrap();
        let bursts = parse(&stream, 64);
        assert_eq!(bursts.len(), 1);
        assert_eq!(bursts[0].1, payload);
    }

    #[test]
    fn what_does_not_fit_is_refused() {
        let mut out = Vec::new();
        assert_eq!(
            write_iamf_burst(&mut out, &[0; 100], 104),
            Err(BurstError::PeriodTooShort {
                needed: 108,
                period: 104
            })
        );
        assert_eq!(
            write_iamf_burst(&mut out, &[0; 4], 63),
            Err(BurstError::OddPeriod { period: 63 })
        );
        let long = vec![0; IAMF_MAX_PAYLOAD_BYTES + 1];
        assert_eq!(
            write_iamf_burst(&mut out, &long, 1 << 22),
            Err(BurstError::PayloadTooLong {
                payload: long.len()
            })
        );
        assert!(out.is_empty());
        assert!(write_iamf_burst(&mut out, &[0; 100], 108).is_ok());
    }
}
