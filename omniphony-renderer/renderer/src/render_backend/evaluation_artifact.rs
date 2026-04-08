use anyhow::Result;
use flate2::Compression;
use flate2::read::ZlibDecoder;
use flate2::write::ZlibEncoder;
use serde::{Deserialize, Serialize};
use std::io::{Cursor, Read, Write};

use super::{
    BackendCapabilities, CartesianSpeakerHeatmapSlices, CartesianSpeakerHeatmapVolume,
    EffectiveEvaluationMode, EvaluationBuildConfig, PreparedEvaluator, RenderRequest,
    RenderResponse, sample_cartesian_table, sample_polar_table,
};
use crate::speaker_layout::SpeakerLayout;

const MAGIC: &[u8; 4] = b"OEVL";
const VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SerializedEvaluationMode {
    PrecomputedCartesian,
    PrecomputedPolar,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrozenRenderRequest {
    pub spread_min: f32,
    pub spread_max: f32,
    pub spread_from_distance: bool,
    pub spread_distance_range: f32,
    pub spread_distance_curve: f32,
    pub room_ratio: [f32; 3],
    pub room_ratio_rear: f32,
    pub room_ratio_lower: f32,
    pub room_ratio_center_blend: f32,
    pub use_distance_diffuse: bool,
    pub distance_diffuse_threshold: f32,
    pub distance_diffuse_curve: f32,
    pub distance_model: String,
}

impl From<RenderRequest> for FrozenRenderRequest {
    fn from(value: RenderRequest) -> Self {
        Self {
            spread_min: value.spread_min,
            spread_max: value.spread_max,
            spread_from_distance: value.spread_from_distance,
            spread_distance_range: value.spread_distance_range,
            spread_distance_curve: value.spread_distance_curve,
            room_ratio: value.room_ratio,
            room_ratio_rear: value.room_ratio_rear,
            room_ratio_lower: value.room_ratio_lower,
            room_ratio_center_blend: value.room_ratio_center_blend,
            use_distance_diffuse: value.use_distance_diffuse,
            distance_diffuse_threshold: value.distance_diffuse_threshold,
            distance_diffuse_curve: value.distance_diffuse_curve,
            distance_model: value.distance_model.to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationArtifactMetadata {
    pub source_backend_id: String,
    pub source_backend_label: String,
    pub mode: SerializedEvaluationMode,
    pub speaker_layout: SpeakerLayout,
    pub frozen_request: FrozenRenderRequest,
    pub position_interpolation: bool,
    pub backend_restore_payload: Option<Vec<u8>>,
    pub domain: EvaluationArtifactDomainMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendRestoreSnapshot {
    pub backend_id: String,
    pub backend_label: String,
    pub evaluation_mode: SerializedEvaluationMode,
    pub position_interpolation: bool,
    pub allow_negative_z: bool,
    pub cartesian_x_size: usize,
    pub cartesian_y_size: usize,
    pub cartesian_z_size: usize,
    pub cartesian_z_neg_size: usize,
    pub polar_azimuth_values: usize,
    pub polar_elevation_values: usize,
    pub polar_distance_res: usize,
    pub polar_distance_max: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvaluationArtifactDomainMetadata {
    Cartesian {
        speaker_count: usize,
        x_count: usize,
        y_count: usize,
        z_count: usize,
    },
    Polar {
        speaker_count: usize,
        azimuth_count: usize,
        elevation_count: usize,
        distance_count: usize,
    },
}

#[derive(Clone)]
pub enum LoadedEvaluationArtifact {
    Cartesian(CartesianArtifact),
    Polar(PolarArtifact),
}

#[derive(Clone)]
pub struct CartesianArtifact {
    metadata: EvaluationArtifactMetadata,
    x_positions: Vec<f32>,
    y_positions: Vec<f32>,
    z_positions: Vec<f32>,
    gains: Vec<f32>,
}

#[derive(Clone)]
pub struct PolarArtifact {
    metadata: EvaluationArtifactMetadata,
    azimuth_positions: Vec<f32>,
    elevation_positions: Vec<f32>,
    distance_positions: Vec<f32>,
    gains: Vec<f32>,
}

impl LoadedEvaluationArtifact {
    pub fn load_from_file(path: &std::path::Path) -> Result<Self> {
        let bytes = std::fs::read(path)?;
        let mut cursor = Cursor::new(bytes.as_slice());

        let mut magic = [0u8; 4];
        cursor.read_exact(&mut magic)?;
        if &magic != MAGIC {
            anyhow::bail!("Unsupported evaluator artifact magic");
        }

        let version = read_u32(&mut cursor)?;
        if version != VERSION {
            anyhow::bail!(
                "Unsupported evaluator artifact version: expected {}, got {}",
                VERSION,
                version
            );
        }

        let metadata_len = read_u32(&mut cursor)? as usize;
        let payload_len = read_u32(&mut cursor)? as usize;

        let mut metadata_json = vec![0u8; metadata_len];
        cursor.read_exact(&mut metadata_json)?;
        let metadata: EvaluationArtifactMetadata = serde_json::from_slice(&metadata_json)?;

        let mut payload = vec![0u8; payload_len];
        cursor.read_exact(&mut payload)?;
        let payload = decompress(&payload)?;
        Self::from_parts(metadata, &payload)
    }

    pub fn save_to_file(&self, path: &std::path::Path) -> Result<()> {
        let metadata = self.metadata();
        let metadata_json = serde_json::to_vec(metadata)?;
        let payload = compress(&self.payload_bytes()?)?;

        let mut out = Vec::with_capacity(16 + metadata_json.len() + payload.len());
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        out.extend_from_slice(&(metadata_json.len() as u32).to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&metadata_json);
        out.extend_from_slice(&payload);
        std::fs::write(path, out)?;
        Ok(())
    }

    pub fn speaker_layout(&self) -> &SpeakerLayout {
        &self.metadata().speaker_layout
    }

    pub fn mode(&self) -> SerializedEvaluationMode {
        self.metadata().mode
    }

    pub fn frozen_request(&self) -> &FrozenRenderRequest {
        &self.metadata().frozen_request
    }

    pub fn source_backend_id(&self) -> &str {
        self.metadata().source_backend_id.as_str()
    }

    pub fn source_backend_label(&self) -> &str {
        self.metadata().source_backend_label.as_str()
    }

    pub fn position_interpolation(&self) -> bool {
        self.metadata().position_interpolation
    }

    pub fn has_backend_restore_snapshot(&self) -> bool {
        self.metadata().backend_restore_payload.is_some()
    }

    pub fn backend_restore_snapshot(&self) -> Option<BackendRestoreSnapshot> {
        self.metadata()
            .backend_restore_payload
            .as_ref()
            .and_then(|payload| serde_json::from_slice::<BackendRestoreSnapshot>(payload).ok())
    }

    pub fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supports_realtime: false,
            supports_precomputed_polar: false,
            supports_precomputed_cartesian: false,
            supports_position_interpolation: true,
            supports_distance_model: false,
            supports_spread: false,
            supports_spread_from_distance: false,
            supports_distance_diffuse: false,
            supports_heatmap_cartesian: matches!(self, Self::Cartesian(_)),
            supports_table_export: true,
        }
    }

    pub fn speaker_count(&self) -> usize {
        match self {
            Self::Cartesian(artifact) => match artifact.metadata.domain {
                EvaluationArtifactDomainMetadata::Cartesian { speaker_count, .. } => speaker_count,
                EvaluationArtifactDomainMetadata::Polar { .. } => unreachable!(),
            },
            Self::Polar(artifact) => match artifact.metadata.domain {
                EvaluationArtifactDomainMetadata::Polar { speaker_count, .. } => speaker_count,
                EvaluationArtifactDomainMetadata::Cartesian { .. } => unreachable!(),
            },
        }
    }

    pub fn cartesian_dimensions(&self) -> Option<(usize, usize, usize)> {
        match self {
            Self::Cartesian(artifact) => Some((
                artifact.x_positions.len(),
                artifact.y_positions.len(),
                artifact.z_positions.len(),
            )),
            Self::Polar(_) => None,
        }
    }

    pub fn polar_dimensions(&self) -> Option<(usize, usize, usize)> {
        match self {
            Self::Polar(artifact) => Some((
                artifact.azimuth_positions.len(),
                artifact.elevation_positions.len(),
                artifact.distance_positions.len(),
            )),
            Self::Cartesian(_) => None,
        }
    }

    fn metadata(&self) -> &EvaluationArtifactMetadata {
        match self {
            Self::Cartesian(artifact) => &artifact.metadata,
            Self::Polar(artifact) => &artifact.metadata,
        }
    }

    fn payload_bytes(&self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        match self {
            Self::Cartesian(artifact) => {
                write_f32_slice(&mut out, &artifact.x_positions)?;
                write_f32_slice(&mut out, &artifact.y_positions)?;
                write_f32_slice(&mut out, &artifact.z_positions)?;
                write_f32_slice(&mut out, &artifact.gains)?;
            }
            Self::Polar(artifact) => {
                write_f32_slice(&mut out, &artifact.azimuth_positions)?;
                write_f32_slice(&mut out, &artifact.elevation_positions)?;
                write_f32_slice(&mut out, &artifact.distance_positions)?;
                write_f32_slice(&mut out, &artifact.gains)?;
            }
        }
        Ok(out)
    }

    fn from_parts(metadata: EvaluationArtifactMetadata, payload: &[u8]) -> Result<Self> {
        let mut cursor = Cursor::new(payload);
        match metadata.domain {
            EvaluationArtifactDomainMetadata::Cartesian {
                x_count,
                y_count,
                z_count,
                speaker_count,
            } => {
                let x_positions = read_f32_vec(&mut cursor, x_count)?;
                let y_positions = read_f32_vec(&mut cursor, y_count)?;
                let z_positions = read_f32_vec(&mut cursor, z_count)?;
                let gains = read_f32_vec(&mut cursor, x_count * y_count * z_count * speaker_count)?;
                Ok(Self::Cartesian(CartesianArtifact {
                    metadata,
                    x_positions,
                    y_positions,
                    z_positions,
                    gains,
                }))
            }
            EvaluationArtifactDomainMetadata::Polar {
                azimuth_count,
                elevation_count,
                distance_count,
                speaker_count,
            } => {
                let azimuth_positions = read_f32_vec(&mut cursor, azimuth_count)?;
                let elevation_positions = read_f32_vec(&mut cursor, elevation_count)?;
                let distance_positions = read_f32_vec(&mut cursor, distance_count)?;
                let gains = read_f32_vec(
                    &mut cursor,
                    azimuth_count * elevation_count * distance_count * speaker_count,
                )?;
                Ok(Self::Polar(PolarArtifact {
                    metadata,
                    azimuth_positions,
                    elevation_positions,
                    distance_positions,
                    gains,
                }))
            }
        }
    }

    pub fn from_sampled_cartesian(
        source_backend_id: &str,
        source_backend_label: &str,
        speaker_layout: &SpeakerLayout,
        frozen_request: RenderRequest,
        position_interpolation: bool,
        backend_restore_snapshot: Option<&BackendRestoreSnapshot>,
        x_positions: &[f32],
        y_positions: &[f32],
        z_positions: &[f32],
        gains: &[f32],
        speaker_count: usize,
    ) -> Result<Self> {
        Ok(Self::Cartesian(CartesianArtifact {
            metadata: EvaluationArtifactMetadata {
                source_backend_id: source_backend_id.to_string(),
                source_backend_label: source_backend_label.to_string(),
                mode: SerializedEvaluationMode::PrecomputedCartesian,
                speaker_layout: speaker_layout.clone(),
                frozen_request: frozen_request.into(),
                position_interpolation,
                backend_restore_payload: encode_backend_restore_snapshot(backend_restore_snapshot)?,
                domain: EvaluationArtifactDomainMetadata::Cartesian {
                    speaker_count,
                    x_count: x_positions.len(),
                    y_count: y_positions.len(),
                    z_count: z_positions.len(),
                },
            },
            x_positions: x_positions.to_vec(),
            y_positions: y_positions.to_vec(),
            z_positions: z_positions.to_vec(),
            gains: gains.to_vec(),
        }))
    }

    pub fn from_sampled_polar(
        source_backend_id: &str,
        source_backend_label: &str,
        speaker_layout: &SpeakerLayout,
        frozen_request: RenderRequest,
        position_interpolation: bool,
        backend_restore_snapshot: Option<&BackendRestoreSnapshot>,
        azimuth_positions: &[f32],
        elevation_positions: &[f32],
        distance_positions: &[f32],
        gains: &[f32],
        speaker_count: usize,
    ) -> Result<Self> {
        Ok(Self::Polar(PolarArtifact {
            metadata: EvaluationArtifactMetadata {
                source_backend_id: source_backend_id.to_string(),
                source_backend_label: source_backend_label.to_string(),
                mode: SerializedEvaluationMode::PrecomputedPolar,
                speaker_layout: speaker_layout.clone(),
                frozen_request: frozen_request.into(),
                position_interpolation,
                backend_restore_payload: encode_backend_restore_snapshot(backend_restore_snapshot)?,
                domain: EvaluationArtifactDomainMetadata::Polar {
                    speaker_count,
                    azimuth_count: azimuth_positions.len(),
                    elevation_count: elevation_positions.len(),
                    distance_count: distance_positions.len(),
                },
            },
            azimuth_positions: azimuth_positions.to_vec(),
            elevation_positions: elevation_positions.to_vec(),
            distance_positions: distance_positions.to_vec(),
            gains: gains.to_vec(),
        }))
    }
}

pub struct EvaluationArtifactEvaluator {
    artifact: LoadedEvaluationArtifact,
}

impl EvaluationArtifactEvaluator {
    pub fn new(artifact: LoadedEvaluationArtifact) -> Self {
        Self { artifact }
    }
}

impl PreparedEvaluator for EvaluationArtifactEvaluator {
    fn speaker_count(&self) -> usize {
        self.artifact.speaker_count()
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        let gains = match &self.artifact {
            LoadedEvaluationArtifact::Cartesian(artifact) => sample_cartesian_table(
                &artifact.gains,
                self.artifact.speaker_count(),
                &artifact.x_positions,
                &artifact.y_positions,
                &artifact.z_positions,
                req.adm_position.map(|value| value as f32),
                artifact.metadata.position_interpolation,
            ),
            LoadedEvaluationArtifact::Polar(artifact) => {
                let (azimuth, elevation, distance) = crate::spatial_vbap::adm_to_spherical(
                    req.adm_position[0] as f32,
                    req.adm_position[1] as f32,
                    req.adm_position[2] as f32,
                );
                sample_polar_table(
                    &artifact.gains,
                    self.artifact.speaker_count(),
                    &artifact.azimuth_positions,
                    &artifact.elevation_positions,
                    &artifact.distance_positions,
                    [azimuth, elevation, distance],
                    artifact.metadata.position_interpolation,
                )
            }
        };
        RenderResponse { gains }
    }

    fn save_to_file(&self, path: &std::path::Path, _speaker_layout: &SpeakerLayout) -> Result<()> {
        self.artifact.save_to_file(path)
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

pub fn build_from_artifact_render_engine(
    artifact: LoadedEvaluationArtifact,
) -> super::PreparedRenderEngine {
    let backend_restore_snapshot = artifact.backend_restore_snapshot();
    super::PreparedRenderEngine::new(
        super::GainModelKind::FromFile,
        "from_file",
        "From File",
        artifact.capabilities(),
        EffectiveEvaluationMode::FromFile,
        backend_restore_snapshot,
        Box::new(EvaluationArtifactEvaluator::new(artifact)),
    )
}

pub fn build_backend_restore_snapshot(
    source_backend_id: &str,
    source_backend_label: &str,
    mode: SerializedEvaluationMode,
    config: &EvaluationBuildConfig,
) -> Option<BackendRestoreSnapshot> {
    match source_backend_id {
        "vbap" | "experimental_distance" => Some(BackendRestoreSnapshot {
            backend_id: source_backend_id.to_string(),
            backend_label: source_backend_label.to_string(),
            evaluation_mode: mode,
            position_interpolation: config.position_interpolation,
            allow_negative_z: config.polar.allow_negative_z,
            cartesian_x_size: config.cartesian.x_size.saturating_sub(1),
            cartesian_y_size: config.cartesian.y_size.saturating_sub(1),
            cartesian_z_size: config.cartesian.z_size.saturating_sub(1),
            cartesian_z_neg_size: config.cartesian.z_neg_size,
            polar_azimuth_values: config.polar.azimuth_values.max(2),
            polar_elevation_values: config.polar.elevation_values.max(2),
            polar_distance_res: config.polar.distance_values.saturating_sub(1).max(1),
            polar_distance_max: config.polar.distance_max.max(0.01),
        }),
        _ => None,
    }
}

fn encode_backend_restore_snapshot(
    snapshot: Option<&BackendRestoreSnapshot>,
) -> Result<Option<Vec<u8>>> {
    match snapshot {
        Some(snapshot) => Ok(Some(serde_json::to_vec(snapshot)?)),
        None => Ok(None),
    }
}

fn compress(payload: &[u8]) -> Result<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(6));
    encoder.write_all(payload)?;
    Ok(encoder.finish()?)
}

fn decompress(payload: &[u8]) -> Result<Vec<u8>> {
    let mut decoder = ZlibDecoder::new(payload);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out)?;
    Ok(out)
}

fn write_f32_slice(out: &mut Vec<u8>, values: &[f32]) -> Result<()> {
    for value in values {
        out.write_all(&value.to_le_bytes())?;
    }
    Ok(())
}

fn read_u32(reader: &mut Cursor<&[u8]>) -> Result<u32> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_f32_vec(reader: &mut Cursor<&[u8]>, count: usize) -> Result<Vec<f32>> {
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let mut bytes = [0u8; 4];
        reader.read_exact(&mut bytes)?;
        out.push(f32::from_le_bytes(bytes));
    }
    Ok(out)
}
