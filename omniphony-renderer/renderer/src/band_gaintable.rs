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
//!
//! Two more derived fields ride the same container to visualise **breaks** in
//! speaker usage — places where two adjacent grid cells are served by abruptly
//! different speaker configurations:
//!
//! - [`BandGaintableFull::serialize_gain_discontinuity`]: per cell, the largest
//!   L2 distance between this cell's *energy-normalised* gain vector and any of
//!   its 6 grid neighbours'. Normalising strips the level (the energy field
//!   already shows that), leaving pure configuration: a smooth pan or a
//!   triplet-edge crossing reads near 0, a hard switch between disjoint speaker
//!   sets reads √2, opposite-phase configurations up to 2.
//! - [`BandGaintableFull::serialize_centroid_jump`]: per cell, the largest jump
//!   of the gain²-weighted speaker-position centroid ("where the sound image
//!   is") to any neighbour, in room units. Weights breaks by the physical
//!   distance the image jumps across the room.
//!
//! Neither is divided by the grid step, deliberately: a true discontinuity has
//! a grid-independent jump, while a fast-but-smooth transition's per-step
//! difference shrinks as the grid refines — so refining the grid separates the
//! two instead of blurring them together.

use std::io::Write as _;

/// `speaker_index` marking a payload as the all-speaker energy field rather
/// than a single speaker's slice. Shared with the OSC subscribe argument and
/// the Studio decoder, which key their caches on it.
pub const GLOBAL_ENERGY_INDEX: i64 = -1;

/// `speaker_index` marking a payload as the gain-configuration discontinuity
/// field (normalised gain-vector jump to the worst neighbour).
pub const GAIN_DISCONTINUITY_INDEX: i64 = -2;

/// `speaker_index` marking a payload as the energy-centroid jump field (room
/// distance the gain²-weighted speaker centroid moves to the worst neighbour).
pub const CENTROID_JUMP_INDEX: i64 = -3;

/// Below this per-cell amplitude (`√Σ gᵢ²`) a cell has no meaningful speaker
/// configuration; pairs involving it contribute 0 to the discontinuity fields.
/// The energy heatmap already shows holes — lighting up their *edges* here
/// would drown the real breaks in silence-boundary noise.
const SILENT_ENERGY: f32 = 1e-6;

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
    /// Cartesian speaker positions in the same normalised `[-1, 1]` room cube
    /// as the grid, in full speaker-index order. Consumed by the centroid-jump
    /// field; empty is accepted (that field then reads all-zero).
    pub speaker_positions: Vec<[f32; 3]>,
    pub bands: Vec<BandField>,
}

impl BandGaintableFull {
    /// Serialize just one speaker's per-band field (one f32 per cell per band) to
    /// the "OBGT" wire format. `speaker` out of range yields an all-zero field.
    pub fn serialize_for_speaker(&self, speaker: usize) -> Vec<u8> {
        let sc = self.speaker_count.max(1);
        self.serialize_field(speaker as i64, |_, band, cell| {
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
        self.serialize_field(GLOBAL_ENERGY_INDEX, |_, band, cell| {
            let base = cell * sc;
            band.gains
                .get(base..base + sc)
                .map(|row| row.iter().map(|g| g * g).sum::<f32>().sqrt())
                .unwrap_or(0.0)
        })
    }

    /// Serialize the gain-configuration discontinuity field: per cell per band,
    /// the largest L2 distance between the cell's energy-normalised gain vector
    /// and any of its 6 grid neighbours'. 0 = same speaker configuration, √2 =
    /// disjoint speaker sets. See the module docs for why this is a raw jump,
    /// not a gradient.
    pub fn serialize_gain_discontinuity(&self) -> Vec<u8> {
        let sc = self.speaker_count.max(1);
        let fields: Vec<Vec<f32>> = self
            .bands
            .iter()
            .map(|band| {
                let cells = band.gains.len() / sc;
                // Energy-normalise each cell's gain vector; silent cells keep
                // a zero vector and are gated out of the neighbour pairs.
                let mut features = vec![0.0f32; cells * sc];
                let mut active = vec![false; cells];
                for cell in 0..cells {
                    let row = &band.gains[cell * sc..(cell + 1) * sc];
                    let norm = row.iter().map(|g| g * g).sum::<f32>().sqrt();
                    if norm > SILENT_ENERGY {
                        active[cell] = true;
                        let inv = 1.0 / norm;
                        for (out, &g) in features[cell * sc..(cell + 1) * sc].iter_mut().zip(row) {
                            *out = g * inv;
                        }
                    }
                }
                self.neighbor_jump_field(sc, &features, &active)
            })
            .collect();
        self.serialize_field(GAIN_DISCONTINUITY_INDEX, |bi, _, cell| fields[bi][cell])
    }

    /// Serialize the energy-centroid jump field: per cell per band, the largest
    /// distance (in room units, `[-1, 1]` cube) the gain²-weighted speaker
    /// centroid moves to any of the 6 grid neighbours. Weights a break by how
    /// far the sound image physically jumps across the room.
    pub fn serialize_centroid_jump(&self) -> Vec<u8> {
        let sc = self.speaker_count.max(1);
        let fields: Vec<Vec<f32>> = self
            .bands
            .iter()
            .map(|band| {
                let cells = band.gains.len() / sc;
                let mut features = vec![0.0f32; cells * 3];
                let mut active = vec![false; cells];
                for cell in 0..cells {
                    let row = &band.gains[cell * sc..(cell + 1) * sc];
                    let power: f32 = row.iter().map(|g| g * g).sum();
                    if power > SILENT_ENERGY * SILENT_ENERGY {
                        active[cell] = true;
                        let inv = 1.0 / power;
                        let mut c = [0.0f32; 3];
                        for (si, &g) in row.iter().enumerate() {
                            if let Some(p) = self.speaker_positions.get(si) {
                                let w = g * g * inv;
                                c[0] += w * p[0];
                                c[1] += w * p[1];
                                c[2] += w * p[2];
                            }
                        }
                        features[cell * 3..cell * 3 + 3].copy_from_slice(&c);
                    }
                }
                self.neighbor_jump_field(3, &features, &active)
            })
            .collect();
        self.serialize_field(CENTROID_JUMP_INDEX, |bi, _, cell| fields[bi][cell])
    }

    /// Max-over-neighbours L2 jump between per-cell feature vectors.
    ///
    /// `features` is `cells × dim` (row per cell); `active` gates cells with no
    /// energy — a pair with an inactive endpoint contributes nothing, so holes
    /// don't outline themselves. Each forward-neighbour pair along the three
    /// axes feeds the max of BOTH endpoints, which yields the 6-neighbour max.
    fn neighbor_jump_field(&self, dim: usize, features: &[f32], active: &[bool]) -> Vec<f32> {
        let nx = self.x_positions.len();
        let ny = self.y_positions.len();
        let nz = self.z_positions.len();
        let cells = nx * ny * nz;
        let mut out = vec![0.0f32; cells];
        if features.len() < cells * dim {
            return out;
        }
        let mut consider = |a: usize, b: usize| {
            if !(active[a] && active[b]) {
                return;
            }
            let fa = &features[a * dim..(a + 1) * dim];
            let fb = &features[b * dim..(b + 1) * dim];
            let d = fa
                .iter()
                .zip(fb)
                .map(|(x, y)| {
                    let e = x - y;
                    e * e
                })
                .sum::<f32>()
                .sqrt();
            if d > out[a] {
                out[a] = d;
            }
            if d > out[b] {
                out[b] = d;
            }
        };
        for zi in 0..nz {
            for yi in 0..ny {
                for xi in 0..nx {
                    let idx = xi + nx * (yi + ny * zi);
                    if xi + 1 < nx {
                        consider(idx, idx + 1);
                    }
                    if yi + 1 < ny {
                        consider(idx, idx + nx);
                    }
                    if zi + 1 < nz {
                        consider(idx, idx + nx * ny);
                    }
                }
            }
        }
        out
    }

    /// Shared "OBGT" writer: `value(band_index, band, cell)` supplies one f32
    /// per cell per band, `speaker_index` tags what the payload represents.
    fn serialize_field(
        &self,
        speaker_index: i64,
        value: impl Fn(usize, &BandField, usize) -> f32,
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
        for (bi, band) in self.bands.iter().enumerate() {
            for cell in 0..cells {
                raw.extend_from_slice(&value(bi, band, cell).to_le_bytes());
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
        fixture_with_gains(vec![h, h, 0.5, 0.0])
    }

    /// Same 2-cell grid and speakers (at the room's left/right walls), custom
    /// per-cell gains: `[cell0_s0, cell0_s1, cell1_s0, cell1_s1]`.
    fn fixture_with_gains(gains: Vec<f32>) -> BandGaintableFull {
        BandGaintableFull {
            x_positions: vec![-1.0, 1.0],
            y_positions: vec![0.0],
            z_positions: vec![0.0],
            speaker_count: 2,
            speaker_positions: vec![[-1.0, 0.0, 0.0], [1.0, 0.0, 0.0]],
            bands: vec![BandField {
                low_hz: 0.0,
                high_hz: f32::INFINITY,
                gains,
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

    #[test]
    fn gain_discontinuity_measures_the_configuration_jump_not_the_level() {
        let h = std::f32::consts::FRAC_1_SQRT_2;
        // cell 0 normalises to (h, h), cell 1 to (1, 0): the jump is the chord
        // ‖(h−1, h)‖ = √(2 − √2), independent of cell 1 being at half level.
        let (metadata, values) = decode(&fixture().serialize_gain_discontinuity(), 4);
        assert_eq!(metadata["speaker_index"], GAIN_DISCONTINUITY_INDEX);
        let expected = (2.0f32 - std::f32::consts::SQRT_2).sqrt();
        for (cell, v) in values.iter().enumerate() {
            assert!(
                (v - expected).abs() < 1e-6,
                "cell {cell}: jump {v} != chord {expected}"
            );
        }
    }

    #[test]
    fn a_pure_level_change_reads_zero_discontinuity() {
        let h = std::f32::consts::FRAC_1_SQRT_2;
        // Same direction (h, h) at full and at 1/5 level: the energy field sees
        // a 14 dB step, the configuration field must see nothing.
        let table = fixture_with_gains(vec![h, h, 0.2 * h, 0.2 * h]);
        let (_, values) = decode(&table.serialize_gain_discontinuity(), 4);
        assert!(values.iter().all(|v| v.abs() < 1e-6), "got {values:?}");
    }

    #[test]
    fn disjoint_speaker_sets_read_sqrt_two() {
        let table = fixture_with_gains(vec![1.0, 0.0, 0.0, 1.0]);
        let (_, values) = decode(&table.serialize_gain_discontinuity(), 4);
        for v in &values {
            assert!((v - std::f32::consts::SQRT_2).abs() < 1e-6, "got {v}");
        }
    }

    #[test]
    fn silent_cells_do_not_outline_holes() {
        let h = std::f32::consts::FRAC_1_SQRT_2;
        let table = fixture_with_gains(vec![h, h, 0.0, 0.0]);
        let (_, disc) = decode(&table.serialize_gain_discontinuity(), 4);
        let (_, jump) = decode(&table.serialize_centroid_jump(), 4);
        assert!(disc.iter().all(|v| *v == 0.0), "got {disc:?}");
        assert!(jump.iter().all(|v| *v == 0.0), "got {jump:?}");
    }

    #[test]
    fn centroid_jump_is_the_room_distance_the_image_moves() {
        // cell 0 splits power evenly → centroid at the origin; cell 1 is all on
        // the left-wall speaker → centroid at (−1, 0, 0). Jump = 1 room unit.
        let (metadata, values) = decode(&fixture().serialize_centroid_jump(), 4);
        assert_eq!(metadata["speaker_index"], CENTROID_JUMP_INDEX);
        for (cell, v) in values.iter().enumerate() {
            assert!((v - 1.0).abs() < 1e-6, "cell {cell}: jump {v} != 1.0");
        }
    }

    #[test]
    fn missing_speaker_positions_yield_a_zero_centroid_field() {
        let mut table = fixture();
        table.speaker_positions.clear();
        let (_, values) = decode(&table.serialize_centroid_jump(), 4);
        assert!(values.iter().all(|v| *v == 0.0), "got {values:?}");
    }

    #[test]
    fn jumps_are_detected_along_every_grid_axis() {
        // 1×1×2 grid: the only neighbour pair runs along Z. Same disjoint
        // configurations as the X-axis test — the break must still show.
        let table = BandGaintableFull {
            x_positions: vec![0.0],
            y_positions: vec![0.0],
            z_positions: vec![0.0, 1.0],
            speaker_count: 2,
            speaker_positions: vec![[-1.0, 0.0, 0.0], [1.0, 0.0, 0.0]],
            bands: vec![BandField {
                low_hz: 0.0,
                high_hz: f32::INFINITY,
                gains: vec![1.0, 0.0, 0.0, 1.0],
            }],
        };
        let (_, values) = decode(&table.serialize_gain_discontinuity(), 4);
        for v in &values {
            assert!((v - std::f32::consts::SQRT_2).abs() < 1e-6, "got {v}");
        }
    }
}
