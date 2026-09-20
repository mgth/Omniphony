//! File / FIFO / stdout audio sink.
//!
//! Writes the renderer's interleaved `f32` output to stdout, a named pipe
//! (FIFO) or a regular file, in one of two formats:
//!   - [`FileSinkFormat::RawF32`]: headerless interleaved 32-bit float,
//!     little-endian — the lingua franca, consumable by
//!     `ffmpeg -f f32le -ar <sr> -ac <n>`.
//!   - [`FileSinkFormat::Caf`]: a streaming Core Audio Format (CAF) container
//!     with LPCM 32-bit float samples and a data chunk of *unknown* size
//!     (`-1`), so it streams to a pipe without ever seeking back to patch a
//!     size. The header layout mirrors truehdd's CAF writer (so files stay
//!     interchangeable) minus the seek-based size patching it does on close.
//!
//! This is a **non-realtime** sink: there is no device clock, no adaptive
//! resampling and no pacer. Flow control comes from blocking writes — when the
//! downstream consumer (e.g. a network relay) stalls, the pipe fills and the
//! write blocks, pacing the producer. A consumer that goes away instead of
//! stalling makes the write fail with `BrokenPipe`; that is the end of the
//! output rather than an error, and the decode layer ends the run on it.

use std::fs::OpenOptions;
use std::io::{self, BufWriter, Write};

/// Output container/format for [`FileAudioWriter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSinkFormat {
    /// Headerless interleaved f32 little-endian PCM.
    RawF32,
    /// Streaming CAF (LPCM 32-bit float, data chunk size `-1`).
    Caf,
}

/// One CAF channel description, carrying the speaker's spatial position so the
/// container is self-describing for arbitrary (non-standard) layouts that have
/// no standard `ChannelLayoutTag`.
#[derive(Debug, Clone, Copy)]
pub struct CafChannelDesc {
    /// `mChannelLabel` — we use [`Self::LABEL_USE_COORDINATES`].
    pub channel_label: u32,
    /// `mChannelFlags` — we set spherical + metres.
    pub channel_flags: u32,
    /// `mCoordinates` — for spherical: `(azimuth_deg, elevation_deg, distance)`.
    pub coordinates: [f32; 3],
}

impl CafChannelDesc {
    /// CAF `mChannelLabel` value meaning "described solely by mCoordinates".
    pub const LABEL_USE_COORDINATES: u32 = 100;
    /// `kAudioChannelFlags_SphericalCoordinates`.
    pub const FLAG_SPHERICAL: u32 = 1 << 1;
    /// `kAudioChannelFlags_Meters` — the third coordinate is in metres.
    pub const FLAG_METERS: u32 = 1 << 2;

    /// Describe a channel by its spherical position. Azimuth/elevation are in
    /// degrees, distance in metres — matching the renderer's `Speaker` fields.
    pub fn spherical(azimuth_deg: f32, elevation_deg: f32, distance_m: f32) -> Self {
        Self {
            channel_label: Self::LABEL_USE_COORDINATES,
            channel_flags: Self::FLAG_SPHERICAL | Self::FLAG_METERS,
            coordinates: [azimuth_deg, elevation_deg, distance_m],
        }
    }
}

/// Streaming writer for the file/FIFO/stdout output backend.
pub struct FileAudioWriter {
    inner: BufWriter<Box<dyn Write + Send>>,
    /// Reused per write so steady-state writing does not allocate.
    byte_scratch: Vec<u8>,
}

impl FileAudioWriter {
    const BUF_CAPACITY: usize = 64 * 1024;

    /// Open `destination` (`"-"` for stdout, otherwise a regular file or FIFO
    /// path) and, for CAF, write the header immediately.
    pub fn new(
        destination: &str,
        format: FileSinkFormat,
        sample_rate: u32,
        channel_count: u32,
        channel_descs: Option<Vec<CafChannelDesc>>,
    ) -> io::Result<Self> {
        let sink: Box<dyn Write + Send> = if destination == "-" {
            Box::new(io::stdout())
        } else {
            // A FIFO opens like a regular file and blocks until a reader
            // attaches — that is the intended backpressure. `truncate` is a
            // no-op on FIFOs and gives a fresh capture for regular files.
            Box::new(
                OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(destination)?,
            )
        };
        Self::with_writer(sink, format, sample_rate, channel_count, channel_descs)
    }

    /// Construct over an arbitrary writer. Used by tests with an in-memory
    /// buffer; production code uses [`Self::new`].
    pub fn with_writer(
        sink: Box<dyn Write + Send>,
        format: FileSinkFormat,
        sample_rate: u32,
        channel_count: u32,
        channel_descs: Option<Vec<CafChannelDesc>>,
    ) -> io::Result<Self> {
        let mut inner = BufWriter::with_capacity(Self::BUF_CAPACITY, sink);
        if format == FileSinkFormat::Caf {
            write_caf_header(
                &mut inner,
                sample_rate,
                channel_count,
                channel_descs.as_deref(),
            )?;
        }
        Ok(Self {
            inner,
            byte_scratch: Vec::new(),
        })
    }

    /// Append one block of interleaved f32 samples as little-endian bytes.
    pub fn write_samples(&mut self, interleaved_f32: &[f32]) -> io::Result<()> {
        push_f32_le(&mut self.byte_scratch, interleaved_f32);
        self.inner.write_all(&self.byte_scratch)
    }

    /// Flush buffered bytes all the way to the destination.
    pub fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Encode `samples` as little-endian f32 into `buf` (cleared first).
fn push_f32_le(buf: &mut Vec<u8>, samples: &[f32]) {
    buf.clear();
    buf.reserve(samples.len() * 4);
    for &s in samples {
        buf.extend_from_slice(&s.to_le_bytes());
    }
}

/// Write a streaming CAF header (file header + `desc` + optional `chan` +
/// `data` with size `-1`). All metadata fields are big-endian; the sample
/// payload that follows is little-endian f32 (per the `desc` format flags).
fn write_caf_header(
    w: &mut impl Write,
    sample_rate: u32,
    channel_count: u32,
    channel_descs: Option<&[CafChannelDesc]>,
) -> io::Result<()> {
    // File header: "caff", version = 1, flags = 0.
    w.write_all(b"caff")?;
    w.write_all(&1u16.to_be_bytes())?;
    w.write_all(&0u16.to_be_bytes())?;

    // Audio Description ("desc") chunk — 32-byte body.
    w.write_all(b"desc")?;
    w.write_all(&32u64.to_be_bytes())?;
    w.write_all(&(sample_rate as f64).to_be_bytes())?;
    // format_id "lpcm": stored as the four ASCII bytes (== u32 big-endian).
    w.write_all(b"lpcm")?;
    // format_flags: bit0 = is_float, bit1 = is_little_endian.
    w.write_all(&0b11u32.to_be_bytes())?;
    w.write_all(&(4 * channel_count).to_be_bytes())?; // bytes_per_packet
    w.write_all(&1u32.to_be_bytes())?; // frames_per_packet
    w.write_all(&channel_count.to_be_bytes())?; // channels_per_frame
    w.write_all(&32u32.to_be_bytes())?; // bits_per_channel

    // Channel Layout ("chan") chunk — self-describes arbitrary spatial layouts
    // via per-channel coordinates. Players that don't read it ignore it.
    if let Some(descs) = channel_descs {
        if !descs.is_empty() {
            // body = layout_tag(4) + bitmap(4) + count(4) + descs * (4+4+12)
            let body_len = 12 + descs.len() * 20;
            w.write_all(b"chan")?;
            w.write_all(&(body_len as u64).to_be_bytes())?;
            w.write_all(&0u32.to_be_bytes())?; // UseChannelDescriptions
            w.write_all(&0u32.to_be_bytes())?; // channel_bitmap (unused)
            w.write_all(&(descs.len() as u32).to_be_bytes())?; // mNumberChannelDescriptions
            for d in descs {
                w.write_all(&d.channel_label.to_be_bytes())?;
                w.write_all(&d.channel_flags.to_be_bytes())?;
                for c in d.coordinates {
                    w.write_all(&c.to_be_bytes())?;
                }
            }
        }
    }

    // Data ("data") chunk: size = -1 ("until EOF" → streamable, no seek to
    // patch), edit_count = 0. The interleaved f32 LE payload follows.
    w.write_all(b"data")?;
    w.write_all(&(-1i64).to_be_bytes())?;
    w.write_all(&0u32.to_be_bytes())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_u32_be(buf: &[u8], at: usize) -> u32 {
        u32::from_be_bytes(buf[at..at + 4].try_into().unwrap())
    }
    fn read_u64_be(buf: &[u8], at: usize) -> u64 {
        u64::from_be_bytes(buf[at..at + 8].try_into().unwrap())
    }
    fn read_i64_be(buf: &[u8], at: usize) -> i64 {
        i64::from_be_bytes(buf[at..at + 8].try_into().unwrap())
    }

    #[test]
    fn caf_header_fields_are_correct() {
        let ch = 16u32;
        let descs: Vec<CafChannelDesc> = (0..ch)
            .map(|i| CafChannelDesc::spherical(i as f32 * 10.0, 0.0, 1.0))
            .collect();
        let mut buf = Vec::new();
        write_caf_header(&mut buf, 48_000, ch, Some(&descs)).unwrap();

        // File header.
        assert_eq!(&buf[0..4], b"caff");
        assert_eq!(u16::from_be_bytes([buf[4], buf[5]]), 1);
        assert_eq!(u16::from_be_bytes([buf[6], buf[7]]), 0);

        // desc chunk at offset 8.
        assert_eq!(&buf[8..12], b"desc");
        assert_eq!(read_u64_be(&buf, 12), 32);
        assert_eq!(
            f64::from_be_bytes(buf[20..28].try_into().unwrap()),
            48_000.0
        );
        assert_eq!(&buf[28..32], b"lpcm");
        assert_eq!(read_u32_be(&buf, 32), 0b11); // float | little-endian
        assert_eq!(read_u32_be(&buf, 36), 4 * ch); // bytes_per_packet
        assert_eq!(read_u32_be(&buf, 40), 1); // frames_per_packet
        assert_eq!(read_u32_be(&buf, 44), ch); // channels_per_frame
        assert_eq!(read_u32_be(&buf, 48), 32); // bits_per_channel

        // chan chunk at offset 52.
        assert_eq!(&buf[52..56], b"chan");
        let chan_body = read_u64_be(&buf, 56) as usize;
        assert_eq!(chan_body, 12 + ch as usize * 20);
        assert_eq!(read_u32_be(&buf, 64), 0); // UseChannelDescriptions
        assert_eq!(read_u32_be(&buf, 72), ch); // mNumberChannelDescriptions
        // First channel description.
        assert_eq!(read_u32_be(&buf, 76), CafChannelDesc::LABEL_USE_COORDINATES);

        // data chunk follows the chan chunk.
        let data_off = 64 + chan_body;
        assert_eq!(&buf[data_off..data_off + 4], b"data");
        assert_eq!(read_i64_be(&buf, data_off + 4), -1); // streamable, never patched
        assert_eq!(read_u32_be(&buf, data_off + 12), 0); // edit_count
    }

    #[test]
    fn caf_header_omits_chan_without_descs() {
        let mut buf = Vec::new();
        write_caf_header(&mut buf, 48_000, 2, None).unwrap();
        // desc body (8 + desc chunk) ends at 52, then straight to data.
        assert_eq!(&buf[52..56], b"data");
    }

    #[test]
    fn raw_f32_le_encoding() {
        let mut buf = Vec::new();
        push_f32_le(&mut buf, &[1.0f32, -1.0, 0.5]);
        assert_eq!(buf.len(), 12);
        assert_eq!(&buf[0..4], &1.0f32.to_le_bytes());
        assert_eq!(&buf[4..8], &(-1.0f32).to_le_bytes());
        assert_eq!(&buf[8..12], &0.5f32.to_le_bytes());
    }

    #[test]
    fn channel_count_sweep_byte_sizes() {
        for &ch in &[2usize, 8, 12, 16, 24, 32, 64] {
            let frames = 40;
            let samples = vec![0.0f32; frames * ch];
            let mut buf = Vec::new();
            push_f32_le(&mut buf, &samples);
            assert_eq!(buf.len(), frames * ch * 4);

            let mut header = Vec::new();
            write_caf_header(&mut header, 48_000, ch as u32, None).unwrap();
            assert_eq!(read_u32_be(&header, 44), ch as u32);
        }
    }
}
