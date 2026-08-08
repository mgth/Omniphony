//! Per-crossover-band speaker gain table, sampled over the cartesian grid.
//!
//! Built once per topology (see [`crate::live_params::RendererControl::build_band_gaintable_full`])
//! and cached raw. Because a heatmap only ever shows **one** speaker, the wire
//! payload is serialized per speaker ([`BandGaintableFull::serialize_for_speaker`])
//! — one value per cell per band — which is `speaker_count`× smaller than shipping
//! the whole table. Format "OBGT": 16-byte header (magic + version + meta_len +
//! payload_len), metadata JSON, then zlib(x_pos, y_pos, z_pos, band0 gains, …).
//!
//! [`BandGaintableFull::serialize_energy`] emits the same container and the same
//! size, but each value is the cell's **total** amplitude across every speaker
//! (`√Σ gᵢ²`) instead of one speaker's gain. Same unit as a gain, so `1.0` is
//! exactly unit energy — the reference a conservation check compares against.

use std::io::Write as _;

/// `speaker_index` marking a payload as the all-speaker energy field rather
/// than a single speaker's slice. Shared with the OSC subscribe argument and
/// the Studio decoder, which key their caches on it.
pub const GLOBAL_ENERGY_INDEX: i64 = -1;

/// One crossover band's full field: `gains[cell * speaker_count + speaker]`.
pub struct BandField {
    pub low_hz: f32,
    pub high_hz: f32,
    pub gains: Vec<f32>,
}

/// All bands' fields over a shared cartesian grid (all speakers).
pub struct BandGaintableFull {
    pub x_positions: Vec<f32>,
    pub y_positions: Vec<f32>,
    pub z_positions: Vec<f32>,
    pub speaker_count: usize,
    pub bands: Vec<BandField>,
}

impl BandGaintableFull {
    /// Serialize just one speaker's per-band field (one f32 per cell per band) to
    /// the "OBGT" wire format. `speaker` out of range yields an all-zero field.
    pub fn serialize_for_speaker(&self, speaker: usize) -> Vec<u8> {
        let sc = self.speaker_count.max(1);
        self.serialize_field(speaker as i64, |band, cell| {
            band.gains.get(cell * sc + speaker).copied().unwrap_or(0.0)
        })
    }

    /// Serialize the **total** field: per cell per band, the amplitude summed in
    /// power over every speaker (`√Σ gᵢ²`). Same container and size as one
    /// speaker's slice, so it rides the existing chunking untouched.
    ///
    /// `1.0` is unit energy: a panner that conserves energy reads 0 dB
    /// everywhere, and any deviation is a real gain or loss at that direction —
    /// which is what the out-of-hull modes differ on.
    pub fn serialize_energy(&self) -> Vec<u8> {
        let sc = self.speaker_count.max(1);
        self.serialize_field(GLOBAL_ENERGY_INDEX, |band, cell| {
            let base = cell * sc;
            band.gains
                .get(base..base + sc)
                .map(|row| row.iter().map(|g| g * g).sum::<f32>().sqrt())
                .unwrap_or(0.0)
        })
    }

    /// Shared "OBGT" writer: `value(band, cell)` supplies one f32 per cell per
    /// band, `speaker_index` tags what the payload represents.
    fn serialize_field(
        &self,
        speaker_index: i64,
        value: impl Fn(&BandField, usize) -> f32,
    ) -> Vec<u8> {
        let nx = self.x_positions.len();
        let ny = self.y_positions.len();
        let nz = self.z_positions.len();
        let cells = nx * ny * nz;

        let metadata = serde_json::json!({
            "domain": "cartesian_bands",
            "speaker_index": speaker_index,
            "x_count": nx,
            "y_count": ny,
            "z_count": nz,
            "band_count": self.bands.len(),
            "bands": self.bands
                .iter()
                .map(|b| {
                    serde_json::json!({
                        "low_hz": b.low_hz,
                        "high_hz": if b.high_hz.is_finite() {
                            serde_json::json!(b.high_hz)
                        } else {
                            serde_json::Value::Null
                        },
                    })
                })
                .collect::<Vec<_>>(),
        })
        .to_string();

        let mut raw: Vec<u8> = Vec::with_capacity((nx + ny + nz + cells * self.bands.len()) * 4);
        for &v in &self.x_positions {
            raw.extend_from_slice(&v.to_le_bytes());
        }
        for &v in &self.y_positions {
            raw.extend_from_slice(&v.to_le_bytes());
        }
        for &v in &self.z_positions {
            raw.extend_from_slice(&v.to_le_bytes());
        }
        for band in &self.bands {
            for cell in 0..cells {
                raw.extend_from_slice(&value(band, cell).to_le_bytes());
            }
        }

        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        let _ = enc.write_all(&raw);
        let payload = enc.finish().unwrap_or_default();

        let meta = metadata.as_bytes();
        let mut out = Vec::with_capacity(16 + meta.len() + payload.len());
        out.extend_from_slice(b"OBGT");
        out.push(1); // version
        out.extend_from_slice(&[0u8; 3]); // reserved
        out.extend_from_slice(&(meta.len() as u32).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(meta);
        out.extend_from_slice(&payload);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode an "OBGT" payload back into `(metadata, values_after_the_axes)`.
    fn decode(bytes: &[u8], axis_len: usize) -> (serde_json::Value, Vec<f32>) {
        use std::io::Read as _;
        assert_eq!(&bytes[0..4], b"OBGT");
        let meta_len = u32::from_le_bytes(bytes[8..12].try_into().unwrap()) as usize;
        let payload_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let meta_end = 16 + meta_len;
        let metadata: serde_json::Value = serde_json::from_slice(&bytes[16..meta_end]).unwrap();
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(&bytes[meta_end..meta_end + payload_len])
            .read_to_end(&mut raw)
            .unwrap();
        let values: Vec<f32> = raw
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .skip(axis_len)
            .collect();
        (metadata, values)
    }

    /// 2 cells × 2 speakers, one band. Cell 0 carries unit energy split over
    /// both speakers; cell 1 carries a single speaker at half amplitude.
    fn fixture() -> BandGaintableFull {
        let h = std::f32::consts::FRAC_1_SQRT_2;
        BandGaintableFull {
            x_positions: vec![-1.0, 1.0],
            y_positions: vec![0.0],
            z_positions: vec![0.0],
            speaker_count: 2,
            bands: vec![BandField {
                low_hz: 0.0,
                high_hz: f32::INFINITY,
                // cell 0: [h, h] → √(0.5+0.5) = 1; cell 1: [0.5, 0] → 0.5
                gains: vec![h, h, 0.5, 0.0],
            }],
        }
    }

    #[test]
    fn energy_field_is_the_power_sum_amplitude_over_all_speakers() {
        let (metadata, values) = decode(&fixture().serialize_energy(), 4);
        assert_eq!(metadata["speaker_index"], GLOBAL_ENERGY_INDEX);
        assert_eq!(metadata["domain"], "cartesian_bands");
        assert_eq!(values.len(), 2, "one value per cell, not per speaker");
        // Unit energy reads exactly 1.0 — the 0 dB reference the heatmap centres on.
        assert!(
            (values[0] - 1.0).abs() < 1e-6,
            "energy-conserving cell should read 1.0, got {}",
            values[0]
        );
        assert!(
            (values[1] - 0.5).abs() < 1e-6,
            "half-amplitude cell should read 0.5, got {}",
            values[1]
        );
    }

    #[test]
    fn energy_field_matches_the_per_speaker_slices_it_sums() {
        let table = fixture();
        let (_, energy) = decode(&table.serialize_energy(), 4);
        let (_, s0) = decode(&table.serialize_for_speaker(0), 4);
        let (_, s1) = decode(&table.serialize_for_speaker(1), 4);
        assert_eq!(energy.len(), s0.len());
        for cell in 0..energy.len() {
            let expected = (s0[cell] * s0[cell] + s1[cell] * s1[cell]).sqrt();
            assert!(
                (energy[cell] - expected).abs() < 1e-6,
                "cell {cell}: energy {} != √Σ per-speaker² {expected}",
                energy[cell]
            );
        }
    }

    #[test]
    fn a_speaker_out_of_range_yields_a_silent_field_not_a_panic() {
        let (metadata, values) = decode(&fixture().serialize_for_speaker(99), 4);
        assert_eq!(metadata["speaker_index"], 99);
        assert!(values.iter().all(|v| *v == 0.0));
    }
}
