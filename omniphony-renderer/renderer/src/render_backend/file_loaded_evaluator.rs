use anyhow::Result;
use flate2::read::ZlibDecoder;
use std::io::Read;

use super::{
    BackendCapabilities, CartesianSpeakerHeatmapSlices, CartesianSpeakerHeatmapVolume,
    EffectiveEvaluationMode, PreparedEvaluator, RenderRequest, RenderResponse,
};
use crate::spatial_vbap::{Gains, adm_to_spherical};
use crate::speaker_layout::{Speaker, SpeakerLayout};

#[derive(Clone)]
struct LoadedSpreadTable {
    spread: f32,
    gains: Vec<f32>,
}

#[derive(Clone)]
pub struct LoadedVbapFile {
    raw_bytes: Vec<u8>,
    n_speakers: usize,
    az_res_deg: i32,
    el_res_deg: i32,
    n_az: usize,
    n_el: usize,
    n_triangles: usize,
    spread_resolution: f32,
    spread_tables: Vec<LoadedSpreadTable>,
    speaker_layout: Option<SpeakerLayout>,
}

impl LoadedVbapFile {
    pub fn load_from_file(path: &std::path::Path) -> Result<Self> {
        const MAGIC: u32 = 0x56424150;

        let raw_bytes = std::fs::read(path)?;
        let mut cursor = std::io::Cursor::new(raw_bytes.as_slice());

        let mut magic_buf = [0u8; 4];
        cursor.read_exact(&mut magic_buf)?;
        let magic = u32::from_le_bytes(magic_buf);
        if magic != MAGIC {
            anyhow::bail!(
                "Invalid magic number: expected 0x{:08X}, got 0x{:08X}",
                MAGIC,
                magic
            );
        }

        let version = read_u32(&mut cursor, "version")?;
        let n_speakers = read_u32(&mut cursor, "n_speakers")? as usize;
        let az_res_deg = read_u32(&mut cursor, "az_res_deg")? as i32;
        let el_res_deg = read_u32(&mut cursor, "el_res_deg")? as i32;
        let n_az = read_u32(&mut cursor, "n_az")? as usize;
        let n_el = read_u32(&mut cursor, "n_el")? as usize;
        let n_gtable = read_u32(&mut cursor, "n_gtable")? as usize;
        let n_triangles = read_u32(&mut cursor, "n_triangles")? as usize;
        let num_spread_tables = read_u32(&mut cursor, "num_spread_tables")? as usize;
        let spread_resolution = read_f32(&mut cursor, "spread_resolution")?;

        let (spread_tables, speaker_layout, actual_n_speakers) = match version {
            1 => load_v1_tables(&mut cursor, n_speakers, n_gtable, num_spread_tables)?,
            2 => load_v2_tables(&mut cursor, n_speakers, n_gtable, num_spread_tables)?,
            3 => load_v3_tables(&mut cursor, n_speakers, n_gtable, num_spread_tables)?,
            _ => anyhow::bail!("Unsupported version: {}", version),
        };

        Ok(Self {
            raw_bytes,
            n_speakers: actual_n_speakers,
            az_res_deg,
            el_res_deg,
            n_az,
            n_el,
            n_triangles,
            spread_resolution,
            spread_tables,
            speaker_layout,
        })
    }

    pub fn speaker_layout(&self) -> Option<&SpeakerLayout> {
        self.speaker_layout.as_ref()
    }

    pub fn num_speakers(&self) -> usize {
        self.n_speakers
    }

    pub fn num_triangles(&self) -> usize {
        self.n_triangles
    }

    pub fn azimuth_resolution(&self) -> i32 {
        self.az_res_deg
    }

    pub fn elevation_resolution(&self) -> i32 {
        self.el_res_deg
    }

    pub fn spread_resolution(&self) -> f32 {
        self.spread_resolution
    }
}

pub struct FileLoadedEvaluator {
    file: LoadedVbapFile,
    allow_negative_z: bool,
    position_interpolation: bool,
}

impl FileLoadedEvaluator {
    pub fn new(file: LoadedVbapFile, allow_negative_z: bool, position_interpolation: bool) -> Self {
        Self {
            file,
            allow_negative_z,
            position_interpolation,
        }
    }

    pub fn capabilities() -> BackendCapabilities {
        BackendCapabilities {
            supports_realtime: false,
            supports_precomputed_polar: false,
            supports_precomputed_cartesian: false,
            supports_position_interpolation: true,
            supports_distance_model: true,
            supports_spread: true,
            supports_spread_from_distance: true,
            supports_distance_diffuse: true,
            supports_heatmap_cartesian: false,
            supports_table_export: true,
        }
    }

    fn get_gains_with_spread(&self, azimuth_deg: f32, elevation_deg: f32, spread: f32) -> Gains {
        let (az0_idx, az1_idx, azt) = self.get_azimuth_idx(azimuth_deg);
        let (el0_idx, el1_idx, elt) = self.get_elevation_idx(elevation_deg);
        let (sp0_idx, sp1_idx, spt) = self.get_spread_idx(spread);

        let offset00 = (el0_idx * self.file.n_az + az0_idx) * self.file.n_speakers;
        let offset01 = (el1_idx * self.file.n_az + az0_idx) * self.file.n_speakers;
        let offset10 = (el0_idx * self.file.n_az + az1_idx) * self.file.n_speakers;
        let offset11 = (el1_idx * self.file.n_az + az1_idx) * self.file.n_speakers;

        if sp0_idx == sp1_idx {
            return self
                .get_gains_from_4(offset00, offset01, offset10, offset11, azt, elt, sp0_idx);
        }

        let g0 = self.get_gains_from_4(offset00, offset01, offset10, offset11, azt, elt, sp0_idx);
        let g1 = self.get_gains_from_4(offset00, offset01, offset10, offset11, azt, elt, sp1_idx);
        self.interpol(&g0, &g1, spt)
    }

    fn get_azimuth_idx(&self, azimuth_deg: f32) -> (usize, usize, f32) {
        let az = (azimuth_deg + 180.0) / self.file.az_res_deg as f32;
        let az0_idx = (az.floor() as usize) % self.file.n_az;
        let az1_idx = if self.position_interpolation {
            (az.ceil() as usize) % self.file.n_az
        } else {
            az0_idx
        };
        let azt = az - az0_idx as f32;
        (az0_idx, az1_idx, azt)
    }

    fn get_elevation_idx(&self, elevation_deg: f32) -> (usize, usize, f32) {
        let min = if self.allow_negative_z { -90.0 } else { 0.0 };
        let clamped = elevation_deg.clamp(min, 90.0);
        let el = if self.allow_negative_z {
            (clamped + 90.0) / self.file.el_res_deg as f32
        } else {
            clamped / self.file.el_res_deg as f32
        };
        let max = self.file.n_el - 1;
        let el0_idx = (el.floor() as usize).min(max);
        let el1_idx = if self.position_interpolation {
            (el.ceil() as usize).min(max)
        } else {
            el0_idx
        };
        let elt = el - el0_idx as f32;
        (el0_idx, el1_idx, elt)
    }

    fn get_spread_idx(&self, spread: f32) -> (usize, usize, f32) {
        if self.file.spread_tables.len() <= 1 {
            return (0, 0, 0.0);
        }
        let spread_clamped = spread.clamp(0.0, 1.0);
        if !self.position_interpolation {
            let nearest = self
                .file
                .spread_tables
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    (a.spread - spread_clamped)
                        .abs()
                        .total_cmp(&(b.spread - spread_clamped).abs())
                })
                .map(|(index, _)| index)
                .unwrap_or(0);
            return (nearest, nearest, 0.0);
        }

        if spread_clamped <= self.file.spread_tables[0].spread {
            return (0, 0, 0.0);
        }
        let last = self.file.spread_tables.len() - 1;
        if spread_clamped >= self.file.spread_tables[last].spread {
            return (last, last, 0.0);
        }
        for index in 0..last {
            let lower = self.file.spread_tables[index].spread;
            let upper = self.file.spread_tables[index + 1].spread;
            if spread_clamped >= lower && spread_clamped <= upper {
                let span = (upper - lower).max(1e-6);
                return (
                    index,
                    index + 1,
                    ((spread_clamped - lower) / span).clamp(0.0, 1.0),
                );
            }
        }
        (last, last, 0.0)
    }

    fn get_gains_from_1(&self, offset: usize, table_idx: usize) -> Gains {
        let mut gains = Gains::zeroed(self.file.n_speakers);
        for (index, value) in self.file.spread_tables[table_idx].gains
            [offset..offset + self.file.n_speakers]
            .iter()
            .copied()
            .enumerate()
        {
            gains.set(index, value);
        }
        gains
    }

    fn get_gains_from_2(
        &self,
        offset00: usize,
        offset01: usize,
        t: f32,
        table_idx: usize,
    ) -> Gains {
        let gains0 = self.get_gains_from_1(offset00, table_idx);
        let gains1 = self.get_gains_from_1(offset01, table_idx);
        self.interpol(&gains0, &gains1, t)
    }

    fn get_gains_from_4(
        &self,
        offset00: usize,
        offset01: usize,
        offset10: usize,
        offset11: usize,
        azt: f32,
        elt: f32,
        table_idx: usize,
    ) -> Gains {
        let gains_left = self.get_gains_from_2(offset00, offset01, elt, table_idx);
        let gains_right = self.get_gains_from_2(offset10, offset11, elt, table_idx);
        self.interpol(&gains_left, &gains_right, azt)
    }

    fn interpol(&self, gains_low: &Gains, gains_high: &Gains, t: f32) -> Gains {
        let mut gains_interp = Gains::zeroed(self.file.n_speakers);
        for i in 0..self.file.n_speakers {
            gains_interp.set(i, gains_low[i] * (1.0 - t) + gains_high[i] * t);
        }
        gains_interp
    }

    fn apply_gain(gains: &Gains, gain: f32) -> Gains {
        let mut gains_out = Gains::zeroed(gains.len());
        for i in 0..gains.len() {
            gains_out.set(i, gains[i] * gain);
        }
        gains_out
    }
}

impl PreparedEvaluator for FileLoadedEvaluator {
    fn speaker_count(&self) -> usize {
        self.file.n_speakers
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        let rendering_position = req.adm_position;
        let scaled_x = rendering_position[0] as f32 * req.room_ratio[0];
        let scaled_y = map_depth_with_room_ratios(
            rendering_position[1] as f32,
            req.room_ratio[1],
            req.room_ratio_rear,
            req.room_ratio_center_blend,
        );
        let scaled_z = if rendering_position[2] >= 0.0 {
            rendering_position[2] as f32 * req.room_ratio[2]
        } else {
            rendering_position[2] as f32 * req.room_ratio_lower
        };

        let (_, _, dist) = adm_to_spherical(scaled_x, scaled_y, scaled_z);
        let effective_spread = if req.spread_from_distance {
            let t = (1.0 - dist / req.spread_distance_range)
                .clamp(0.0, 1.0)
                .powf(req.spread_distance_curve);
            (req.spread_min + t * (req.spread_max - req.spread_min)).clamp(0.0, 1.0)
        } else {
            req.spread_min.clamp(0.0, 1.0)
        };

        let z = if self.allow_negative_z {
            scaled_z
        } else {
            scaled_z.max(0.0)
        };
        let (azimuth, elevation, distance) = adm_to_spherical(scaled_x, scaled_y, z);
        let direct = self.get_gains_with_spread(azimuth, elevation, effective_spread);
        let directional = if req.use_distance_diffuse {
            let mirror_z = if self.allow_negative_z {
                scaled_z
            } else {
                scaled_z.max(0.0)
            };
            let (mirror_azimuth, mirror_elevation, _) =
                adm_to_spherical(-scaled_x, -scaled_y, mirror_z);
            let mirror =
                self.get_gains_with_spread(mirror_azimuth, mirror_elevation, effective_spread);
            let [rx, ry, rz] = rendering_position;
            let adm_dist = ((rx * rx + ry * ry + rz * rz) as f32).sqrt();
            let t = (adm_dist / req.distance_diffuse_threshold.max(1e-6))
                .min(1.0)
                .powf(req.distance_diffuse_curve);
            let alpha = 0.5 + 0.5 * t;
            let w_direct = alpha.sqrt();
            let w_mirror = (1.0 - alpha).sqrt();
            let mut blended = Gains::zeroed(self.file.n_speakers);
            let mut energy_direct = 0.0f32;
            let mut energy_blended = 0.0f32;
            for i in 0..self.file.n_speakers {
                let g = w_direct * direct[i] + w_mirror * mirror[i];
                blended.set(i, g);
                energy_direct += direct[i] * direct[i];
                energy_blended += g * g;
            }
            if energy_blended > 1e-12 {
                let scale = (energy_direct / energy_blended).sqrt();
                for g in blended.iter_mut() {
                    *g *= scale;
                }
            }
            blended
        } else {
            direct
        };

        RenderResponse {
            gains: Self::apply_gain(
                &directional,
                crate::spatial_vbap::calculate_distance_attenuation(distance, req.distance_model),
            ),
        }
    }

    fn save_to_file(&self, path: &std::path::Path, _speaker_layout: &SpeakerLayout) -> Result<()> {
        std::fs::write(path, &self.file.raw_bytes)?;
        Ok(())
    }

    fn cartesian_slices_for_speaker(
        &self,
        _speaker_index: usize,
        _speaker_position: [f32; 3],
    ) -> Option<CartesianSpeakerHeatmapSlices> {
        None
    }

    fn cartesian_volume_for_speaker(
        &self,
        _speaker_index: usize,
        _threshold: f32,
        _max_samples: usize,
    ) -> Option<CartesianSpeakerHeatmapVolume> {
        None
    }
}

pub fn build_from_file_render_engine(
    file: LoadedVbapFile,
    allow_negative_z: bool,
    position_interpolation: bool,
) -> super::PreparedRenderEngine {
    super::PreparedRenderEngine::new(
        super::GainModelKind::Vbap,
        "vbap",
        "VBAP",
        FileLoadedEvaluator::capabilities(),
        EffectiveEvaluationMode::FromFile,
        None,
        Box::new(FileLoadedEvaluator::new(
            file,
            allow_negative_z,
            position_interpolation,
        )),
    )
}

fn map_depth_with_room_ratios(
    depth: f32,
    front_ratio: f32,
    rear_ratio: f32,
    center_blend: f32,
) -> f32 {
    let d = depth.clamp(-1.0, 1.0);
    let blend = center_blend.clamp(0.0, 1.0);
    let center_ratio = rear_ratio + (front_ratio - rear_ratio) * blend;
    if d >= 0.0 {
        let t = d;
        let a = center_ratio - front_ratio;
        let b = 2.0 * (front_ratio - center_ratio);
        a * t * t * t + b * t * t + center_ratio * t
    } else {
        let t = -d;
        let a = center_ratio - rear_ratio;
        let b = 2.0 * (rear_ratio - center_ratio);
        -(a * t * t * t + b * t * t + center_ratio * t)
    }
}

fn load_v1_tables(
    cursor: &mut std::io::Cursor<&[u8]>,
    n_speakers: usize,
    n_gtable: usize,
    num_spread_tables: usize,
) -> Result<(Vec<LoadedSpreadTable>, Option<SpeakerLayout>, usize)> {
    let mut reserved = [0u8; 28];
    cursor.read_exact(&mut reserved)?;
    let mut spread_tables = Vec::with_capacity(num_spread_tables);
    for _ in 0..num_spread_tables {
        let spread = read_f32(cursor, "spread")?;
        let total_elements = n_gtable * n_speakers;
        let mut gains = Vec::with_capacity(total_elements);
        for _ in 0..total_elements {
            gains.push(read_f32(cursor, "gain")?);
        }
        spread_tables.push(LoadedSpreadTable { spread, gains });
    }
    Ok((spread_tables, None, n_speakers))
}

fn load_v2_tables(
    cursor: &mut std::io::Cursor<&[u8]>,
    n_speakers: usize,
    n_gtable: usize,
    num_spread_tables: usize,
) -> Result<(Vec<LoadedSpreadTable>, Option<SpeakerLayout>, usize)> {
    let compression_flag = read_u32(cursor, "compression_flag")?;
    let mut reserved = [0u8; 24];
    cursor.read_exact(&mut reserved)?;
    if compression_flag != 1 {
        anyhow::bail!("Unsupported compression flag: {}", compression_flag);
    }
    let compressed_size = read_u32(cursor, "compressed_size")? as usize;
    let mut compressed_data = vec![0u8; compressed_size];
    cursor.read_exact(&mut compressed_data)?;
    let uncompressed = decompress(&compressed_data)?;
    let spread_tables =
        parse_uncompressed_tables(&uncompressed, n_speakers, n_gtable, num_spread_tables)?;
    Ok((spread_tables, None, n_speakers))
}

fn load_v3_tables(
    cursor: &mut std::io::Cursor<&[u8]>,
    n_speakers: usize,
    n_gtable: usize,
    num_spread_tables: usize,
) -> Result<(Vec<LoadedSpreadTable>, Option<SpeakerLayout>, usize)> {
    let compression_flag = read_u32(cursor, "compression_flag")?;
    let layout_data_size = read_u32(cursor, "layout_data_size")? as usize;
    let mut reserved = [0u8; 20];
    cursor.read_exact(&mut reserved)?;
    if compression_flag != 1 {
        anyhow::bail!("Unsupported compression flag: {}", compression_flag);
    }
    let mut layout_data = vec![0u8; layout_data_size];
    cursor.read_exact(&mut layout_data)?;
    let speakers = parse_layout_data(&layout_data, n_speakers)?;
    let speaker_layout = SpeakerLayout {
        radius_m: 1.0,
        speakers: speakers.clone(),
    };
    let n_spatializable = speakers.iter().filter(|s| s.spatialize).count();
    let compressed_size = read_u32(cursor, "compressed_size")? as usize;
    let mut compressed_data = vec![0u8; compressed_size];
    cursor.read_exact(&mut compressed_data)?;
    let uncompressed = decompress(&compressed_data)?;
    let spread_tables =
        parse_uncompressed_tables(&uncompressed, n_spatializable, n_gtable, num_spread_tables)?;
    Ok((spread_tables, Some(speaker_layout), n_spatializable))
}

fn parse_uncompressed_tables(
    bytes: &[u8],
    speaker_count: usize,
    n_gtable: usize,
    num_spread_tables: usize,
) -> Result<Vec<LoadedSpreadTable>> {
    let mut tables = Vec::with_capacity(num_spread_tables);
    let mut offset = 0usize;
    for _ in 0..num_spread_tables {
        if offset + 4 > bytes.len() {
            anyhow::bail!("Incomplete spread table data");
        }
        let spread = f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        offset += 4;
        let total_elements = n_gtable * speaker_count;
        let required_bytes = total_elements * 4;
        if offset + required_bytes > bytes.len() {
            anyhow::bail!(
                "Incomplete gain table data: need {} bytes, have {} bytes",
                required_bytes,
                bytes.len() - offset
            );
        }
        let mut gains = Vec::with_capacity(total_elements);
        for _ in 0..total_elements {
            gains.push(f32::from_le_bytes(
                bytes[offset..offset + 4].try_into().unwrap(),
            ));
            offset += 4;
        }
        tables.push(LoadedSpreadTable { spread, gains });
    }
    Ok(tables)
}

fn parse_layout_data(bytes: &[u8], n_speakers: usize) -> Result<Vec<Speaker>> {
    let mut offset = 0usize;
    let mut speakers = Vec::with_capacity(n_speakers);
    for _ in 0..n_speakers {
        if offset + 4 > bytes.len() {
            anyhow::bail!("Incomplete speaker layout data");
        }
        let name_len = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        if offset + name_len > bytes.len() {
            anyhow::bail!("Incomplete speaker name data");
        }
        let name = String::from_utf8(bytes[offset..offset + name_len].to_vec())?;
        offset += name_len;
        if offset + 9 > bytes.len() {
            anyhow::bail!("Incomplete speaker position data");
        }
        let azimuth = f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        offset += 4;
        let elevation = f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        offset += 4;
        let spatialize = bytes[offset] != 0;
        offset += 1;
        speakers.push(Speaker::from_polar(
            name, azimuth, elevation, 1.0, spatialize, 0.0,
        ));
    }
    Ok(speakers)
}

fn decompress(data: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = ZlibDecoder::new(data);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out)?;
    Ok(out)
}

fn read_u32(reader: &mut impl Read, name: &str) -> Result<u32> {
    let mut buf = [0u8; 4];
    reader
        .read_exact(&mut buf)
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", name, e))?;
    Ok(u32::from_le_bytes(buf))
}

fn read_f32(reader: &mut impl Read, name: &str) -> Result<f32> {
    let mut buf = [0u8; 4];
    reader
        .read_exact(&mut buf)
        .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", name, e))?;
    Ok(f32::from_le_bytes(buf))
}
