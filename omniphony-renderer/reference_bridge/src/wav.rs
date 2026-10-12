//! Minimal streaming RIFF/WAVE header parser plus PCM sample conversion.
//!
//! The bridge receives the input file as a sequence of raw byte chunks, so the
//! header may straddle several `push_packet` calls. [`parse_header`] therefore
//! works on the accumulated buffer and reports [`HeaderParse::NeedMore`] until
//! enough bytes are present to locate the `fmt ` chunk and the start of the
//! `data` chunk payload.
//!
//! Samples are converted to the renderer's internal PCM convention: signed
//! integers scaled to 24 bits in an `i32` (full scale = `2^23`). This matches
//! `orender_engine::render::fill_pcm_f32_drc`, which divides decoded PCM by
//! `2^23` to obtain `f32`. (The `bridge_api` doc comment calls this "full-scale
//! i32", but the host actually treats decoded samples as 24-bit-scaled, exactly
//! as the production decoder bridge emits them.)

/// Largest chunk the header parser buffers before the `data` chunk (`fmt `,
/// `LIST`, `bext`, `iXML`, `JUNK`… are kilobytes). Bounds the memory a header
/// can make the bridge hold, on a 32-bit target too.
const MAX_HEADER_CHUNK_BYTES: usize = 16 * 1024 * 1024;

/// Full-scale magnitude of the renderer's 24-bit-in-`i32` PCM convention.
const PCM_FULL_SCALE: f32 = 8_388_607.0; // 2^23 - 1

/// Concrete sample encoding resolved from the `fmt ` chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SampleFormat {
    PcmI16,
    PcmI24,
    PcmI32,
    F32,
}

impl SampleFormat {
    /// Bytes occupied by one sample of this format.
    pub(crate) fn bytes_per_sample(self) -> usize {
        match self {
            SampleFormat::PcmI16 => 2,
            SampleFormat::PcmI24 => 3,
            SampleFormat::PcmI32 | SampleFormat::F32 => 4,
        }
    }

    /// Convert one little-endian sample (`bytes.len() == bytes_per_sample`) to
    /// the renderer's 24-bit-scaled `i32` convention.
    #[inline]
    pub(crate) fn decode_sample(self, bytes: &[u8]) -> i32 {
        match self {
            // i16 full scale (2^15) maps to 24-bit full scale (2^23): << 8.
            SampleFormat::PcmI16 => (i16::from_le_bytes([bytes[0], bytes[1]]) as i32) << 8,
            // Already 24-bit; sign-extend the 3 little-endian bytes.
            SampleFormat::PcmI24 => {
                let raw = (bytes[0] as i32) | ((bytes[1] as i32) << 8) | ((bytes[2] as i32) << 16);
                (raw << 8) >> 8 // sign-extend from bit 23
            }
            // i32 full scale (2^31) maps to 24-bit full scale (2^23): >> 8.
            SampleFormat::PcmI32 => {
                i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) >> 8
            }
            SampleFormat::F32 => {
                let v = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                if !v.is_finite() {
                    0
                } else {
                    (v.clamp(-1.0, 1.0) * PCM_FULL_SCALE) as i32
                }
            }
        }
    }
}

/// Audio format parsed from a `fmt ` chunk.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WavFormat {
    pub(crate) channels: u16,
    pub(crate) sample_rate: u32,
    pub(crate) sample_format: SampleFormat,
    /// `dwChannelMask` of a `WAVE_FORMAT_EXTENSIBLE` header: which speaker
    /// positions the channels carry, in ascending bit order. 0 when the header
    /// is not extensible or declares no positions.
    pub(crate) channel_mask: u32,
}

impl WavFormat {
    /// Bytes occupied by one interleaved sample-frame (all channels).
    pub(crate) fn bytes_per_frame(&self) -> usize {
        self.sample_format.bytes_per_sample() * self.channels as usize
    }
}

/// Outcome of a header-parse attempt over the accumulated buffer.
pub(crate) enum HeaderParse {
    /// Not enough bytes buffered yet to locate `fmt ` + `data`.
    NeedMore,
    /// Header parsed. `data_offset` is the byte index in the buffer where PCM
    /// begins; `data_len` is the declared `data` chunk size in bytes
    /// (`u64::MAX` when the size is unknown / streamed to end-of-input).
    Found {
        format: WavFormat,
        data_offset: usize,
        data_len: u64,
    },
    /// The buffer does not begin with a valid RIFF/WAVE container.
    Invalid(&'static str),
}

fn read_u16(buf: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([buf[off], buf[off + 1]])
}

fn read_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
}

/// Resolve a `(format_tag, bits_per_sample)` pair to a [`SampleFormat`].
fn resolve_format(tag: u16, bits: u16) -> Result<SampleFormat, &'static str> {
    match (tag, bits) {
        (1, 16) => Ok(SampleFormat::PcmI16),
        (1, 24) => Ok(SampleFormat::PcmI24),
        (1, 32) => Ok(SampleFormat::PcmI32),
        (3, 32) => Ok(SampleFormat::F32),
        (1, _) => Err("unsupported PCM bit depth (need 16/24/32)"),
        (3, _) => Err("unsupported float bit depth (need 32)"),
        _ => Err("unsupported WAVE format tag (need 1, 3, or extensible)"),
    }
}

/// Parse the `fmt ` chunk body into a [`WavFormat`].
fn parse_fmt(body: &[u8]) -> Result<WavFormat, &'static str> {
    if body.len() < 16 {
        return Err("fmt chunk too short");
    }
    let mut tag = read_u16(body, 0);
    let channels = read_u16(body, 2);
    let sample_rate = read_u32(body, 4);
    let bits = read_u16(body, 14);
    let mut channel_mask = 0;

    // WAVE_FORMAT_EXTENSIBLE: the real format lives in the sub-format GUID
    // (its first two little-endian bytes are the effective format tag), and
    // `dwChannelMask` says which speaker each channel feeds.
    if tag == 0xFFFE {
        if body.len() < 40 {
            return Err("extensible fmt chunk too short");
        }
        channel_mask = read_u32(body, 20);
        tag = read_u16(body, 24);
    }

    if channels == 0 {
        return Err("zero channels");
    }
    if sample_rate == 0 {
        return Err("zero sample rate");
    }
    let sample_format = resolve_format(tag, bits)?;
    Ok(WavFormat {
        channels,
        sample_rate,
        sample_format,
        channel_mask,
    })
}

/// Attempt to parse the RIFF/WAVE header from the front of `buf`.
///
/// Walks the chunk list until the `data` chunk header is reached, parsing the
/// `fmt ` chunk along the way and skipping any others (`fact`, `LIST`, …).
pub(crate) fn parse_header(buf: &[u8]) -> HeaderParse {
    if buf.len() < 12 {
        return HeaderParse::NeedMore;
    }
    if &buf[0..4] != b"RIFF" || &buf[8..12] != b"WAVE" {
        return HeaderParse::Invalid("missing RIFF/WAVE signature");
    }

    let mut fmt: Option<WavFormat> = None;
    let mut off = 12usize;
    loop {
        if off + 8 > buf.len() {
            return HeaderParse::NeedMore;
        }
        let id = &buf[off..off + 4];
        let size = read_u32(buf, off + 4) as usize;
        let body_off = off + 8;

        if id == b"data" {
            return match fmt {
                Some(format) => HeaderParse::Found {
                    format,
                    data_offset: body_off,
                    // A size of 0 or 0xFFFFFFFF means "until end of input".
                    data_len: if size == 0 || size == 0xFFFF_FFFF {
                        u64::MAX
                    } else {
                        size as u64
                    },
                },
                None => HeaderParse::Invalid("data chunk before fmt chunk"),
            };
        }

        // Non-data chunk: its body must be fully buffered before we can skip
        // past it (or parse it, for `fmt `). A declared size past the cap is a
        // broken header, not a large one: waiting for it would buffer the whole
        // stream and never play it.
        if size > MAX_HEADER_CHUNK_BYTES {
            return HeaderParse::Invalid("a chunk before the audio is implausibly large");
        }
        if body_off + size > buf.len() {
            return HeaderParse::NeedMore;
        }
        if id == b"fmt " {
            match parse_fmt(&buf[body_off..body_off + size]) {
                Ok(f) => fmt = Some(f),
                Err(e) => return HeaderParse::Invalid(e),
            }
        }
        // Chunks are word-aligned: an odd size carries a trailing pad byte.
        off = body_off + size + (size & 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_i16_full_scale_maps_to_24bit() {
        assert_eq!(
            SampleFormat::PcmI16.decode_sample(&[0x00, 0x80]),
            -8_388_608
        ); // -2^23
        assert_eq!(SampleFormat::PcmI16.decode_sample(&[0xFF, 0x7F]), 0x7F_FF00); // ~+2^23
        assert_eq!(SampleFormat::PcmI16.decode_sample(&[0x00, 0x00]), 0);
    }

    #[test]
    fn decode_i24_passthrough_and_sign() {
        assert_eq!(
            SampleFormat::PcmI24.decode_sample(&[0x00, 0x00, 0x80]),
            -8_388_608
        );
        assert_eq!(
            SampleFormat::PcmI24.decode_sample(&[0xFF, 0xFF, 0x7F]),
            8_388_607
        );
        assert_eq!(SampleFormat::PcmI24.decode_sample(&[0x00, 0x00, 0x00]), 0);
    }

    #[test]
    fn decode_f32_clamps_and_scales() {
        assert_eq!(
            SampleFormat::F32.decode_sample(&1.0f32.to_le_bytes()),
            8_388_607
        );
        assert_eq!(
            SampleFormat::F32.decode_sample(&(-1.0f32).to_le_bytes()),
            -8_388_607
        );
        assert_eq!(
            SampleFormat::F32.decode_sample(&2.0f32.to_le_bytes()),
            8_388_607
        );
        assert_eq!(SampleFormat::F32.decode_sample(&0.0f32.to_le_bytes()), 0);
    }

    #[test]
    fn parse_minimal_header() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"RIFF");
        buf.extend_from_slice(&36u32.to_le_bytes());
        buf.extend_from_slice(b"WAVE");
        buf.extend_from_slice(b"fmt ");
        buf.extend_from_slice(&16u32.to_le_bytes());
        buf.extend_from_slice(&1u16.to_le_bytes()); // PCM
        buf.extend_from_slice(&6u16.to_le_bytes()); // channels
        buf.extend_from_slice(&48_000u32.to_le_bytes());
        buf.extend_from_slice(&(48_000u32 * 6 * 2).to_le_bytes());
        buf.extend_from_slice(&(6u16 * 2).to_le_bytes());
        buf.extend_from_slice(&16u16.to_le_bytes());
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&100u32.to_le_bytes());

        match parse_header(&buf) {
            HeaderParse::Found {
                format,
                data_offset,
                data_len,
            } => {
                assert_eq!(format.channels, 6);
                assert_eq!(format.sample_rate, 48_000);
                assert_eq!(format.sample_format, SampleFormat::PcmI16);
                assert_eq!(data_offset, buf.len());
                assert_eq!(data_len, 100);
            }
            _ => panic!("expected Found"),
        }
    }

    /// A RIFF/WAVE prefix followed by `chunks` (id, body), each padded to an
    /// even size, then a `data` chunk header declaring `data_len` bytes.
    fn header(chunks: &[(&[u8; 4], Vec<u8>)], data_len: u32) -> Vec<u8> {
        let mut buf = b"RIFF\0\0\0\0WAVE".to_vec();
        for (id, body) in chunks {
            buf.extend_from_slice(*id);
            buf.extend_from_slice(&(body.len() as u32).to_le_bytes());
            buf.extend_from_slice(body);
            if body.len() % 2 == 1 {
                buf.push(0);
            }
        }
        buf.extend_from_slice(b"data");
        buf.extend_from_slice(&data_len.to_le_bytes());
        buf
    }

    /// A `fmt ` body: 16 bytes, or 40 for `WAVE_FORMAT_EXTENSIBLE` (whose
    /// sub-format tag is `sub_tag`).
    fn fmt(tag: u16, channels: u16, rate: u32, bits: u16, sub_tag: u16) -> Vec<u8> {
        let align = channels * bits / 8;
        let mut f = Vec::new();
        f.extend_from_slice(&tag.to_le_bytes());
        f.extend_from_slice(&channels.to_le_bytes());
        f.extend_from_slice(&rate.to_le_bytes());
        f.extend_from_slice(&(rate * align as u32).to_le_bytes());
        f.extend_from_slice(&align.to_le_bytes());
        f.extend_from_slice(&bits.to_le_bytes());
        if tag == 0xFFFE {
            f.extend_from_slice(&22u16.to_le_bytes()); // cbSize
            f.extend_from_slice(&bits.to_le_bytes()); // valid bits
            f.extend_from_slice(&0x3Fu32.to_le_bytes()); // 5.1 mask
            f.extend_from_slice(&sub_tag.to_le_bytes());
            f.extend_from_slice(&[0; 14]); // rest of the GUID
        }
        f
    }

    fn invalid_reason(buf: &[u8]) -> &'static str {
        match parse_header(buf) {
            HeaderParse::Invalid(reason) => reason,
            HeaderParse::NeedMore => panic!("expected Invalid, got NeedMore"),
            HeaderParse::Found { .. } => panic!("expected Invalid, got Found"),
        }
    }

    #[test]
    fn a_malformed_header_is_refused_with_its_reason() {
        let pcm16 = fmt(1, 2, 48_000, 16, 0);
        assert_eq!(
            invalid_reason(b"RIFX\0\0\0\0WAVE"),
            "missing RIFF/WAVE signature"
        );
        assert_eq!(
            invalid_reason(b"RIFF\0\0\0\0AVI "),
            "missing RIFF/WAVE signature"
        );
        assert_eq!(
            invalid_reason(&header(&[], 8)),
            "data chunk before fmt chunk"
        );
        let cases: &[(Vec<u8>, &str)] = &[
            (pcm16[..12].to_vec(), "fmt chunk too short"),
            (
                fmt(0xFFFE, 2, 48_000, 16, 1)[..24].to_vec(),
                "extensible fmt chunk too short",
            ),
            (fmt(1, 0, 48_000, 16, 0), "zero channels"),
            (fmt(1, 2, 0, 16, 0), "zero sample rate"),
            (
                fmt(1, 2, 48_000, 8, 0),
                "unsupported PCM bit depth (need 16/24/32)",
            ),
            (
                fmt(3, 2, 48_000, 64, 0),
                "unsupported float bit depth (need 32)",
            ),
            (
                fmt(2, 2, 48_000, 16, 0),
                "unsupported WAVE format tag (need 1, 3, or extensible)",
            ),
            (
                fmt(0xFFFE, 2, 48_000, 16, 2),
                "unsupported WAVE format tag (need 1, 3, or extensible)",
            ),
        ];
        for (body, reason) in cases {
            assert_eq!(
                invalid_reason(&header(&[(b"fmt ", body.clone())], 8)),
                *reason
            );
        }
    }

    #[test]
    fn extensible_headers_resolve_their_sub_format_and_mask() {
        match parse_header(&header(&[(b"fmt ", fmt(0xFFFE, 6, 48_000, 24, 1))], 36)) {
            HeaderParse::Found { format, .. } => {
                assert_eq!(format.sample_format, SampleFormat::PcmI24);
                assert_eq!(format.channel_mask, 0x3F);
            }
            _ => panic!("expected Found"),
        }
    }

    #[test]
    fn chunks_before_the_audio_are_skipped_with_their_pad_byte() {
        let buf = header(
            &[
                (b"LIST", vec![1; 5]),
                (b"fmt ", fmt(3, 1, 44_100, 32, 0)),
                (b"JUNK", vec![2; 3]),
            ],
            0,
        );
        match parse_header(&buf) {
            HeaderParse::Found {
                format,
                data_offset,
                data_len,
            } => {
                assert_eq!(format.sample_format, SampleFormat::F32);
                assert_eq!(format.sample_rate, 44_100);
                assert_eq!(data_offset, buf.len());
                assert_eq!(data_len, u64::MAX, "a zero size streams to the end");
            }
            _ => panic!("expected Found"),
        }
    }

    #[test]
    fn a_header_can_arrive_in_pieces() {
        let buf = header(
            &[(b"LIST", vec![1; 9]), (b"fmt ", fmt(1, 2, 48_000, 16, 0))],
            u32::MAX,
        );
        for cut in 0..buf.len() {
            assert!(
                matches!(parse_header(&buf[..cut]), HeaderParse::NeedMore),
                "cut at {cut} of {}",
                buf.len()
            );
        }
        assert!(matches!(
            parse_header(&buf),
            HeaderParse::Found {
                data_len: u64::MAX,
                ..
            }
        ));
    }

    /// A chunk that declares more than the cap would have the bridge buffer
    /// the whole stream waiting for it: it is refused at once instead.
    #[test]
    fn an_implausibly_large_chunk_is_refused_not_waited_for() {
        let mut buf = header(&[(b"fmt ", fmt(1, 2, 48_000, 16, 0))], 0);
        buf.truncate(12);
        buf.extend_from_slice(b"LIST");
        buf.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            invalid_reason(&buf),
            "a chunk before the audio is implausibly large"
        );
        // At the cap it is still a chunk to wait for.
        buf.truncate(12);
        buf.extend_from_slice(b"LIST");
        buf.extend_from_slice(&(MAX_HEADER_CHUNK_BYTES as u32).to_le_bytes());
        assert!(matches!(parse_header(&buf), HeaderParse::NeedMore));
    }

    #[test]
    fn parse_header_needs_more_when_truncated() {
        assert!(matches!(parse_header(b"RIF"), HeaderParse::NeedMore));
        assert!(matches!(
            parse_header(b"RIFF\x24\x00\x00\x00WAVE"),
            HeaderParse::NeedMore
        ));
    }
}
