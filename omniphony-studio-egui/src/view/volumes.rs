//! Energy volume providers (`scene/object-energy-volume.js`,
//! `global-energy-volume.js`, `speaker-solo-volume.js`,
//! `discontinuity-volume.js`) sampling their `n³` field on the CPU at the
//! `volumeRefreshMs` cadence, exactly as the Studio does.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::model::app_state::RoomRatio;
use crate::osc::apply::GainTable;
use crate::osc::dispatch::Live;
use crate::render::volume::{VolumeData, VolumeDraw, VolumeUniforms, f16_bits, model_matrix};

use super::RoomBounds;

/// `OBJECT_ENERGY_COLORMAPS` indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Colormap {
    Heatmap = 0,
    BlueWhite = 1,
    WhiteRed = 2,
    Red = 3,
    Custom = 4,
}

impl Colormap {
    pub const ALL: [Colormap; 5] = [
        Colormap::Heatmap,
        Colormap::BlueWhite,
        Colormap::WhiteRed,
        Colormap::Red,
        Colormap::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Colormap::Heatmap => "Heatmap",
            Colormap::BlueWhite => "Blue → white",
            Colormap::WhiteRed => "White → red",
            Colormap::Red => "Red",
            Colormap::Custom => "Custom",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GradientStop {
    pub pos: f32,
    pub rgb: [f32; 3],
}

pub fn default_stops() -> Vec<GradientStop> {
    vec![
        GradientStop {
            pos: 0.0,
            rgb: [0.0, 0.0, 1.0],
        },
        GradientStop {
            pos: 0.5,
            rgb: [0.0, 1.0, 0.0],
        },
        GradientStop {
            pos: 1.0,
            rgb: [1.0, 0.0, 0.0],
        },
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiscontinuityMode {
    Gain,
    Centroid,
}

impl DiscontinuityMode {
    pub fn table_index(self) -> i64 {
        match self {
            DiscontinuityMode::Gain => -2,
            DiscontinuityMode::Centroid => -3,
        }
    }
}

/// Shared "Common parameters" block plus each provider's own settings.
#[derive(Clone, Debug)]
pub struct VolumeSettings {
    pub resolution: u32,
    pub opacity: f32,
    pub mix: f32,
    pub gamma_accumulate: f32,
    pub gamma_mip: f32,
    pub refresh_ms: u32,
    pub smooth: bool,
    /// The band the volumes are drawn for — `ViewSettings::heatmap_band_index`,
    /// copied in each frame — and whether they composite every band instead
    /// (`heatmapAllBands`: the index is kept, as the web keeps it).
    pub band_index: usize,
    pub all_bands: bool,
    pub object_field_enabled: bool,
    pub object_colormap: Colormap,
    pub object_radius: f32,
    pub object_stops: Vec<GradientStop>,
    pub global_enabled: bool,
    pub global_scale_db: f32,
    pub speaker_enabled: bool,
    pub speaker_colormap: Colormap,
    pub speaker_stops: Vec<GradientStop>,
    pub discontinuity_enabled: bool,
    pub discontinuity_mode: DiscontinuityMode,
    pub discontinuity_scale: f32,
}

impl Default for VolumeSettings {
    fn default() -> Self {
        Self {
            resolution: 64,
            opacity: 1.0,
            mix: 0.6,
            gamma_accumulate: 4.0,
            gamma_mip: 3.0,
            refresh_ms: 160,
            smooth: false,
            band_index: 0,
            all_bands: true,
            object_field_enabled: false,
            object_colormap: Colormap::BlueWhite,
            object_radius: 0.5,
            object_stops: default_stops(),
            global_enabled: false,
            global_scale_db: 6.0,
            speaker_enabled: false,
            speaker_colormap: Colormap::Heatmap,
            speaker_stops: default_stops(),
            discontinuity_enabled: false,
            discontinuity_mode: DiscontinuityMode::Gain,
            discontinuity_scale: 0.5,
        }
    }
}

/// Provider slots.
pub const SLOT_OBJECT_FIELD: usize = 0;
pub const SLOT_GLOBAL: usize = 1;
pub const SLOT_SPEAKER: usize = 2;
pub const SLOT_DISCONTINUITY: usize = 3;

#[derive(Default)]
struct SlotState {
    last_rebuild: Option<Instant>,
    signature: Option<u64>,
    data: Option<Arc<VolumeData>>,
    /// Max of the sampled field (scalar `.r` or precoloured `.a`) before
    /// normalisation, kept for `uInvMax`.
    max: f32,
}

/// Per-provider rebuild state kept across frames.
#[derive(Default)]
pub struct VolumeState {
    slots: [SlotState; 4],
}

/// One active object for the client-side field, in ADM units.
pub struct ActiveObject {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub energy: f64,
}

/// `collectActiveObjects`: unmuted objects with a finite, positive energy.
pub fn active_objects(live: &Live, settings: &VolumeSettings) -> Vec<ActiveObject> {
    live.app
        .sources
        .iter()
        .filter(|(id, _)| !live.app.object_mutes.get(*id).is_some_and(|m| *m != 0))
        .filter_map(|(id, src)| {
            let level = live.app.source_levels.get(id)?;
            let bands = live.object_band_rms.get(id);
            let rms = match bands {
                Some(b) if !settings.all_bands && b.len() >= 2 => {
                    b[settings.band_index.min(b.len() - 1)]
                }
                // Already decayed on the model (`maintain_meters`).
                _ => level.rms_dbfs,
            };
            let energy = if rms.is_finite() && rms > -100.0 {
                10f64.powf(rms / 10.0)
            } else {
                0.0
            };
            (energy > 0.0 && src.x.is_finite() && src.y.is_finite() && src.z.is_finite()).then(
                || ActiveObject {
                    x: src.x,
                    y: src.y,
                    z: src.z,
                    energy,
                },
            )
        })
        .collect()
}

/// Texel centre coordinates in Omniphony units along each texture axis.
struct Axes {
    depth: Vec<f64>,
    height: Vec<f64>,
    width: Vec<f64>,
}

fn axes(n: u32, bounds: &RoomBounds, room: &RoomRatio) -> Axes {
    use omniphony_geometry::f64 as geometry;
    let n = n as usize;
    let height = room.height.max(1e-3);
    let lower = room.lower.max(1e-3);
    let width = room.width.max(1e-3);
    let x_min = bounds.x_min as f64;
    let x_max = bounds.x_max as f64;
    let y_min = bounds.y_min as f64;
    let y_max = bounds.y_max as f64;
    let z_min = bounds.z_min as f64;
    let z_max = bounds.z_max as f64;
    let depth = (0..n)
        .map(|i| {
            let sx = x_min + (i as f64 + 0.5) / n as f64 * (x_max - x_min);
            geometry::inverse_map_depth(sx, room.length, room.rear, room.center_blend)
        })
        .collect();
    let height = (0..n)
        .map(|j| {
            let sy = y_min + (j as f64 + 0.5) / n as f64 * (y_max - y_min);
            if sy >= 0.0 { sy / height } else { sy / lower }
        })
        .collect();
    let width = (0..n)
        .map(|k| (z_min + (k as f64 + 0.5) / n as f64 * (z_max - z_min)) / width)
        .collect();
    Axes {
        depth,
        height,
        width,
    }
}

/// Fill an `n³` scalar field; returns (texels, max).
fn fill_scalar(n: u32, ax: &Axes, mut sample: impl FnMut(f64, f64, f64) -> f64) -> (Vec<u16>, f32) {
    let n = n as usize;
    let mut raw = vec![0f32; n * n * n];
    let mut max = 0f32;
    for k in 0..n {
        let ow = ax.width[k];
        for j in 0..n {
            let oh = ax.height[j];
            for i in 0..n {
                let od = ax.depth[i];
                let v = sample(ow, od, oh) as f32;
                let v = if v.is_finite() { v } else { 0.0 };
                raw[i + n * (j + n * k)] = v;
                max = max.max(v);
            }
        }
    }
    let inv = if max > 0.0 { 1.0 / max } else { 0.0 };
    let mut texels = Vec::with_capacity(raw.len() * 4);
    for v in raw {
        texels.extend_from_slice(&[f16_bits(v * inv), 0, 0, 0]);
    }
    (texels, max)
}

/// Fill an `n³` precoloured field (`rgb`, level); returns (texels, max level).
fn fill_color(
    n: u32,
    ax: &Axes,
    mut sample: impl FnMut(f64, f64, f64) -> [f32; 4],
) -> (Vec<u16>, f32) {
    let n = n as usize;
    let mut texels = vec![0u16; n * n * n * 4];
    let mut max = 0f32;
    for k in 0..n {
        let ow = ax.width[k];
        for j in 0..n {
            let oh = ax.height[j];
            for i in 0..n {
                let od = ax.depth[i];
                let c = sample(ow, od, oh);
                let base = (i + n * (j + n * k)) * 4;
                texels[base] = f16_bits(c[0]);
                texels[base + 1] = f16_bits(c[1]);
                texels[base + 2] = f16_bits(c[2]);
                texels[base + 3] = f16_bits(c[3]);
                max = max.max(c[3]);
            }
        }
    }
    (texels, max)
}

/// `objectEnergyColor` on the CPU (used by the all-bands speaker path).
pub fn energy_color(cm: Colormap, t: f32, stops: &[GradientStop]) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    match cm {
        Colormap::Red => [1.0, 0.0, 0.0],
        Colormap::BlueWhite => [t, t, 1.0],
        Colormap::WhiteRed => [1.0, 1.0 - t, 1.0 - t],
        Colormap::Custom => {
            if stops.is_empty() {
                return [t, t, t];
            }
            if t <= stops[0].pos {
                return stops[0].rgb;
            }
            for w in stops.windows(2) {
                let (a, b) = (w[0], w[1]);
                if t <= b.pos {
                    let f = if b.pos > a.pos {
                        (t - a.pos) / (b.pos - a.pos)
                    } else {
                        0.0
                    };
                    return [
                        a.rgb[0] + (b.rgb[0] - a.rgb[0]) * f,
                        a.rgb[1] + (b.rgb[1] - a.rgb[1]) * f,
                        a.rgb[2] + (b.rgb[2] - a.rgb[2]) * f,
                    ];
                }
            }
            stops[stops.len() - 1].rgb
        }
        Colormap::Heatmap => {
            let mix = |a: [f32; 3], b: [f32; 3], f: f32| {
                [
                    a[0] + (b[0] - a[0]) * f,
                    a[1] + (b[1] - a[1]) * f,
                    a[2] + (b[2] - a[2]) * f,
                ]
            };
            if t < 0.25 {
                mix([0.0, 0.0, 1.0], [0.0, 1.0, 1.0], t / 0.25)
            } else if t < 0.48 {
                mix([0.0, 1.0, 1.0], [0.0, 1.0, 0.0], (t - 0.25) / 0.23)
            } else if t < 0.70 {
                mix([0.0, 1.0, 0.0], [1.0, 1.0, 0.0], (t - 0.48) / 0.22)
            } else {
                mix([1.0, 1.0, 0.0], [1.0, 0.0, 0.0], (t - 0.70) / 0.30)
            }
        }
    }
}

/// Nearest-node lookup into a band gain table (`makeCellIndexer`).
struct CellIndexer<'a> {
    nx: usize,
    ny: usize,
    z_positions: &'a [f32],
}

impl CellIndexer<'_> {
    fn cell(&self, ow: f64, od: f64, oh: f64) -> usize {
        let nx = self.nx;
        let ny = self.ny;
        let xi = (((ow + 1.0) / 2.0 * (nx - 1) as f64).round() as isize).clamp(0, nx as isize - 1)
            as usize;
        let yi = (((od + 1.0) / 2.0 * (ny - 1) as f64).round() as isize).clamp(0, ny as isize - 1)
            as usize;
        let mut zi = 0usize;
        let mut best = f64::INFINITY;
        for (i, z) in self.z_positions.iter().enumerate() {
            let d = (f64::from(*z) - oh).abs();
            if d < best {
                best = d;
                zi = i;
            }
        }
        xi + nx * (yi + ny * zi)
    }
}

/// Views into an "OBGT" table's payload.
struct BandTable<'a> {
    nx: usize,
    ny: usize,
    nz: usize,
    z_positions: &'a [f32],
    bands: Vec<&'a [f32]>,
    low_hz: Vec<f64>,
    high_hz: Vec<Option<f64>>,
}

fn band_table(table: &GainTable) -> Option<BandTable<'_>> {
    let GainTable::CartesianBands {
        x_count,
        y_count,
        z_count,
        band_count,
        bands,
        data,
        ..
    } = table
    else {
        return None;
    };
    let (nx, ny, nz, nb) = (*x_count, *y_count, *z_count, *band_count);
    let cells = nx * ny * nz;
    let off = nx + ny + nz;
    if nx == 0 || ny == 0 || nz == 0 || nb == 0 || data.len() < off + nb * cells {
        return None;
    }
    let z_positions = &data[nx + ny..off];
    let band_slices = (0..nb)
        .map(|b| &data[off + b * cells..off + (b + 1) * cells])
        .collect();
    Some(BandTable {
        nx,
        ny,
        nz,
        z_positions,
        bands: band_slices,
        low_hz: bands.iter().map(|b| b.low_hz).collect(),
        high_hz: bands.iter().map(|b| b.high_hz).collect(),
    })
}

fn signature(parts: &[u64]) -> u64 {
    let mut h = DefaultHasher::new();
    parts.hash(&mut h);
    h.finish()
}

fn f2u(v: f32) -> u64 {
    u64::from(v.to_bits())
}

fn room_sig(room: &RoomRatio) -> [u64; 5] {
    [
        room.height.to_bits(),
        room.lower.to_bits(),
        room.width.to_bits(),
        room.rear.to_bits(),
        room.length.to_bits(),
    ]
}

/// Common uniform values for one draw.
fn uniforms(
    settings: &VolumeSettings,
    bounds: &RoomBounds,
    n: u32,
    max: f32,
    max_level_override: Option<f32>,
    colormap: Colormap,
    precolored: bool,
    stops: &[GradientStop],
) -> VolumeUniforms {
    let effective_max = max_level_override.unwrap_or(max);
    // Scalar fields are uploaded already divided by their max, so the
    // shader's inv_max is 1 unless an absolute scale is requested.
    let inv_max = match max_level_override {
        Some(m) if m > 0.0 => 1.0 / m,
        Some(_) => 0.0,
        None => {
            if effective_max > 0.0 {
                1.0
            } else {
                0.0
            }
        }
    };
    let steps = ((n * 2) as i32).clamp(32, 384);
    let mut custom = [[0f32; 4]; 8];
    let count = stops.len().min(8);
    for (i, s) in stops.iter().take(8).enumerate() {
        custom[i] = [s.pos, s.rgb[0], s.rgb[1], s.rgb[2]];
    }
    let box_min = [bounds.x_min, bounds.y_min, bounds.z_min];
    let box_max = [bounds.x_max, bounds.y_max, bounds.z_max];
    VolumeUniforms {
        model: model_matrix(box_min, box_max).to_cols_array_2d(),
        box_min: [box_min[0], box_min[1], box_min[2], inv_max],
        box_max: [
            box_max[0],
            box_max[1],
            box_max[2],
            settings.opacity.clamp(0.05, 1.0),
        ],
        params: [
            settings.gamma_accumulate.clamp(1.0, 10.0),
            settings.gamma_mip.clamp(0.2, 3.0),
            64.0 / steps as f32,
            settings.mix.clamp(0.0, 1.0),
        ],
        iparams: [
            colormap as i32,
            steps,
            i32::from(precolored),
            if colormap == Colormap::Custom {
                count as i32
            } else {
                0
            },
        ],
        custom_stops: custom,
    }
}

/// Build the volume draws for this frame, rebuilding fields at the refresh
/// cadence and only when their signature changed (static providers).
#[allow(clippy::too_many_arguments)]
pub fn build(
    live: &Live,
    settings: &VolumeSettings,
    state: &mut VolumeState,
    bounds: &RoomBounds,
    room: &RoomRatio,
    selected_speaker: Option<usize>,
    now: Instant,
) -> Vec<VolumeDraw> {
    let mut draws = Vec::new();
    let n = settings.resolution.clamp(8, 64);
    let refresh = Duration::from_millis(u64::from(settings.refresh_ms.max(40)));
    let due = |slot: &SlotState| {
        slot.last_rebuild
            .is_none_or(|t| now.duration_since(t) >= refresh)
    };
    let room_sig = room_sig(room);

    // --- object energy field (live, no signature) ---
    if settings.object_field_enabled {
        let objects = active_objects(live, settings);
        let slot = &mut state.slots[SLOT_OBJECT_FIELD];
        if objects.is_empty() {
            slot.data = None;
        } else {
            let upload = if due(slot) {
                let ax = axes(n, bounds, room);
                let r0 = f64::from(settings.object_radius.clamp(0.01, 1.0));
                let r0sq = r0 * r0;
                let (texels, max) = fill_scalar(n, &ax, |ow, od, oh| {
                    objects
                        .iter()
                        .map(|o| {
                            let dx = ow - o.x;
                            let dy = od - o.y;
                            let dz = oh - o.z;
                            o.energy / (dx * dx + dy * dy + dz * dz + r0sq)
                        })
                        .sum()
                });
                let data = Arc::new(VolumeData { n, texels });
                slot.data = Some(data.clone());
                slot.max = max;
                slot.last_rebuild = Some(now);
                Some(data)
            } else {
                None
            };
            if slot.data.is_some() {
                draws.push(VolumeDraw {
                    slot: SLOT_OBJECT_FIELD,
                    uniforms: uniforms(
                        settings,
                        bounds,
                        n,
                        slot.max,
                        None,
                        settings.object_colormap,
                        false,
                        &settings.object_stops,
                    ),
                    upload,
                    smooth: settings.smooth,
                });
            }
        }
    }

    // --- global energy deviation (table -1, precoloured, absolute) ---
    if settings.global_enabled
        && let Some(table) = live.gain_tables.get(&-1)
        && let Some(bt) = band_table(table)
    {
        let scale = settings.global_scale_db.clamp(1.0, 40.0);
        let sig = signature(&[
            u64::from(table.version()),
            f2u(scale),
            settings.band_index as u64,
            u64::from(settings.all_bands),
            u64::from(n),
            room_sig[0],
            room_sig[1],
            room_sig[2],
            room_sig[3],
            room_sig[4],
        ]);
        let slot = &mut state.slots[SLOT_GLOBAL];
        let upload = if slot.signature != Some(sig) && due(slot) {
            let ax = axes(n, bounds, room);
            let idx = CellIndexer {
                nx: bt.nx,
                ny: bt.ny,
                z_positions: bt.z_positions,
            };
            let all = settings.all_bands && bt.bands.len() > 1;
            let band = settings.band_index.min(bt.bands.len() - 1);
            let amp = |cell: usize| -> f64 {
                if all {
                    bt.bands
                        .iter()
                        .map(|g| f64::from(g[cell]).powi(2))
                        .sum::<f64>()
                        .sqrt()
                } else {
                    f64::from(bt.bands[band][cell])
                }
            };
            let (texels, max) = fill_color(n, &ax, |ow, od, oh| {
                let a = amp(idx.cell(ow, od, oh));
                let t = if a > 1e-6 {
                    (20.0 * a.log10() / f64::from(scale)).clamp(-1.0, 1.0) as f32
                } else {
                    -1.0
                };
                if t >= 0.0 {
                    [1.0, 0.0, 0.0, t]
                } else {
                    [0.0, 0.35, 1.0, -t]
                }
            });
            let data = Arc::new(VolumeData { n, texels });
            slot.data = Some(data.clone());
            slot.max = max;
            slot.signature = Some(sig);
            slot.last_rebuild = Some(now);
            Some(data)
        } else {
            None
        };
        if slot.data.is_some() {
            draws.push(VolumeDraw {
                slot: SLOT_GLOBAL,
                uniforms: uniforms(
                    settings,
                    bounds,
                    n,
                    slot.max,
                    Some(1.0),
                    Colormap::Heatmap,
                    true,
                    &[],
                ),
                upload,
                smooth: settings.smooth,
            });
        }
    }

    // --- selected speaker heatmap volume ---
    if settings.speaker_enabled
        && let Some(si) = selected_speaker
        && let Some(table) = live.gain_tables.get(&(si as i64))
        && let Some(bt) = band_table(table)
    {
        let all = settings.all_bands && bt.bands.len() > 1;
        let stops_sig: u64 = settings.speaker_stops.iter().fold(0u64, |h, s| {
            h.wrapping_mul(31)
                .wrapping_add(f2u(s.pos) ^ f2u(s.rgb[0]) ^ f2u(s.rgb[1]) ^ f2u(s.rgb[2]))
        });
        let sig = signature(&[
            u64::from(table.version()),
            si as u64,
            u64::from(all),
            settings.band_index as u64,
            settings.speaker_colormap as u64,
            stops_sig,
            u64::from(n),
            room_sig[0],
            room_sig[1],
            room_sig[2],
            room_sig[3],
            room_sig[4],
        ]);
        let slot = &mut state.slots[SLOT_SPEAKER];
        let upload = if slot.signature != Some(sig) && due(slot) {
            let ax = axes(n, bounds, room);
            let idx = CellIndexer {
                nx: bt.nx,
                ny: bt.ny,
                z_positions: bt.z_positions,
            };
            let (texels, max) = if all {
                // Precoloured: level-weighted mix of each band's gradient colour.
                let band_rgb: Vec<[f32; 3]> = (0..bt.bands.len())
                    .map(|b| {
                        let lo = bt.low_hz[b].max(20.0);
                        let hi = bt.high_hz[b]
                            .filter(|h| *h > 0.0)
                            .unwrap_or(20000.0)
                            .min(20000.0);
                        let f = (lo * hi.max(lo)).sqrt().clamp(20.0, 20000.0);
                        let t =
                            ((f.ln() - 20f64.ln()) / (20000f64.ln() - 20f64.ln())).clamp(0.0, 1.0);
                        energy_color(settings.speaker_colormap, t as f32, &settings.speaker_stops)
                    })
                    .collect();
                fill_color(n, &ax, |ow, od, oh| {
                    let cell = idx.cell(ow, od, oh);
                    let mut sum = 0f32;
                    let mut rgb = [0f32; 3];
                    for (b, g) in bt.bands.iter().enumerate() {
                        let lvl = g[cell] * g[cell];
                        sum += lvl;
                        rgb[0] += lvl * band_rgb[b][0];
                        rgb[1] += lvl * band_rgb[b][1];
                        rgb[2] += lvl * band_rgb[b][2];
                    }
                    if sum > 0.0 {
                        [rgb[0] / sum, rgb[1] / sum, rgb[2] / sum, sum]
                    } else {
                        [0.0; 4]
                    }
                })
            } else {
                let band = settings.band_index.min(bt.bands.len() - 1);
                fill_scalar(n, &ax, |ow, od, oh| {
                    let g = f64::from(bt.bands[band][idx.cell(ow, od, oh)]);
                    g * g
                })
            };
            let data = Arc::new(VolumeData { n, texels });
            slot.data = Some(data.clone());
            slot.max = max;
            slot.signature = Some(sig);
            slot.last_rebuild = Some(now);
            Some(data)
        } else {
            None
        };
        if slot.data.is_some() {
            // Precoloured all-bands data is stored unnormalised: normalise
            // through inv_max like the Studio does.
            let override_max = if all { Some(slot.max) } else { None };
            draws.push(VolumeDraw {
                slot: SLOT_SPEAKER,
                uniforms: uniforms(
                    settings,
                    bounds,
                    n,
                    slot.max,
                    override_max,
                    settings.speaker_colormap,
                    all,
                    &settings.speaker_stops,
                ),
                upload,
                smooth: settings.smooth,
            });
        }
    }

    // --- discontinuity (tables -2 / -3, precoloured amber, absolute) ---
    if settings.discontinuity_enabled
        && let Some(table) = live
            .gain_tables
            .get(&settings.discontinuity_mode.table_index())
        && let Some(bt) = band_table(table)
    {
        let scale = settings.discontinuity_scale.clamp(0.05, 2.0);
        let sig = signature(&[
            u64::from(table.version()),
            f2u(scale),
            settings.band_index as u64,
            u64::from(settings.all_bands),
            u64::from(n),
            room_sig[0],
            room_sig[1],
            room_sig[2],
            room_sig[3],
            room_sig[4],
        ]);
        let slot = &mut state.slots[SLOT_DISCONTINUITY];
        let upload = if slot.signature != Some(sig) && due(slot) {
            let ax = axes(n, bounds, room);
            let idx = CellIndexer {
                nx: bt.nx,
                ny: bt.ny,
                z_positions: bt.z_positions,
            };
            let all = settings.all_bands && bt.bands.len() > 1;
            let band = settings.band_index.min(bt.bands.len() - 1);
            let (texels, max) = fill_color(n, &ax, |ow, od, oh| {
                let cell = idx.cell(ow, od, oh);
                let jump = if all {
                    bt.bands.iter().map(|g| g[cell]).fold(0f32, f32::max)
                } else {
                    bt.bands[band][cell]
                };
                let t = (jump / scale).clamp(0.0, 1.0);
                [1.0, 0.65 - 0.45 * t, 0.05, t]
            });
            let data = Arc::new(VolumeData { n, texels });
            slot.data = Some(data.clone());
            slot.max = max;
            slot.signature = Some(sig);
            slot.last_rebuild = Some(now);
            Some(data)
        } else {
            None
        };
        if slot.data.is_some() {
            draws.push(VolumeDraw {
                slot: SLOT_DISCONTINUITY,
                uniforms: uniforms(
                    settings,
                    bounds,
                    n,
                    slot.max,
                    Some(1.0),
                    Colormap::Heatmap,
                    true,
                    &[],
                ),
                upload,
                smooth: settings.smooth,
            });
        }
    }

    draws
}

/// Gain-table targets the enabled providers need (`speaker_index` values of
/// the subscribe control), as the Studio's `acquireGainTable` consumers.
pub fn wanted_tables(settings: &VolumeSettings, selected_speaker: Option<usize>) -> Vec<i64> {
    let mut v = Vec::new();
    if settings.speaker_enabled {
        v.push(selected_speaker.unwrap_or(0) as i64);
    }
    if settings.global_enabled {
        v.push(-1);
    }
    if settings.discontinuity_enabled {
        v.push(settings.discontinuity_mode.table_index());
    }
    v.sort_unstable();
    v.dedup();
    v
}
