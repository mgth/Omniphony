use crate::context::RuntimeControlContext;
use crate::heatmap_sub::HeatmapSubscription;
use renderer::crossover::{FreqBand, compute_bands};
use renderer::live_params::{LiveEvaluationMode, RenderTopology};
use renderer::render_backend::RenderBackendKind;
use renderer::render_backend::{CartesianSpeakerHeatmapSlices, CartesianSpeakerHeatmapVolume};
use rosc::{OscMessage, OscType};
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum BroadcastValue {
    Int(i32),
    Float(f32),
    Fff(f32, f32, f32),
    String(String),
}

#[derive(Debug, Clone)]
pub struct BroadcastUpdate {
    pub addr: String,
    pub value: BroadcastValue,
}

#[derive(Debug, Clone, Default)]
pub struct ControlEffects {
    pub mark_dirty: bool,
    pub trigger_layout_recompute: bool,
    pub broadcasts: Vec<BroadcastUpdate>,
    pub log_message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SpeakerHeatmapRequest {
    request_id: u64,
    speaker_index: usize,
    #[serde(default)]
    band_index: usize,
    mode: String,
    max_samples: Option<usize>,
}

#[derive(Debug, Serialize)]
struct SpeakerHeatmapMetaPayload {
    request_id: u64,
    speaker_index: usize,
    band_index: usize,
    speaker_position: [f32; 3],
}

#[derive(Debug, Serialize)]
struct SpeakerHeatmapSlicePayload {
    request_id: u64,
    speaker_index: usize,
    band_index: usize,
    fixed_axis_value: f32,
    axis_a: Vec<f32>,
    axis_b: Vec<f32>,
    values: Vec<f32>,
}

#[derive(Debug, Serialize)]
struct SpeakerHeatmapUnavailablePayload {
    request_id: u64,
    speaker_index: usize,
    band_index: usize,
    reason: &'static str,
}

#[derive(Debug, Serialize)]
struct SpeakerHeatmapVolumeChunkPayload {
    request_id: u64,
    speaker_index: usize,
    band_index: usize,
    chunk_index: usize,
    chunk_count: usize,
    samples: Vec<f32>,
}

#[derive(Debug, Deserialize)]
struct SpeakerHeatmapSubscribePayload {
    subscription_id: u64,
    speaker_index: usize,
    #[serde(default)]
    band_index: usize,
    /// Subset of {"slices", "volume"}.
    modes: Vec<String>,
    max_samples: Option<usize>,
}

// AdaptiveResamplingPatch / AudioConfigPatch / LiveInputPatch / InputConfigPatch
// moved to the `host_audio` crate alongside their dispatch handlers.

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LayoutSpeakerPatch {
    id: usize,
    name: Option<String>,
    coord_mode: Option<String>,
    x: Option<f32>,
    y: Option<f32>,
    z: Option<f32>,
    azimuth: Option<f32>,
    elevation: Option<f32>,
    distance: Option<f32>,
    spatialize: Option<bool>,
    #[serde(default)]
    freq_low: Option<Option<f32>>,
    #[serde(default)]
    freq_high: Option<Option<f32>>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LayoutAddSpeakerPatch {
    name: Option<String>,
    coord_mode: Option<String>,
    x: Option<f32>,
    y: Option<f32>,
    z: Option<f32>,
    azimuth: Option<f32>,
    elevation: Option<f32>,
    distance: Option<f32>,
    spatialize: Option<bool>,
    delay_ms: Option<f32>,
    #[serde(default)]
    freq_low: Option<Option<f32>>,
    #[serde(default)]
    freq_high: Option<Option<f32>>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LayoutMoveSpeakerPatch {
    from: usize,
    to: usize,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LayoutConfigPatch {
    radius_m: Option<f32>,
    speaker_edits: Option<Vec<LayoutSpeakerPatch>>,
    add_speaker: Option<LayoutAddSpeakerPatch>,
    remove_speaker: Option<usize>,
    move_speaker: Option<LayoutMoveSpeakerPatch>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SpeakersRuntimePatch {
    id: usize,
    muted: Option<bool>,
    delay_ms: Option<f32>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct SpeakersConfigPatch {
    speaker_edits: Option<Vec<SpeakersRuntimePatch>>,
}

fn build_constant_slices_from_reference(
    reference: CartesianSpeakerHeatmapSlices,
    value: f32,
) -> CartesianSpeakerHeatmapSlices {
    let xy_len = reference.x_positions.len() * reference.y_positions.len();
    let xz_len = reference.x_positions.len() * reference.z_positions.len();
    let yz_len = reference.y_positions.len() * reference.z_positions.len();
    CartesianSpeakerHeatmapSlices {
        speaker_index: reference.speaker_index,
        speaker_position: reference.speaker_position,
        x_positions: reference.x_positions,
        y_positions: reference.y_positions,
        z_positions: reference.z_positions,
        xy_values: vec![value; xy_len],
        xz_values: vec![value; xz_len],
        yz_values: vec![value; yz_len],
    }
}

fn build_constant_volume_samples(
    reference: &CartesianSpeakerHeatmapSlices,
    value: f32,
    max_samples: usize,
) -> Vec<f32> {
    if value <= 0.0 {
        return Vec::new();
    }
    let total =
        reference.x_positions.len() * reference.y_positions.len() * reference.z_positions.len();
    if total == 0 {
        return Vec::new();
    }
    let sample_count = if max_samples > 0 {
        total.min(max_samples)
    } else {
        total
    };
    let mut samples = Vec::with_capacity(sample_count * 4);
    for sample_index in 0..sample_count {
        let flat_index = if sample_count == total {
            sample_index
        } else {
            ((sample_index as f64 * total as f64) / sample_count as f64).floor() as usize
        };
        let x_len = reference.x_positions.len();
        let y_len = reference.y_positions.len();
        let xy_len = x_len * y_len;
        let z_index = flat_index / xy_len;
        let rem = flat_index % xy_len;
        let y_index = rem / x_len;
        let x_index = rem % x_len;
        samples.extend_from_slice(&[
            reference.x_positions[x_index],
            reference.y_positions[y_index],
            reference.z_positions[z_index],
            value,
        ]);
    }
    samples
}

/// Compute the OSC broadcasts that publish a heatmap for a given speaker for a
/// single mode. Convenience wrapper over [`compute_speaker_heatmap_broadcasts_modes`]
/// for the legacy single-mode `/control/debug/speaker_heatmap/request` route.
pub fn compute_speaker_heatmap_broadcasts(
    ctx: &RuntimeControlContext,
    request_id: u64,
    speaker_index: usize,
    band_index: usize,
    mode: &str,
    max_samples: usize,
) -> Vec<BroadcastUpdate> {
    compute_speaker_heatmap_broadcasts_modes(
        ctx,
        request_id,
        speaker_index,
        band_index,
        std::slice::from_ref(&mode),
        max_samples,
    )
}

/// Compute the OSC broadcasts that publish a heatmap for a given speaker, for
/// one or more modes (`"slices"` and/or `"volume"`).
///
/// Two optimisations vs computing each mode independently:
///   1. The band-specific `RenderTopology` (the expensive part — generates the
///      cartesian VBAP cache) is built at most once per call and shared across
///      modes.
///   2. Built topologies are cached per `band_index` in
///      `ctx.band_topology_cache`. Subsequent calls reuse them until the main
///      layout is recomputed (the cache is cleared post-`publish_topology`).
///
/// Pure with respect to OSC dispatch — caller is responsible for routing the
/// returned updates.
pub fn compute_speaker_heatmap_broadcasts_modes(
    ctx: &RuntimeControlContext,
    request_id: u64,
    speaker_index: usize,
    band_index: usize,
    modes: &[&str],
    max_samples: usize,
) -> Vec<BroadcastUpdate> {
    let mut out: Vec<BroadcastUpdate> = Vec::new();
    let max_samples = max_samples.clamp(128, 20000);
    let topology = ctx.renderer.active_topology();
    let speaker = topology.speaker_layout.speakers.get(speaker_index);
    let unavailable_reason = match speaker {
        None => Some("speaker_not_found"),
        Some(_)
            if topology.backend.evaluation_mode()
                != renderer::render_backend::EffectiveEvaluationMode::PrecomputedCartesian =>
        {
            Some("evaluation_mode_not_precomputed_cartesian")
        }
        Some(_)
            if topology
                .backend_speaker_index_for_layout_speaker(speaker_index)
                .is_none() =>
        {
            Some("speaker_not_spatializable")
        }
        _ => None,
    };

    if let Some(reason) = unavailable_reason {
        let json = serde_json::to_string(&SpeakerHeatmapUnavailablePayload {
            request_id,
            speaker_index,
            band_index,
            reason,
        })
        .unwrap_or_else(|_| "{}".to_string());
        out.push(BroadcastUpdate {
            addr: "/omniphony/state/debug/speaker_heatmap/unavailable".to_string(),
            value: BroadcastValue::String(json),
        });
        return out;
    }

    let Some(speaker) = speaker else {
        return out;
    };
    let Some(backend_speaker_index) =
        topology.backend_speaker_index_for_layout_speaker(speaker_index)
    else {
        return out;
    };

    let bands = compute_bands(&topology.speaker_layout);
    let Some(selected_band) = bands.get(band_index) else {
        let json = serde_json::to_string(&SpeakerHeatmapUnavailablePayload {
            request_id,
            speaker_index,
            band_index,
            reason: "band_not_found",
        })
        .unwrap_or_else(|_| "{}".to_string());
        out.push(BroadcastUpdate {
            addr: "/omniphony/state/debug/speaker_heatmap/unavailable".to_string(),
            value: BroadcastValue::String(json),
        });
        return out;
    };

    let speaker_position = [speaker.x, speaker.y, speaker.z];
    let band_layout_index = selected_band
        .speaker_indices
        .iter()
        .position(|&index| index == speaker_index);

    // Build (or fetch from cache) the band-specific topology exactly once.
    // band_topology depends only on the band composition + main layout; it does
    // not depend on which speaker is selected nor on its position, so it is
    // safely shared across speakers and across slices/volume modes.
    let band_topology: Option<Arc<RenderTopology>> =
        if selected_band.speaker_indices.len() >= 3 && band_layout_index.is_some() {
            if let Some(cached) = ctx.band_topology_cache.get(band_index) {
                Some(cached)
            } else {
                let band_layout = renderer::speaker_layout::SpeakerLayout {
                    radius_m: topology.speaker_layout.radius_m,
                    speakers: selected_band
                        .speaker_indices
                        .iter()
                        .map(|&index| topology.speaker_layout.speakers[index].clone())
                        .collect(),
                };
                let built = ctx
                    .renderer
                    .prepare_topology_rebuild_for_layout(band_layout)
                    .and_then(|plan| plan.build_topology().ok())
                    .map(Arc::new);
                if let Some(arc) = &built {
                    ctx.band_topology_cache.insert(band_index, Arc::clone(arc));
                }
                built
            }
        } else {
            None
        };

    let reference_slices = topology
        .backend
        .cartesian_slices_for_speaker(backend_speaker_index, speaker_position);
    let band_slices = if selected_band.speaker_indices.len() >= 3 {
        if let (Some(layout_index), Some(band_topology)) = (band_layout_index, &band_topology) {
            band_topology
                .backend_speaker_index_for_layout_speaker(layout_index)
                .and_then(|band_backend_index| {
                    band_topology
                        .backend
                        .cartesian_slices_for_speaker(band_backend_index, speaker_position)
                })
        } else {
            reference_slices
                .clone()
                .map(|reference| build_constant_slices_from_reference(reference, 0.0))
        }
    } else {
        let fallback_value =
            if band_layout_index.is_some() && !selected_band.speaker_indices.is_empty() {
                1.0 / (selected_band.speaker_indices.len() as f32).sqrt()
            } else {
                0.0
            };
        reference_slices
            .clone()
            .map(|reference| build_constant_slices_from_reference(reference, fallback_value))
    };

    let meta = serde_json::to_string(&SpeakerHeatmapMetaPayload {
        request_id,
        speaker_index,
        band_index,
        speaker_position,
    })
    .unwrap_or_else(|_| "{}".to_string());
    out.push(BroadcastUpdate {
        addr: "/omniphony/state/debug/speaker_heatmap/meta".to_string(),
        value: BroadcastValue::String(meta),
    });

    for &mode in modes {
        if mode == "volume" {
            push_volume_broadcasts(
                &mut out,
                ctx,
                request_id,
                speaker_index,
                band_index,
                selected_band,
                band_layout_index,
                band_topology.as_deref(),
                band_slices.as_ref(),
                max_samples,
            );
        } else if mode == "slices" {
            push_slices_broadcasts(
                &mut out,
                request_id,
                speaker_index,
                band_index,
                band_slices.as_ref(),
            );
        }
    }

    out
}

fn push_slices_broadcasts(
    out: &mut Vec<BroadcastUpdate>,
    request_id: u64,
    speaker_index: usize,
    band_index: usize,
    band_slices: Option<&CartesianSpeakerHeatmapSlices>,
) {
    let Some(slices) = band_slices else {
        let json = serde_json::to_string(&SpeakerHeatmapUnavailablePayload {
            request_id,
            speaker_index,
            band_index,
            reason: "band_heatmap_unavailable",
        })
        .unwrap_or_else(|_| "{}".to_string());
        out.push(BroadcastUpdate {
            addr: "/omniphony/state/debug/speaker_heatmap/unavailable".to_string(),
            value: BroadcastValue::String(json),
        });
        return;
    };
    for (addr_suffix, fixed_axis_value, axis_a, axis_b, values) in [
        (
            "slice_xy",
            slices.speaker_position[2],
            slices.x_positions.clone(),
            slices.y_positions.clone(),
            slices.xy_values.clone(),
        ),
        (
            "slice_xz",
            slices.speaker_position[1],
            slices.x_positions.clone(),
            slices.z_positions.clone(),
            slices.xz_values.clone(),
        ),
        (
            "slice_yz",
            slices.speaker_position[0],
            slices.y_positions.clone(),
            slices.z_positions.clone(),
            slices.yz_values.clone(),
        ),
    ] {
        let json = serde_json::to_string(&SpeakerHeatmapSlicePayload {
            request_id,
            speaker_index,
            band_index,
            fixed_axis_value,
            axis_a,
            axis_b,
            values,
        })
        .unwrap_or_else(|_| "{}".to_string());
        out.push(BroadcastUpdate {
            addr: format!("/omniphony/state/debug/speaker_heatmap/{addr_suffix}"),
            value: BroadcastValue::String(json),
        });
    }
}

#[allow(clippy::too_many_arguments)]
fn push_volume_broadcasts(
    out: &mut Vec<BroadcastUpdate>,
    _ctx: &RuntimeControlContext,
    request_id: u64,
    speaker_index: usize,
    band_index: usize,
    selected_band: &FreqBand,
    band_layout_index: Option<usize>,
    band_topology: Option<&RenderTopology>,
    band_slices: Option<&CartesianSpeakerHeatmapSlices>,
    max_samples: usize,
) {
    let volume = if selected_band.speaker_indices.len() >= 3 {
        if let (Some(layout_index), Some(band_topology)) = (band_layout_index, band_topology) {
            band_topology
                .backend_speaker_index_for_layout_speaker(layout_index)
                .and_then(|band_backend_index| {
                    band_topology.backend.cartesian_volume_for_speaker(
                        band_backend_index,
                        0.0,
                        max_samples,
                    )
                })
        } else {
            Some(CartesianSpeakerHeatmapVolume {
                speaker_index,
                samples: Vec::new(),
            })
        }
    } else {
        let fallback_value =
            if band_layout_index.is_some() && !selected_band.speaker_indices.is_empty() {
                1.0 / (selected_band.speaker_indices.len() as f32).sqrt()
            } else {
                0.0
            };
        band_slices.map(|slices| CartesianSpeakerHeatmapVolume {
            speaker_index,
            samples: build_constant_volume_samples(slices, fallback_value, max_samples),
        })
    };

    let Some(volume) = volume else {
        let json = serde_json::to_string(&SpeakerHeatmapUnavailablePayload {
            request_id,
            speaker_index,
            band_index,
            reason: "band_heatmap_unavailable",
        })
        .unwrap_or_else(|_| "{}".to_string());
        out.push(BroadcastUpdate {
            addr: "/omniphony/state/debug/speaker_heatmap/unavailable".to_string(),
            value: BroadcastValue::String(json),
        });
        return;
    };
    // Keep OSC/UDP packets comfortably below MTU once the JSON payload
    // wraps the float array.
    const CHUNK_FLOATS: usize = 16 * 4;
    let chunk_count = volume.samples.len().div_ceil(CHUNK_FLOATS).max(1);
    if volume.samples.is_empty() {
        let json = serde_json::to_string(&SpeakerHeatmapVolumeChunkPayload {
            request_id,
            speaker_index,
            band_index,
            chunk_index: 0,
            chunk_count: 1,
            samples: Vec::new(),
        })
        .unwrap_or_else(|_| "{}".to_string());
        out.push(BroadcastUpdate {
            addr: "/omniphony/state/debug/speaker_heatmap/volume_chunk".to_string(),
            value: BroadcastValue::String(json),
        });
    } else {
        for (chunk_index, chunk) in volume.samples.chunks(CHUNK_FLOATS).enumerate() {
            let json = serde_json::to_string(&SpeakerHeatmapVolumeChunkPayload {
                request_id,
                speaker_index,
                band_index,
                chunk_index,
                chunk_count,
                samples: chunk.to_vec(),
            })
            .unwrap_or_else(|_| "{}".to_string());
            out.push(BroadcastUpdate {
                addr: "/omniphony/state/debug/speaker_heatmap/volume_chunk".to_string(),
                value: BroadcastValue::String(json),
            });
        }
    }
}

fn hash_broadcasts(broadcasts: &[BroadcastUpdate]) -> u64 {
    // Deliberately ignore request_id-bearing JSON fields: we hash only the
    // actual heatmap content (speaker_position + axes + values + slice/volume
    // structure). Since broadcasts here always contain `BroadcastValue::String`,
    // hashing the raw JSON works — but the JSON includes `request_id`, which we
    // want to ignore for the cache check (otherwise every call would be "new"
    // because we increment the subscription id's request usage). Strip a fixed
    // `"request_id":N,` substring before hashing.
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for update in broadcasts {
        update.addr.hash(&mut hasher);
        if let BroadcastValue::String(s) = &update.value {
            // Find and skip the `"request_id":<digits>,` segment if present.
            if let Some(rid_start) = s.find("\"request_id\":") {
                let prefix = &s[..rid_start];
                let rest = &s[rid_start + 13..];
                let rid_end = rest.find([',', '}']).unwrap_or(rest.len());
                prefix.hash(&mut hasher);
                rest[rid_end..].hash(&mut hasher);
            } else {
                s.hash(&mut hasher);
            }
        }
    }
    hasher.finish()
}

/// Recompute the heatmap for the active subscription (if any) and return the
/// broadcasts to emit only if the payload differs from the last one cached.
/// Returns an empty Vec when there is no subscription, or when the payload is
/// unchanged since the last push (which is the common case during edits that
/// don't actually move the speaker).
pub fn republish_heatmap_if_changed(ctx: &RuntimeControlContext) -> Vec<BroadcastUpdate> {
    let Some(sub) = ctx.heatmap_sub.current() else {
        return Vec::new();
    };
    if sub.modes.is_empty() {
        return Vec::new();
    }
    // request_id 0 is fine here — hash_broadcasts strips the field. The studio
    // subscriber should accept all incoming heatmap pushes while subscribed,
    // not filter by request_id.
    let modes_refs: Vec<&str> = sub.modes.iter().map(|m| m.as_str()).collect();
    let broadcasts = compute_speaker_heatmap_broadcasts_modes(
        ctx,
        0,
        sub.speaker_index,
        sub.band_index,
        &modes_refs,
        sub.max_samples,
    );
    let hash = hash_broadcasts(&broadcasts);
    if ctx.heatmap_sub.update_hash_if_changed(hash) {
        broadcasts
    } else {
        Vec::new()
    }
}

pub fn parse_bool_arg(arg: Option<&OscType>) -> Option<bool> {
    match arg {
        Some(OscType::Int(i)) => Some(*i != 0),
        Some(OscType::Float(f)) => Some(*f != 0.0),
        _ => None,
    }
}

pub fn parse_positive_u32_arg(arg: Option<&OscType>) -> Option<u32> {
    match arg {
        Some(OscType::Int(i)) if *i > 0 => Some(*i as u32),
        Some(OscType::Float(f)) if *f > 0.0 => Some(*f as u32),
        _ => None,
    }
}

pub fn parse_nonnegative_u32_arg(arg: Option<&OscType>) -> Option<u32> {
    match arg {
        Some(OscType::Int(i)) if *i >= 0 => Some(*i as u32),
        Some(OscType::Float(f)) if *f >= 0.0 => Some(*f as u32),
        _ => None,
    }
}

pub fn parse_positive_f32_arg(arg: Option<&OscType>) -> Option<f32> {
    match arg {
        Some(OscType::Float(f)) if *f > 0.0 => Some(*f),
        Some(OscType::Int(i)) if *i > 0 => Some(*i as f32),
        _ => None,
    }
}

pub fn parse_nonnegative_f32_arg(arg: Option<&OscType>) -> Option<f32> {
    match arg {
        Some(OscType::Float(f)) if *f >= 0.0 => Some(*f),
        Some(OscType::Int(i)) if *i >= 0 => Some(*i as f32),
        _ => None,
    }
}

pub fn parse_f32_arg(arg: Option<&OscType>) -> Option<f32> {
    match arg {
        Some(OscType::Float(f)) => Some(*f),
        Some(OscType::Int(i)) => Some(*i as f32),
        _ => None,
    }
}

pub fn parse_string_arg(arg: Option<&OscType>) -> Option<String> {
    match arg {
        Some(OscType::String(s)) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        _ => None,
    }
}

pub fn parse_input_layout_arg(
    arg: Option<&OscType>,
) -> Option<renderer::speaker_layout::SpeakerLayout> {
    let raw = parse_string_arg(arg)?;
    serde_yaml_ng::from_str::<renderer::speaker_layout::SpeakerLayout>(&raw).ok()
}

fn spherical_to_cartesian(azimuth: f32, elevation: f32, distance: f32) -> (f32, f32, f32) {
    let az = azimuth.to_radians();
    let el = elevation.to_radians();
    let horizontal = distance * el.cos();
    let x = horizontal * az.sin();
    let y = horizontal * az.cos();
    let z = distance * el.sin();
    (x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0), z.clamp(-1.0, 1.0))
}

fn cartesian_to_spherical(x: f32, y: f32, z: f32) -> (f32, f32, f32) {
    let dist = (x * x + y * y + z * z).sqrt();
    let az = x.atan2(y).to_degrees();
    let el = if dist > 0.0 {
        z.atan2((x * x + y * y).sqrt()).to_degrees()
    } else {
        0.0
    };
    (az, el, dist.max(0.01))
}

fn remap_live_speakers_remove(
    speakers: &mut std::collections::HashMap<usize, renderer::live_params::SpeakerLiveParams>,
    remove_idx: usize,
) {
    let mut next = std::collections::HashMap::new();
    for (idx, params) in speakers.drain() {
        if idx == remove_idx {
            continue;
        }
        let mapped = if idx > remove_idx { idx - 1 } else { idx };
        next.insert(mapped, params);
    }
    *speakers = next;
}

pub fn parse_json_string_arg<T: for<'de> Deserialize<'de>>(arg: Option<&OscType>) -> Option<T> {
    let OscType::String(value) = arg? else {
        return None;
    };
    serde_json::from_str(value).ok()
}

// build_audio_state_json / build_input_state_json / push_audio_domain_broadcasts
// / push_input_domain_broadcasts moved to the `host_audio` crate.

fn remap_live_speakers_move(
    speakers: &mut std::collections::HashMap<usize, renderer::live_params::SpeakerLiveParams>,
    from: usize,
    to: usize,
) {
    if from == to {
        return;
    }
    let moved = speakers.remove(&from);
    let mut next = std::collections::HashMap::new();
    for (idx, params) in speakers.drain() {
        let mapped = if from < to {
            if idx > from && idx <= to {
                idx - 1
            } else {
                idx
            }
        } else if idx >= to && idx < from {
            idx + 1
        } else {
            idx
        };
        next.insert(mapped, params);
    }
    if let Some(params) = moved {
        next.insert(to, params);
    }
    *speakers = next;
}

fn normalize_coord_mode(mode: Option<&str>) -> &'static str {
    if mode.is_some_and(|value| value.eq_ignore_ascii_case("cartesian")) {
        "cartesian"
    } else {
        "polar"
    }
}

fn apply_layout_speaker_patch(
    speaker: &mut renderer::speaker_layout::Speaker,
    patch: &LayoutSpeakerPatch,
) -> bool {
    let mut changed = false;
    if let Some(name) = patch
        .name
        .as_ref()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    {
        if speaker.name != name {
            speaker.name = name.to_string();
            changed = true;
        }
    }
    if let Some(spatialize) = patch.spatialize {
        if speaker.spatialize != spatialize {
            speaker.spatialize = spatialize;
            changed = true;
        }
    }
    if let Some(freq_low) = patch.freq_low {
        let next = freq_low.filter(|value| *value > 0.0);
        if speaker.freq_low != next {
            speaker.freq_low = next;
            changed = true;
        }
    }
    if let Some(freq_high) = patch.freq_high {
        let next = freq_high.filter(|value| *value > 0.0);
        if speaker.freq_high != next {
            speaker.freq_high = next;
            changed = true;
        }
    }
    if let Some(coord_mode) = patch.coord_mode.as_deref() {
        let normalized = normalize_coord_mode(Some(coord_mode)).to_string();
        if speaker.coord_mode != normalized {
            speaker.coord_mode = normalized;
            changed = true;
        }
    }

    // A speaker carries both cartesian (x/y/z) and polar (azimuth/elevation/distance)
    // representations. A patch may legitimately contain only one, but some clients
    // send both (typically the cartesian as authoritative + polar as an
    // already-derived snapshot whose convention may not match ours). When both
    // blocks are present, honour `coord_mode` and ignore the other block — never
    // apply both, otherwise the second one silently overwrites the first.
    let has_cartesian = patch.x.is_some() || patch.y.is_some() || patch.z.is_some();
    let has_polar =
        patch.azimuth.is_some() || patch.elevation.is_some() || patch.distance.is_some();
    let coord_mode = speaker.coord_mode.as_str();
    let use_cartesian = has_cartesian && (!has_polar || coord_mode == "cartesian");
    let use_polar = has_polar && (!has_cartesian || coord_mode == "polar");

    if use_cartesian {
        let x = patch.x.unwrap_or(speaker.x).clamp(-1.0, 1.0);
        let y = patch.y.unwrap_or(speaker.y).clamp(-1.0, 1.0);
        let z = patch.z.unwrap_or(speaker.z).clamp(-1.0, 1.0);
        let (azimuth, elevation, distance) = cartesian_to_spherical(x, y, z);
        if speaker.x != x
            || speaker.y != y
            || speaker.z != z
            || speaker.azimuth != azimuth
            || speaker.elevation != elevation
            || speaker.distance != distance
        {
            speaker.x = x;
            speaker.y = y;
            speaker.z = z;
            speaker.azimuth = azimuth;
            speaker.elevation = elevation;
            speaker.distance = distance;
            changed = true;
        }
    } else if use_polar {
        let azimuth = patch
            .azimuth
            .unwrap_or(speaker.azimuth)
            .clamp(-180.0, 180.0);
        let elevation = patch
            .elevation
            .unwrap_or(speaker.elevation)
            .clamp(-90.0, 90.0);
        let distance = patch.distance.unwrap_or(speaker.distance).max(0.01);
        let (x, y, z) = spherical_to_cartesian(azimuth, elevation, distance);
        if speaker.azimuth != azimuth
            || speaker.elevation != elevation
            || speaker.distance != distance
            || speaker.x != x
            || speaker.y != y
            || speaker.z != z
        {
            speaker.azimuth = azimuth;
            speaker.elevation = elevation;
            speaker.distance = distance;
            speaker.x = x;
            speaker.y = y;
            speaker.z = z;
            changed = true;
        }
    }
    changed
}

fn build_layout_speaker_from_patch(
    patch: LayoutAddSpeakerPatch,
    default_name: String,
) -> renderer::speaker_layout::Speaker {
    let name = patch
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(default_name.as_str())
        .to_string();
    let spatialize = patch.spatialize.unwrap_or(true);
    let delay_ms = patch.delay_ms.unwrap_or(0.0).max(0.0);
    // Bootstrap with the polar form (defaults are well-defined for it). The
    // coord_mode + cart/polar block selection below decides which representation
    // wins if both are present in the patch — same rule as apply_layout_speaker_patch.
    let mut speaker = renderer::speaker_layout::Speaker::from_polar(
        name,
        patch.azimuth.unwrap_or(0.0).clamp(-180.0, 180.0),
        patch.elevation.unwrap_or(0.0).clamp(-90.0, 90.0),
        patch.distance.unwrap_or(1.0).max(0.01),
        spatialize,
        delay_ms,
    );
    if let Some(freq_low) = patch.freq_low {
        speaker.freq_low = freq_low.filter(|value| *value > 0.0);
    }
    if let Some(freq_high) = patch.freq_high {
        speaker.freq_high = freq_high.filter(|value| *value > 0.0);
    }
    if patch.coord_mode.as_deref().is_some() {
        speaker.coord_mode = normalize_coord_mode(patch.coord_mode.as_deref()).to_string();
    }

    let has_cartesian = patch.x.is_some() || patch.y.is_some() || patch.z.is_some();
    let has_polar =
        patch.azimuth.is_some() || patch.elevation.is_some() || patch.distance.is_some();
    let coord_mode = speaker.coord_mode.as_str();
    let use_cartesian = has_cartesian && (!has_polar || coord_mode == "cartesian");

    if use_cartesian {
        let x = patch.x.unwrap_or(speaker.x).clamp(-1.0, 1.0);
        let y = patch.y.unwrap_or(speaker.y).clamp(-1.0, 1.0);
        let z = patch.z.unwrap_or(speaker.z).clamp(-1.0, 1.0);
        let (azimuth, elevation, distance) = cartesian_to_spherical(x, y, z);
        speaker.x = x;
        speaker.y = y;
        speaker.z = z;
        speaker.azimuth = azimuth;
        speaker.elevation = elevation;
        speaker.distance = distance;
    }
    // Polar branch: from_polar already initialised x/y/z from polar — nothing to do.
    speaker
}

pub fn apply_simple_osc_control(
    msg: &OscMessage,
    ctx: &RuntimeControlContext,
) -> Option<ControlEffects> {
    let addr = msg.addr.as_str();
    let mut effects = ControlEffects::default();


    if addr == "/omniphony/control/config/layout" {
        let patch = parse_json_string_arg::<LayoutConfigPatch>(msg.args.first());
        if let Some(patch) = patch {
            let mut changed = false;

            if let Some(radius_m) = patch.radius_m {
                let radius_m = radius_m.max(0.01);
                changed |= ctx.renderer.with_editable_layout(|layout| {
                    if (layout.radius_m - radius_m).abs() > f32::EPSILON {
                        layout.radius_m = radius_m;
                        true
                    } else {
                        false
                    }
                });
            }

            if let Some(add_speaker) = patch.add_speaker {
                let idx = ctx.renderer.editable_layout().speakers.len();
                let speaker = build_layout_speaker_from_patch(add_speaker, format!("spk-{idx}"));
                let delay_ms = speaker.delay_ms;
                ctx.renderer.with_editable_layout(|layout| {
                    layout.speakers.push(speaker);
                });
                if delay_ms > 0.0 {
                    ctx.renderer
                        .live
                        .write()
                        .unwrap()
                        .speakers
                        .entry(idx)
                        .or_default()
                        .delay_ms = delay_ms;
                    ctx.renderer.mark_speaker_params_dirty();
                }
                changed = true;
            }

            if let Some(remove_idx) = patch.remove_speaker {
                let removed = ctx.renderer.with_editable_layout(|layout| {
                    if remove_idx >= layout.speakers.len() {
                        false
                    } else {
                        layout.speakers.remove(remove_idx);
                        true
                    }
                });
                if removed {
                    {
                        let mut live = ctx.renderer.live.write().unwrap();
                        remap_live_speakers_remove(&mut live.speakers, remove_idx);
                    }
                    ctx.renderer.mark_speaker_params_dirty();
                    changed = true;
                }
            }

            if let Some(move_speaker) = patch.move_speaker {
                let moved = ctx.renderer.with_editable_layout(|layout| {
                    let len = layout.speakers.len();
                    if move_speaker.from >= len
                        || move_speaker.to >= len
                        || move_speaker.from == move_speaker.to
                    {
                        false
                    } else {
                        let speaker = layout.speakers.remove(move_speaker.from);
                        layout.speakers.insert(move_speaker.to, speaker);
                        true
                    }
                });
                if moved {
                    {
                        let mut live = ctx.renderer.live.write().unwrap();
                        remap_live_speakers_move(
                            &mut live.speakers,
                            move_speaker.from,
                            move_speaker.to,
                        );
                    }
                    ctx.renderer.mark_speaker_params_dirty();
                    changed = true;
                }
            }

            if let Some(speaker_edits) = patch.speaker_edits {
                changed |= ctx.renderer.with_editable_layout(|layout| {
                    let mut any = false;
                    for speaker_patch in &speaker_edits {
                        if let Some(speaker) = layout.speakers.get_mut(speaker_patch.id) {
                            any |= apply_layout_speaker_patch(speaker, speaker_patch);
                        }
                    }
                    any
                });
            }

            // Stage-only: do NOT broadcast the full state bundle here. The change is
            // pending until /omniphony/control/config/layout/apply commits it; the
            // apply path is responsible for the broadcast. This avoids a 3x
            // amplification (stage → broadcast, apply → broadcast, recompute →
            // broadcast) for a single user edit, which previously fed back into the
            // studio's heatmap pull and saturated the renderer.
            let _ = changed;
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/config/layout/apply" {
        effects.mark_dirty = true;
        effects.trigger_layout_recompute = true;
        effects.log_message = Some("OSC: layout config apply".to_string());
        return Some(effects);
    }

    if addr == "/omniphony/control/config/speakers" {
        let patch = parse_json_string_arg::<SpeakersConfigPatch>(msg.args.first());
        if let Some(patch) = patch {
            let mut changed = false;
            if let Some(speaker_edits) = patch.speaker_edits {
                for speaker_patch in speaker_edits {
                    if let Some(delay_ms) = speaker_patch.delay_ms.map(|value| value.max(0.0)) {
                        ctx.renderer
                            .live
                            .write()
                            .unwrap()
                            .speakers
                            .entry(speaker_patch.id)
                            .or_default()
                            .delay_ms = delay_ms;
                        ctx.renderer.with_editable_layout(|layout| {
                            if let Some(speaker) = layout.speakers.get_mut(speaker_patch.id) {
                                speaker.delay_ms = delay_ms;
                            }
                        });
                        ctx.renderer.mark_speaker_params_dirty();
                        changed = true;
                    }
                    if let Some(muted) = speaker_patch.muted {
                        ctx.renderer
                            .live
                            .write()
                            .unwrap()
                            .speakers
                            .entry(speaker_patch.id)
                            .or_default()
                            .muted = muted;
                        ctx.renderer.mark_speaker_params_dirty();
                        changed = true;
                    }
                }
            }
            if changed {
                effects.mark_dirty = true;
            }
        }
        return Some(effects);
    }


    // metering/rate_hz and diag/rate_hz are handled by the OSC layer
    // (orender_engine::osc::dispatch) against RendererControl — the single
    // source of truth for both, persisted to config.

    if addr == "/omniphony/control/render_backend" {
        let requested = parse_string_arg(msg.args.first())
            .and_then(|value| RenderBackendKind::from_str(&value));
        if let Some(requested) = requested {
            let mut live = ctx.renderer.live.write().unwrap();
            if live.backend_id() != requested.as_str() {
                live.backend_id = requested.as_str().to_string();
                effects.mark_dirty = true;
                effects.trigger_layout_recompute = true;
                effects.log_message =
                    Some(format!("OSC: render_backend -> {}", requested.as_str()));
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/render_backend/restore" {
        effects.log_message = Some(
            "OSC: render_backend/restore is no longer supported after removing from_file"
                .to_string(),
        );
        return Some(effects);
    }

    if addr == "/omniphony/control/render_evaluation_mode" {
        let requested = parse_string_arg(msg.args.first())
            .and_then(|value| LiveEvaluationMode::from_str(&value));
        if let Some(requested) = requested {
            let mut live = ctx.renderer.live.write().unwrap();
            if live.evaluation.mode != requested {
                live.set_evaluation_mode(requested);
                effects.mark_dirty = true;
                effects.trigger_layout_recompute = true;
            }
            {
                if live.backend_kind() == Some(RenderBackendKind::Vbap) {
                    effects.mark_dirty = true;
                }
                effects.log_message = Some(format!(
                    "OSC: render_evaluation_mode -> {}",
                    live.requested_evaluation_mode().as_str()
                ));
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/render_evaluation_mode/from_file" {
        effects.log_message =
            Some("OSC: render_evaluation_mode/from_file is no longer supported".to_string());
        return Some(effects);
    }

    if addr == "/omniphony/control/debug/speaker_heatmap/request" {
        let request = parse_string_arg(msg.args.first())
            .and_then(|value| serde_json::from_str::<SpeakerHeatmapRequest>(&value).ok());
        if let Some(request) = request {
            let mode = request.mode.trim().to_ascii_lowercase();
            let max_samples = request.max_samples.unwrap_or(3072).clamp(128, 20000);
            effects
                .broadcasts
                .extend(compute_speaker_heatmap_broadcasts(
                    ctx,
                    request.request_id,
                    request.speaker_index,
                    request.band_index,
                    &mode,
                    max_samples,
                ));
            effects.log_message = Some(format!(
                "OSC: speaker heatmap requested -> speaker={} band={} mode={} request_id={}",
                request.speaker_index, request.band_index, mode, request.request_id
            ));
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/debug/speaker_heatmap/subscribe" {
        let sub = parse_string_arg(msg.args.first())
            .and_then(|value| serde_json::from_str::<SpeakerHeatmapSubscribePayload>(&value).ok());
        if let Some(sub) = sub {
            let modes: Vec<String> = sub
                .modes
                .into_iter()
                .map(|m| m.trim().to_ascii_lowercase())
                .filter(|m| m == "slices" || m == "volume")
                .collect();
            let max_samples = sub.max_samples.unwrap_or(3072).clamp(128, 20000);
            let subscription = HeatmapSubscription {
                speaker_index: sub.speaker_index,
                band_index: sub.band_index,
                modes: modes.clone(),
                max_samples,
            };
            ctx.heatmap_sub.set(subscription);
            // First push: send current heatmap unconditionally so the client gets
            // an initial frame. Subsequent pushes go through republish_heatmap_if_changed.
            // Use request_id = 0 so the studio uses a single accept rule for all
            // pub/sub pushes (the legacy /request route still echoes the
            // caller-supplied request_id and remains backwards-compatible).
            let modes_refs: Vec<&str> = modes.iter().map(|m| m.as_str()).collect();
            effects
                .broadcasts
                .extend(compute_speaker_heatmap_broadcasts_modes(
                    ctx,
                    0,
                    sub.speaker_index,
                    sub.band_index,
                    &modes_refs,
                    max_samples,
                ));
            // Seed the cache so future republish calls only fire on real change.
            let initial_hash = hash_broadcasts(&effects.broadcasts);
            let _ = ctx.heatmap_sub.update_hash_if_changed(initial_hash);
            effects.log_message = Some(format!(
                "OSC: speaker heatmap subscribe -> speaker={} band={} modes={:?} sub_id={}",
                sub.speaker_index, sub.band_index, modes, sub.subscription_id
            ));
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/debug/speaker_heatmap/unsubscribe" {
        ctx.heatmap_sub.clear();
        effects.log_message = Some("OSC: speaker heatmap unsubscribe".to_string());
        return Some(effects);
    }


    if addr == "/omniphony/control/input/drc_mode" {
        let requested = parse_string_arg(msg.args.first());
        if let Some(requested) = requested {
            let mut live = ctx.renderer.live.write().unwrap();
            if live.drc_mode != requested {
                live.drc_mode = requested.clone();
                effects.mark_dirty = true;
                effects.log_message = Some(format!("OSC: DRC mode staged → {}", requested));
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/input/drc_weight" {
        if let Some(value) = parse_f32_arg(msg.args.first()) {
            let clamped = value.clamp(0.0, 1.0);
            let mut live = ctx.renderer.live.write().unwrap();
            if (live.drc_weight - clamped).abs() > f32::EPSILON {
                live.drc_weight = clamped;
                effects.mark_dirty = true;
                effects.log_message = Some(format!("OSC: DRC weight staged → {:.3}", clamped));
            }
        }
        return Some(effects);
    }


    if addr == "/omniphony/control/ramp_mode" {
        let Some(mode) = msg.args.first().and_then(|arg| match arg {
            OscType::String(s) => renderer::live_params::RampMode::from_str(s),
            _ => None,
        }) else {
            return Some(effects);
        };
        ctx.renderer.set_requested_ramp_mode(mode);
        ctx.renderer.live.write().unwrap().ramp_mode = mode;
        effects.mark_dirty = true;
        effects.log_message = Some(format!("OSC: ramp_mode → {}", mode.as_str()));
        return Some(effects);
    }


    if addr == "/omniphony/control/layout/radius_m" {
        if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.max(0.01)) {
            ctx.renderer
                .with_editable_layout(|layout| layout.radius_m = v);
            effects.mark_dirty = true;
            effects.log_message = Some(format!("OSC: layout radius_m → {}", v));
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/gain" {
        if let Some(gain) = parse_f32_arg(msg.args.first()) {
            ctx.renderer.live.write().unwrap().master_gain = gain;
            effects.mark_dirty = true;
        }
        return Some(effects);
    }

    macro_rules! layout_float_with_recompute {
        ($path:literal, $field:ident) => {
            if addr == $path {
                if let Some(value) = parse_f32_arg(msg.args.first()) {
                    ctx.renderer.live.write().unwrap().$field = value;
                    effects.mark_dirty = true;
                    effects.trigger_layout_recompute = true;
                }
                return Some(effects);
            }
        };
    }

    layout_float_with_recompute!("/omniphony/control/spread/min", spread_min);
    layout_float_with_recompute!("/omniphony/control/spread/max", spread_max);
    layout_float_with_recompute!(
        "/omniphony/control/spread/distance_range",
        spread_distance_range
    );
    layout_float_with_recompute!(
        "/omniphony/control/spread/distance_curve",
        spread_distance_curve
    );

    if addr == "/omniphony/control/spread/from_distance" {
        if let Some(v) = parse_bool_arg(msg.args.first()) {
            ctx.renderer.live.write().unwrap().spread_from_distance = v;
            effects.mark_dirty = true;
            effects.trigger_layout_recompute = true;
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/spread/size_to_spread_mode" {
        if let Some(OscType::String(s)) = msg.args.first() {
            if let Some(mode) = renderer::render_backend::SizeToSpreadMode::from_str(
                s.trim().to_ascii_lowercase().as_str(),
            ) {
                ctx.renderer.live.write().unwrap().size_to_spread_mode = mode;
                effects.mark_dirty = true;
                // No layout recompute: only the GainCache is affected via its
                // size_to_spread_mode key.
            }
        }
        return Some(effects);
    }

    if let Some(rest) = addr.strip_prefix("/omniphony/control/render_evaluation/cartesian/") {
        let size = match msg.args.first() {
            Some(OscType::Int(i)) => Some((*i).max(1) as usize),
            Some(OscType::Float(f)) => Some((*f).round().max(1.0) as usize),
            _ => None,
        };
        if let Some(size) = size {
            let state_addr = match rest {
                "x_size" => {
                    ctx.renderer
                        .live
                        .write()
                        .unwrap()
                        .evaluation
                        .cartesian
                        .x_size = size;
                    Some("/omniphony/state/render_evaluation/cartesian/x_size")
                }
                "y_size" => {
                    ctx.renderer
                        .live
                        .write()
                        .unwrap()
                        .evaluation
                        .cartesian
                        .y_size = size;
                    Some("/omniphony/state/render_evaluation/cartesian/y_size")
                }
                "z_size" => {
                    ctx.renderer
                        .live
                        .write()
                        .unwrap()
                        .evaluation
                        .cartesian
                        .z_size = size;
                    Some("/omniphony/state/render_evaluation/cartesian/z_size")
                }
                "z_neg_size" => {
                    ctx.renderer
                        .live
                        .write()
                        .unwrap()
                        .evaluation
                        .cartesian
                        .z_neg_size = size;
                    Some("/omniphony/state/render_evaluation/cartesian/z_neg_size")
                }
                _ => None,
            };
            if let Some(state_addr) = state_addr {
                effects.mark_dirty = true;
                effects.trigger_layout_recompute = true;
                effects.broadcasts.push(BroadcastUpdate {
                    addr: state_addr.to_string(),
                    value: BroadcastValue::Int(size as i32),
                });
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/render_evaluation/position_interpolation" {
        if let Some(enabled) = parse_bool_arg(msg.args.first()) {
            ctx.renderer
                .live
                .write()
                .unwrap()
                .evaluation
                .position_interpolation = enabled;
            effects.mark_dirty = true;
            effects.trigger_layout_recompute = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/render_evaluation/position_interpolation".to_string(),
                value: BroadcastValue::Int(if enabled { 1 } else { 0 }),
            });
        }
        return Some(effects);
    }

    if let Some(rest) = addr.strip_prefix("/omniphony/control/render_evaluation/polar/") {
        match rest {
            "azimuth_resolution" | "elevation_resolution" => {
                let res = match msg.args.first() {
                    Some(OscType::Int(i)) => Some((*i).max(1)),
                    Some(OscType::Float(f)) => Some((*f as i32).max(1)),
                    _ => None,
                };
                if let Some(res) = res {
                    let state_addr = match rest {
                        "azimuth_resolution" => {
                            ctx.renderer
                                .live
                                .write()
                                .unwrap()
                                .evaluation
                                .polar
                                .azimuth_values = res;
                            Some("/omniphony/state/render_evaluation/polar/azimuth_resolution")
                        }
                        "elevation_resolution" => {
                            ctx.renderer
                                .live
                                .write()
                                .unwrap()
                                .evaluation
                                .polar
                                .elevation_values = res;
                            Some("/omniphony/state/render_evaluation/polar/elevation_resolution")
                        }
                        _ => None,
                    };
                    if let Some(state_addr) = state_addr {
                        effects.mark_dirty = true;
                        effects.trigger_layout_recompute = true;
                        effects.broadcasts.push(BroadcastUpdate {
                            addr: state_addr.to_string(),
                            value: BroadcastValue::Int(res),
                        });
                    }
                }
            }
            "distance_res" => {
                let res = match msg.args.first() {
                    Some(OscType::Int(i)) => Some((*i).max(1)),
                    Some(OscType::Float(f)) => Some((*f as i32).max(1)),
                    _ => None,
                };
                if let Some(res) = res {
                    ctx.renderer
                        .live
                        .write()
                        .unwrap()
                        .evaluation
                        .polar
                        .distance_res = res;
                    effects.mark_dirty = true;
                    effects.trigger_layout_recompute = true;
                    effects.broadcasts.push(BroadcastUpdate {
                        addr: "/omniphony/state/render_evaluation/polar/distance_res".to_string(),
                        value: BroadcastValue::Int(res),
                    });
                }
            }
            "distance_max" => {
                let max_v = match msg.args.first() {
                    Some(OscType::Int(i)) => Some((*i as f32).max(0.01)),
                    Some(OscType::Float(f)) => Some((*f).max(0.01)),
                    _ => None,
                };
                if let Some(max_v) = max_v {
                    ctx.renderer
                        .live
                        .write()
                        .unwrap()
                        .evaluation
                        .polar
                        .distance_max = max_v;
                    effects.mark_dirty = true;
                    effects.trigger_layout_recompute = true;
                    effects.broadcasts.push(BroadcastUpdate {
                        addr: "/omniphony/state/render_evaluation/polar/distance_max".to_string(),
                        value: BroadcastValue::Float(max_v),
                    });
                }
            }
            _ => {}
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/loudness" {
        if let Some(v) = parse_bool_arg(msg.args.first()) {
            ctx.renderer.live.write().unwrap().use_loudness = v;
            effects.mark_dirty = true;
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/distance_model" {
        if let Some(OscType::String(model)) = msg.args.first() {
            if let Ok(model) = model.parse::<renderer::spatial_vbap::DistanceModel>() {
                ctx.renderer.live.write().unwrap().distance_model = model;
                effects.mark_dirty = true;
                effects.trigger_layout_recompute = true;
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/distance_model_metric" {
        if let Some(OscType::String(metric)) = msg.args.first() {
            if let Ok(metric) = metric.parse::<renderer::spatial_vbap::DistanceMetric>() {
                ctx.renderer.live.write().unwrap().distance_model_metric = metric;
                effects.mark_dirty = true;
                effects.trigger_layout_recompute = true;
            }
        }
        return Some(effects);
    }

    if let Some(rest) = addr.strip_prefix("/omniphony/control/experimental_distance/") {
        let mut live = ctx.renderer.live.write().unwrap();
        let mut changed = false;
        match rest {
            "distance_floor" => {
                if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.max(0.0)) {
                    live.experimental_distance.distance_floor = v;
                    changed = true;
                    effects.log_message =
                        Some(format!("OSC: experimental_distance/distance_floor -> {v}"));
                }
            }
            "min_active_speakers" => {
                if let Some(v) = parse_positive_u32_arg(msg.args.first()) {
                    live.experimental_distance.min_active_speakers = v as usize;
                    if live.experimental_distance.max_active_speakers
                        < live.experimental_distance.min_active_speakers
                    {
                        live.experimental_distance.max_active_speakers =
                            live.experimental_distance.min_active_speakers;
                    }
                    changed = true;
                    effects.log_message = Some(format!(
                        "OSC: experimental_distance/min_active_speakers -> {v}"
                    ));
                }
            }
            "max_active_speakers" => {
                if let Some(v) = parse_positive_u32_arg(msg.args.first()) {
                    live.experimental_distance.max_active_speakers =
                        (v as usize).max(live.experimental_distance.min_active_speakers);
                    changed = true;
                    effects.log_message = Some(format!(
                        "OSC: experimental_distance/max_active_speakers -> {}",
                        live.experimental_distance.max_active_speakers
                    ));
                }
            }
            "position_error_floor" => {
                if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.max(0.0)) {
                    live.experimental_distance.position_error_floor = v;
                    changed = true;
                    effects.log_message = Some(format!(
                        "OSC: experimental_distance/position_error_floor -> {v}"
                    ));
                }
            }
            "position_error_nearest_scale" => {
                if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.max(0.0)) {
                    live.experimental_distance.position_error_nearest_scale = v;
                    changed = true;
                    effects.log_message = Some(format!(
                        "OSC: experimental_distance/position_error_nearest_scale -> {v}"
                    ));
                }
            }
            "position_error_span_scale" => {
                if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.max(0.0)) {
                    live.experimental_distance.position_error_span_scale = v;
                    changed = true;
                    effects.log_message = Some(format!(
                        "OSC: experimental_distance/position_error_span_scale -> {v}"
                    ));
                }
            }
            _ => {}
        }

        if changed {
            effects.mark_dirty = true;
            effects.trigger_layout_recompute = true;
        }
        return Some(effects);
    }

    if let Some(rest) = addr.strip_prefix("/omniphony/control/barycenter/") {
        let mut live = ctx.renderer.live.write().unwrap();
        let mut changed = false;
        match rest {
            "localize" => {
                if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.max(0.0)) {
                    live.barycenter.localize = v;
                    changed = true;
                    effects.log_message = Some(format!("OSC: barycenter/localize -> {v}"));
                }
            }
            _ => {}
        }

        if changed {
            effects.mark_dirty = true;
            effects.trigger_layout_recompute = true;
        }
        return Some(effects);
    }

    if let Some(rest) = addr.strip_prefix("/omniphony/control/hybrid/") {
        let mut live = ctx.renderer.live.write().unwrap();
        let mut changed = false;
        match rest {
            "external_backend" | "internal_backend" => {
                if let Some(value) = parse_string_arg(msg.args.first()) {
                    let normalized = value.trim().to_ascii_lowercase();
                    // Only concrete backends are valid inner models (no nested hybrid).
                    if matches!(
                        normalized.as_str(),
                        "vbap" | "barycenter" | "experimental_distance"
                    ) {
                        let slot = if rest == "external_backend" {
                            &mut live.hybrid.external_backend_id
                        } else {
                            &mut live.hybrid.internal_backend_id
                        };
                        if *slot != normalized {
                            *slot = normalized.clone();
                            changed = true;
                            effects.log_message = Some(format!("OSC: hybrid/{rest} -> {normalized}"));
                        }
                    }
                }
            }
            "curve_smoothing" => {
                if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.clamp(0.0, 1.0)) {
                    if (live.hybrid.curve_smoothing - v).abs() > 1e-6 {
                        live.hybrid.curve_smoothing = v;
                        changed = true;
                        effects.log_message =
                            Some(format!("OSC: hybrid/curve_smoothing -> {v}"));
                    }
                }
            }
            "metric" => {
                if let Some(OscType::String(metric)) = msg.args.first() {
                    if let Ok(metric) = metric.parse::<renderer::spatial_vbap::DistanceMetric>() {
                        if live.hybrid.metric != metric {
                            live.hybrid.metric = metric;
                            changed = true;
                        }
                    }
                }
            }
            "curve" => {
                // Flat list of (x, y) pairs: x0, y0, x1, y1, …
                let mut values: Vec<f32> = Vec::with_capacity(msg.args.len());
                let mut valid = true;
                for arg in &msg.args {
                    match parse_f32_arg(Some(arg)) {
                        Some(v) => values.push(v),
                        None => {
                            valid = false;
                            break;
                        }
                    }
                }
                if valid && values.len() >= 4 && values.len() % 2 == 0 {
                    live.hybrid.curve = values
                        .chunks_exact(2)
                        .map(|pair| [pair[0].clamp(0.0, 1.0), pair[1].clamp(0.0, 1.0)])
                        .collect();
                    changed = true;
                    effects.log_message =
                        Some(format!("OSC: hybrid/curve -> {} points", live.hybrid.curve.len()));
                }
            }
            _ => {}
        }

        if changed {
            effects.mark_dirty = true;
            effects.trigger_layout_recompute = true;
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/room_ratio" {
        if msg.args.len() >= 3 {
            let w = parse_f32_arg(msg.args.first());
            let l = parse_f32_arg(msg.args.get(1));
            let h = parse_f32_arg(msg.args.get(2));
            if let (Some(w), Some(l), Some(h)) = (w, l, h) {
                ctx.renderer.live.write().unwrap().room_ratio = [w, l, h];
                effects.mark_dirty = true;
                effects.trigger_layout_recompute = true;
                effects.log_message = Some(format!("OSC: room_ratio → [{}, {}, {}]", w, l, h));
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/room_ratio_rear" {
        if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.max(0.01)) {
            ctx.renderer.live.write().unwrap().room_ratio_rear = v;
            effects.mark_dirty = true;
            effects.trigger_layout_recompute = true;
            effects.log_message = Some(format!("OSC: room_ratio_rear → {}", v));
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/room_ratio_lower" {
        if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.max(0.01)) {
            ctx.renderer.live.write().unwrap().room_ratio_lower = v;
            effects.mark_dirty = true;
            effects.trigger_layout_recompute = true;
            effects.log_message = Some(format!("OSC: room_ratio_lower → {}", v));
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/room_ratio_center_blend" {
        if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.clamp(0.0, 1.0)) {
            ctx.renderer.live.write().unwrap().room_ratio_center_blend = v;
            effects.mark_dirty = true;
            effects.trigger_layout_recompute = true;
            effects.log_message = Some(format!("OSC: room_ratio_center_blend → {}", v));
        }
        return Some(effects);
    }

    if let Some(rest) = addr.strip_prefix("/omniphony/control/distance_diffuse/") {
        match rest {
            "enabled" => {
                if let Some(v) = parse_bool_arg(msg.args.first()) {
                    ctx.renderer.live.write().unwrap().use_distance_diffuse = v;
                    effects.mark_dirty = true;
                    effects.trigger_layout_recompute = true;
                }
                return Some(effects);
            }
            "metric" => {
                if let Some(OscType::String(metric)) = msg.args.first() {
                    if let Ok(metric) =
                        metric.parse::<renderer::spatial_vbap::DistanceMetric>()
                    {
                        ctx.renderer.live.write().unwrap().distance_diffuse_metric = metric;
                        effects.mark_dirty = true;
                        effects.trigger_layout_recompute = true;
                    }
                }
                return Some(effects);
            }
            "threshold" => {
                if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.max(1e-6)) {
                    ctx.renderer
                        .live
                        .write()
                        .unwrap()
                        .distance_diffuse_threshold = v;
                    effects.mark_dirty = true;
                    effects.trigger_layout_recompute = true;
                }
                return Some(effects);
            }
            "curve" => {
                if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.max(0.0)) {
                    ctx.renderer.live.write().unwrap().distance_diffuse_curve = v;
                    effects.mark_dirty = true;
                    effects.trigger_layout_recompute = true;
                }
                return Some(effects);
            }
            _ => {}
        }
    }

    if let Some(rest) = addr.strip_prefix("/omniphony/control/object/") {
        if let Some(idx_str) = rest.strip_suffix("/gain") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                if let Some(gain) =
                    parse_f32_arg(msg.args.first()).map(|value| value.clamp(0.0, 2.0))
                {
                    ctx.renderer
                        .live
                        .write()
                        .unwrap()
                        .objects
                        .entry(idx)
                        .or_default()
                        .gain = gain;
                    ctx.renderer.mark_object_params_dirty();
                    effects.mark_dirty = true;
                    effects.broadcasts.push(BroadcastUpdate {
                        addr: format!("/omniphony/state/object/{}/gain", idx),
                        value: BroadcastValue::Float(gain),
                    });
                    effects.log_message = Some(format!("OSC: object[{}] gain → {}", idx, gain));
                }
            }
            return Some(effects);
        }
        if let Some(idx_str) = rest.strip_suffix("/mute") {
            if let Ok(idx) = idx_str.parse::<usize>() {
                if let Some(muted) = parse_bool_arg(msg.args.first()) {
                    ctx.renderer
                        .live
                        .write()
                        .unwrap()
                        .objects
                        .entry(idx)
                        .or_default()
                        .muted = muted;
                    ctx.renderer.mark_object_params_dirty();
                    effects.mark_dirty = true;
                    effects.broadcasts.push(BroadcastUpdate {
                        addr: format!("/omniphony/state/object/{}/mute", idx),
                        value: BroadcastValue::Int(if muted { 1 } else { 0 }),
                    });
                    effects.log_message = Some(format!("OSC: object[{}] mute → {}", idx, muted));
                }
            }
            return Some(effects);
        }
    }

    None
}
