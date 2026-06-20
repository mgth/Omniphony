use crate::context::RuntimeControlContext;
use renderer::live_params::LiveEvaluationMode;
use renderer::render_backend::canonical_builtin_backend_id;
use rosc::{OscMessage, OscType};
use serde::Deserialize;
use std::hash::{Hash, Hasher};

/// In-flight SOFA upload (single slot — Studio serialises uploads).
struct HrtfUpload {
    name: String,
    total: usize,
    data: Vec<u8>,
    chunks: u32,
}

/// 1 GiB cap on an uploaded HRTF file.
const HRTF_UPLOAD_MAX_BYTES: usize = 1 << 30;

fn hrtf_upload_state() -> &'static std::sync::Mutex<Option<HrtfUpload>> {
    static STATE: std::sync::OnceLock<std::sync::Mutex<Option<HrtfUpload>>> =
        std::sync::OnceLock::new();
    STATE.get_or_init(|| std::sync::Mutex::new(None))
}

#[derive(Debug, Clone)]
pub enum BroadcastValue {
    Int(i32),
    Float(f32),
    Fff(f32, f32, f32),
    String(String),
    /// Raw bytes sent as a single OSC `blob` arg. Used for bulk binary payloads
    /// (e.g. the chunked, compressed speaker gain table) where text/float args
    /// would be far too verbose. Any framing (version, chunk index, …) is encoded
    /// inside the bytes by the producer.
    Blob(Vec<u8>),
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
    /// When `trigger_layout_recompute` is set, whether this change is
    /// evaluation-layer only (mode / grid resolution) — i.e. the backend geometry
    /// (triangulation + decorator metrics) is unchanged, so the recompute can
    /// reuse the existing gain models and rebuild only the evaluation wrapper.
    /// Default `false` = treat as a geometry change (full rebuild), which is the
    /// safe assumption; handlers known to be evaluation-only opt in.
    pub evaluation_only: bool,
    pub broadcasts: Vec<BroadcastUpdate>,
    pub log_message: Option<String>,
}

// AdaptiveResamplingPatch / AudioConfigPatch / LiveInputPatch / InputConfigPatch
// moved to the `host_audio` crate alongside their dispatch handlers.

/// Deserialize an `Option<Option<T>>` so an explicit JSON `null` means "clear"
/// (`Some(None)`) while an absent field leaves the value untouched (`None`).
///
/// Plain `#[serde(default)]` can't express this: serde_json folds `null` into
/// the *outer* `None`, making an explicit "remove this cutoff" request from the
/// editor indistinguishable from "field not sent" — so a per-speaker crossover
/// cutoff could be changed but never blanked. Pair with
/// `#[serde(default, deserialize_with = "double_option")]`: `default` covers the
/// absent case (the helper is only called when the field is present).
fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

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
    #[serde(default, deserialize_with = "double_option")]
    freq_low: Option<Option<f32>>,
    #[serde(default, deserialize_with = "double_option")]
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
    #[serde(default, deserialize_with = "double_option")]
    freq_low: Option<Option<f32>>,
    #[serde(default, deserialize_with = "double_option")]
    freq_high: Option<Option<f32>>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LayoutMoveSpeakerPatch {
    from: usize,
    to: usize,
}

/// Wholesale replacement of the editable layout: the full speaker set in one
/// message. Studio sends this when a layout is imported or a preset is selected,
/// instead of trying to morph the live layout with a fragile add/remove/move
/// sequence (which desynced Studio and the renderer and could empty the layout).
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LayoutReplacePatch {
    radius_m: Option<f32>,
    #[serde(default)]
    speakers: Vec<LayoutAddSpeakerPatch>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct LayoutConfigPatch {
    radius_m: Option<f32>,
    replace_layout: Option<LayoutReplacePatch>,
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

/// Payload bytes per gain-table chunk (excluding the 8-byte version+index header),
/// kept well under the UDP MTU once OSC blob framing is added.
const GAINTABLE_CHUNK_BYTES: usize = 1024;

/// Stable 31-bit id of a serialized table, so a re-request returns the same
/// version while a topology rebuild yields a new one.
pub fn gaintable_version(bytes: &[u8]) -> u32 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    (hasher.finish() & 0x7fff_ffff) as u32
}

/// Chunk an already-serialized gain table into OSC broadcasts: a `meta` header
/// (JSON: version, total_len, chunk_count, chunk_bytes) followed by `chunk` blobs,
/// each prefixed with `version` + chunk index. `only = Some((version, missing))`
/// re-emits just those chunk indices when the version still matches (NACK resend),
/// else falls back to a full send. `None` = full send with `meta`.
///
/// Pure: deterministic for given bytes, so a NACK resend reproduces the same
/// chunking the client expects.
pub fn gaintable_chunk_broadcasts(
    bytes: &[u8],
    only: Option<(u32, Vec<u32>)>,
) -> Vec<BroadcastUpdate> {
    let version = gaintable_version(bytes);
    let chunk_count = bytes.len().div_ceil(GAINTABLE_CHUNK_BYTES).max(1);

    // Decide which chunks to send + whether to (re)send the meta header.
    let (emit_meta, indices): (bool, Vec<usize>) = match &only {
        None => (true, (0..chunk_count).collect()),
        Some((req_version, missing)) if *req_version == version => (
            false,
            missing
                .iter()
                .map(|&i| i as usize)
                .filter(|&i| i < chunk_count)
                .collect(),
        ),
        // Version mismatch → the table changed under the client: full resend.
        Some(_) => (true, (0..chunk_count).collect()),
    };

    let mut out = Vec::with_capacity(indices.len() + 1);
    if emit_meta {
        let meta = serde_json::json!({
            "version": version,
            "total_len": bytes.len(),
            "chunk_count": chunk_count,
            "chunk_bytes": GAINTABLE_CHUNK_BYTES,
        })
        .to_string();
        out.push(BroadcastUpdate {
            addr: "/omniphony/state/debug/speaker_gaintable/meta".to_string(),
            value: BroadcastValue::String(meta),
        });
    }
    for ci in indices {
        let start = ci * GAINTABLE_CHUNK_BYTES;
        let end = (start + GAINTABLE_CHUNK_BYTES).min(bytes.len());
        let mut blob = Vec::with_capacity(8 + (end - start));
        blob.extend_from_slice(&version.to_le_bytes());
        blob.extend_from_slice(&(ci as u32).to_le_bytes());
        blob.extend_from_slice(&bytes[start..end]);
        out.push(BroadcastUpdate {
            addr: "/omniphony/state/debug/speaker_gaintable/chunk".to_string(),
            value: BroadcastValue::Blob(blob),
        });
    }
    out
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

            // Wholesale layout replacement must run first: it resets the whole
            // speaker set, so any add/remove/edit in the same message would apply
            // against the new layout (Studio never combines them, but the order
            // keeps the semantics well-defined).
            if let Some(replace) = patch.replace_layout {
                let new_speakers: Vec<renderer::speaker_layout::Speaker> = replace
                    .speakers
                    .into_iter()
                    .enumerate()
                    .map(|(idx, sp)| build_layout_speaker_from_patch(sp, format!("spk-{idx}")))
                    .collect();
                // Per-speaker live params are keyed by position, so a wholesale
                // swap invalidates every entry — capture the new delays before
                // moving the speakers into the layout.
                let delays: Vec<(usize, f32)> = new_speakers
                    .iter()
                    .enumerate()
                    .filter(|(_, spk)| spk.delay_ms > 0.0)
                    .map(|(idx, spk)| (idx, spk.delay_ms))
                    .collect();
                ctx.renderer.with_editable_layout(|layout| {
                    if let Some(radius_m) = replace.radius_m {
                        layout.radius_m = radius_m.max(0.01);
                    }
                    layout.speakers = new_speakers;
                });
                {
                    let mut live = ctx.renderer.live.write();
                    live.speakers.clear();
                    for (idx, delay_ms) in delays {
                        live.speakers.entry(idx).or_default().delay_ms = delay_ms;
                    }
                }
                ctx.renderer.mark_speaker_params_dirty();
                changed = true;
            }

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
                        let mut live = ctx.renderer.live.write();
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
                        let mut live = ctx.renderer.live.write();
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
        // Accept built-in ids/aliases (e.g. "distance") and any registered backend
        // id (e.g. a contributor's "example"), so the dropdown can offer them all.
        let requested_id = parse_string_arg(msg.args.first()).and_then(|value| {
            canonical_builtin_backend_id(&value)
                .map(|id| id.to_string())
                .or_else(|| ctx.renderer.has_backend(&value).then_some(value))
        });
        if let Some(requested_id) = requested_id {
            let mut live = ctx.renderer.live.write();
            if live.backend_id() != requested_id {
                live.backend_id = requested_id.clone();
                effects.mark_dirty = true;
                effects.trigger_layout_recompute = true;
                effects.log_message = Some(format!("OSC: render_backend -> {requested_id}"));
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/output_mode" {
        // Switch between the classic speaker (VBAP) path and the independent
        // binaural (headphone) stage. No topology recompute: the binaural path
        // does not use the speaker topology.
        if let Some(mode) = parse_string_arg(msg.args.first())
            .and_then(|v| renderer::live_params::OutputMode::from_str(&v))
        {
            let mut live = ctx.renderer.live.write();
            if live.binaural.output_mode != mode {
                live.binaural.output_mode = mode;
                effects.mark_dirty = true;
                effects.log_message = Some(format!("OSC: output_mode -> {}", mode.as_str()));
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/head/orientation" {
        // Static head pose from Euler degrees [yaw, pitch, roll]. The live
        // head-tracking input (SensorsOSC) lands in M2; this lets Studio / tests
        // drive the pose directly. No topology rebuild (binaural is topology-free).
        let yaw = parse_f32_arg(msg.args.first()).unwrap_or(0.0);
        let pitch = parse_f32_arg(msg.args.get(1)).unwrap_or(0.0);
        let roll = parse_f32_arg(msg.args.get(2)).unwrap_or(0.0);
        let mut live = ctx.renderer.live.write();
        live.binaural.head_pose = renderer::binaural::HeadPose::from_euler_deg(yaw, pitch, roll);
        effects.mark_dirty = true;
        effects.log_message = Some(format!("OSC: head/orientation -> {yaw},{pitch},{roll}"));
        return Some(effects);
    }

    if addr == "/omniphony/control/head/quat" {
        // Static head pose from a raw quaternion [w, x, y, z].
        let w = parse_f32_arg(msg.args.first()).unwrap_or(1.0);
        let x = parse_f32_arg(msg.args.get(1)).unwrap_or(0.0);
        let y = parse_f32_arg(msg.args.get(2)).unwrap_or(0.0);
        let z = parse_f32_arg(msg.args.get(3)).unwrap_or(0.0);
        let mut live = ctx.renderer.live.write();
        live.binaural.head_pose = renderer::binaural::HeadPose::from_quat(w, x, y, z);
        effects.mark_dirty = true;
        effects.log_message = Some("OSC: head/quat".to_string());
        return Some(effects);
    }

    if addr == "/omniphony/control/binaural/hrir_source" {
        // "synthetic" | "saf"/"kemar" | "sofa:<path>". No topology rebuild; the
        // render thread rebuilds the HRIR grid lazily when the source changes.
        if let Some(src) = parse_string_arg(msg.args.first())
            .and_then(|s| renderer::binaural::HrirSource::from_str(&s))
        {
            let mut live = ctx.renderer.live.write();
            if live.binaural.hrir_source != src {
                live.binaural.hrir_source = src.clone();
                effects.mark_dirty = true;
                effects.log_message = Some(format!("OSC: hrir_source -> {}", src.as_str()));
            }
        }
        return Some(effects);
    }

    // ── SOFA upload (Studio → renderer, chunked OSC blobs) ──────────────────
    // For setups where Studio does not share a filesystem with the renderer:
    // begin [s name, i total_bytes] → chunk [i seq, b data]* → end [i chunks].
    // The file lands in <config dir>/hrtf/ and is activated on completion.
    if addr == "/omniphony/control/binaural/hrtf_upload/begin" {
        let name = parse_string_arg(msg.args.first()).unwrap_or_default();
        let total = match msg.args.get(1) {
            Some(rosc::OscType::Int(i)) if *i > 0 => *i as usize,
            _ => 0,
        };
        let base = std::path::Path::new(&name)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut upload = hrtf_upload_state().lock().unwrap();
        if base.is_empty() || !base.to_ascii_lowercase().ends_with(".sofa") {
            *upload = None;
            effects.log_message = Some(format!("hrtf upload rejected: bad name {name:?}"));
        } else if total == 0 || total > HRTF_UPLOAD_MAX_BYTES {
            *upload = None;
            effects.log_message = Some(format!("hrtf upload rejected: bad size {total}"));
        } else {
            *upload = Some(HrtfUpload {
                name: base.clone(),
                total,
                data: Vec::with_capacity(total),
                chunks: 0,
            });
            effects.log_message = Some(format!("hrtf upload started: {base} ({total} bytes)"));
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/binaural/hrtf_upload/chunk" {
        let seq = match msg.args.first() {
            Some(rosc::OscType::Int(i)) if *i >= 0 => *i as u32,
            _ => return Some(effects),
        };
        let mut upload = hrtf_upload_state().lock().unwrap();
        let abort = match (upload.as_mut(), msg.args.get(1)) {
            (Some(up), Some(rosc::OscType::Blob(data))) => {
                if seq != up.chunks || up.data.len() + data.len() > up.total {
                    true
                } else {
                    up.data.extend_from_slice(data);
                    up.chunks += 1;
                    false
                }
            }
            _ => return Some(effects),
        };
        if abort {
            *upload = None;
            effects.log_message =
                Some("hrtf upload aborted: chunk out of sequence or oversize".to_string());
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/binaural/hrtf_upload/end" {
        let chunks = match msg.args.first() {
            Some(rosc::OscType::Int(i)) if *i >= 0 => *i as u32,
            _ => 0,
        };
        let done = hrtf_upload_state().lock().unwrap().take();
        match done {
            Some(up) if up.chunks == chunks && up.data.len() == up.total => {
                let dir = renderer::config::default_config_path()
                    .and_then(|p| p.parent().map(|d| d.join("hrtf")))
                    .unwrap_or_else(|| std::path::PathBuf::from("hrtf"));
                let write = std::fs::create_dir_all(&dir)
                    .and_then(|_| std::fs::write(dir.join(&up.name), &up.data));
                match write {
                    Ok(()) => {
                        let path = dir.join(&up.name).to_string_lossy().into_owned();
                        ctx.renderer.live.write().binaural.hrir_source =
                            renderer::binaural::HrirSource::Sofa(path.clone());
                        effects.mark_dirty = true;
                        effects.log_message =
                            Some(format!("hrtf upload complete, activated: {path}"));
                    }
                    Err(e) => {
                        effects.log_message = Some(format!("hrtf upload write failed: {e}"));
                    }
                }
            }
            Some(up) => {
                effects.log_message = Some(format!(
                    "hrtf upload incomplete: {}/{} bytes, {}/{} chunks",
                    up.data.len(),
                    up.total,
                    up.chunks,
                    chunks
                ));
            }
            None => {
                effects.log_message = Some("hrtf upload end without begin".to_string());
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/binaural/unit_scale" {
        // Metres per ADM unit (isotropic distance scale for the binaural stage).
        if let Some(v) = parse_f32_arg(msg.args.first()) {
            if v.is_finite() && v > 0.0 {
                let v = v.clamp(0.01, 100.0);
                ctx.renderer.live.write().binaural.unit_scale_m = v;
                effects.mark_dirty = true;
                effects.log_message = Some(format!("OSC: binaural/unit_scale -> {v}"));
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/binaural/head_radius" {
        // Effective head radius (m) for the ITD model — half the inter-ear
        // distance. Human range is roughly 6–12 cm.
        if let Some(v) = parse_f32_arg(msg.args.first()) {
            if v.is_finite() && v > 0.0 {
                let v = v.clamp(0.05, 0.15);
                ctx.renderer.live.write().binaural.head_radius_m = v;
                effects.mark_dirty = true;
                effects.log_message = Some(format!("OSC: binaural/head_radius -> {v}"));
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/binaural/reflections/enabled" {
        if let Some(v) = parse_bool_arg(msg.args.first()) {
            ctx.renderer.live.write().binaural.reflections.enabled = v;
            effects.mark_dirty = true;
            effects.log_message = Some(format!("OSC: binaural/reflections/enabled -> {v}"));
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/binaural/reflections/level" {
        if let Some(v) = parse_f32_arg(msg.args.first()) {
            if v.is_finite() {
                ctx.renderer.live.write().binaural.reflections.level = v.clamp(0.0, 1.0);
                effects.mark_dirty = true;
            }
        }
        return Some(effects);
    }

    if let Some(axis) = match addr {
        "/omniphony/control/binaural/reflections/room_width" => Some(0usize),
        "/omniphony/control/binaural/reflections/room_depth" => Some(1),
        "/omniphony/control/binaural/reflections/room_height" => Some(2),
        _ => None,
    } {
        if let Some(v) = parse_f32_arg(msg.args.first()) {
            if v.is_finite() && v > 0.0 {
                let v = v.clamp(
                    renderer::binaural::reflections::MIN_ROOM_M,
                    renderer::binaural::reflections::MAX_ROOM_M,
                );
                ctx.renderer.live.write().binaural.reflections.room_size_m[axis] = v;
                effects.mark_dirty = true;
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/binaural/reverb/enabled" {
        if let Some(v) = parse_bool_arg(msg.args.first()) {
            ctx.renderer.live.write().binaural.reverb.enabled = v;
            effects.mark_dirty = true;
            effects.log_message = Some(format!("OSC: binaural/reverb/enabled -> {v}"));
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/binaural/reverb/level" {
        if let Some(v) = parse_f32_arg(msg.args.first()) {
            if v.is_finite() {
                ctx.renderer.live.write().binaural.reverb.level = v.clamp(0.0, 1.0);
                effects.mark_dirty = true;
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/binaural/reverb/rt60" {
        if let Some(v) = parse_f32_arg(msg.args.first()) {
            if v.is_finite() && v > 0.0 {
                ctx.renderer.live.write().binaural.reverb.rt60_s = v.clamp(0.1, 3.0);
                effects.mark_dirty = true;
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/binaural/reverb/predelay" {
        if let Some(v) = parse_f32_arg(msg.args.first()) {
            if v.is_finite() && v >= 0.0 {
                ctx.renderer.live.write().binaural.reverb.predelay_ms = v.clamp(0.0, 100.0);
                effects.mark_dirty = true;
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/binaural/air_absorption" {
        if let Some(v) = parse_bool_arg(msg.args.first()) {
            ctx.renderer.live.write().binaural.air_absorption = v;
            effects.mark_dirty = true;
            effects.log_message = Some(format!("OSC: binaural/air_absorption -> {v}"));
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/head/recenter" {
        // Capture the current raw tracker orientation as "forward" and snap the
        // rendered pose to identity so the scene faces straight ahead.
        let mut live = ctx.renderer.live.write();
        live.binaural.tracking.recenter();
        live.binaural.head_pose = renderer::binaural::HeadPose::identity();
        effects.mark_dirty = true;
        effects.log_message = Some("OSC: head/recenter".to_string());
        return Some(effects);
    }

    if addr == "/omniphony/control/head/tracking/address" {
        // Empty string disables tracking.
        let raw = parse_string_arg(msg.args.first()).unwrap_or_default();
        let mut live = ctx.renderer.live.write();
        live.binaural.tracking.address = if raw.is_empty() {
            None
        } else {
            Some(raw.clone())
        };
        effects.mark_dirty = true;
        effects.log_message = Some(format!("OSC: head/tracking/address -> {raw:?}"));
        return Some(effects);
    }

    if addr == "/omniphony/control/head/tracking/format" {
        if let Some(fmt) = parse_string_arg(msg.args.first())
            .and_then(|s| renderer::binaural::HeadTrackingFormat::from_str(&s))
        {
            ctx.renderer.live.write().binaural.tracking.format = fmt;
            effects.mark_dirty = true;
            effects.log_message = Some(format!("OSC: head/tracking/format -> {}", fmt.as_str()));
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/head/tracking/smoothing" {
        if let Some(v) = parse_f32_arg(msg.args.first()) {
            ctx.renderer.live.write().binaural.tracking.smoothing = v.clamp(0.0, 0.999);
            effects.mark_dirty = true;
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/head/tracking/invert" {
        if let Some(v) = parse_bool_arg(msg.args.first()) {
            ctx.renderer.live.write().binaural.tracking.invert = v;
            effects.mark_dirty = true;
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

    // Generic backend parameter set. Two forms:
    //   `[string key, <scalar value>]`              -> currently selected backend
    //   `[string backend_id, string key, <value>]`  -> an explicit backend
    // The explicit form lets the UI address an inner backend (e.g. the hybrid
    // barycenter tab) even though it is not the active selection. Values are
    // stored generically (no typed field per backend) and read at the next
    // topology rebuild via the schema.
    if addr == "/omniphony/control/backend/param" {
        let (target, key, value_arg) = if msg.args.len() >= 3 {
            (
                parse_string_arg(msg.args.first()),
                parse_string_arg(msg.args.get(1)),
                msg.args.get(2),
            )
        } else {
            (None, parse_string_arg(msg.args.first()), msg.args.get(1))
        };
        let value = value_arg.and_then(|arg| match arg {
            OscType::Float(f) => Some(renderer::backend_params::ParamValue::Float(*f)),
            OscType::Double(d) => Some(renderer::backend_params::ParamValue::Float(*d as f32)),
            OscType::Int(i) => Some(renderer::backend_params::ParamValue::Int(*i as i64)),
            OscType::Long(i) => Some(renderer::backend_params::ParamValue::Int(*i)),
            OscType::Bool(b) => Some(renderer::backend_params::ParamValue::Bool(*b)),
            OscType::String(s) => Some(renderer::backend_params::ParamValue::Text(s.clone())),
            _ => None,
        });
        if let (Some(key), Some(value)) = (key, value) {
            let backend_id =
                target.unwrap_or_else(|| ctx.renderer.live.read().backend_id().to_string());
            ctx.renderer.set_backend_param(&backend_id, &key, value);
            effects.mark_dirty = true;
            effects.trigger_layout_recompute = true;
            effects.log_message = Some(format!("OSC: backend param {backend_id}.{key} updated"));
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/render_evaluation_mode" {
        let requested = parse_string_arg(msg.args.first())
            .and_then(|value| LiveEvaluationMode::from_str(&value));
        if let Some(requested) = requested {
            let mut live = ctx.renderer.live.write();
            if live.evaluation.mode != requested {
                live.set_evaluation_mode(requested);
                effects.mark_dirty = true;
                effects.trigger_layout_recompute = true;
                // Changing only the evaluation mode leaves the backend geometry
                // (triangulation + decorator metrics) untouched: reuse the gain
                // models and rebuild only the evaluation wrapper (no re-triangulation).
                effects.evaluation_only = true;
            }
            {
                if live.backend_id() == "vbap" {
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

    if addr == "/omniphony/control/input/drc_mode" {
        let requested = parse_string_arg(msg.args.first());
        if let Some(requested) = requested {
            let mut live = ctx.renderer.live.write();
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
            let mut live = ctx.renderer.live.write();
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
        ctx.renderer.live.write().ramp_mode = mode;
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
            ctx.renderer.live.write().master_gain = gain;
            effects.mark_dirty = true;
        }
        return Some(effects);
    }

    // VBAP spread tuning is a generic backend param now (baked into the backend
    // at build, keyed by backend id "vbap"). These dedicated addresses are kept
    // as thin aliases over `set_backend_param` so existing OSC clients keep
    // working; the canonical path is `/omniphony/control/backend/param`. All of
    // them trigger a topology rebuild (the values are baked, not per-request).
    macro_rules! spread_param_with_recompute {
        ($path:literal, $key:literal) => {
            if addr == $path {
                if let Some(value) = parse_f32_arg(msg.args.first()) {
                    ctx.renderer.set_backend_param(
                        "vbap",
                        $key,
                        renderer::backend_params::ParamValue::Float(value),
                    );
                    effects.mark_dirty = true;
                    effects.trigger_layout_recompute = true;
                }
                return Some(effects);
            }
        };
    }

    spread_param_with_recompute!("/omniphony/control/spread/min", "spread_min");
    spread_param_with_recompute!("/omniphony/control/spread/max", "spread_max");
    spread_param_with_recompute!(
        "/omniphony/control/spread/distance_range",
        "spread_distance_range"
    );
    spread_param_with_recompute!(
        "/omniphony/control/spread/distance_curve",
        "spread_distance_curve"
    );

    if addr == "/omniphony/control/spread/from_distance" {
        if let Some(v) = parse_bool_arg(msg.args.first()) {
            ctx.renderer.set_backend_param(
                "vbap",
                "spread_from_distance",
                renderer::backend_params::ParamValue::Bool(v),
            );
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
                ctx.renderer.set_backend_param(
                    "vbap",
                    "size_to_spread_mode",
                    renderer::backend_params::ParamValue::Text(mode.as_str().to_string()),
                );
                effects.mark_dirty = true;
                // Size policy is baked into the backend now, so a rebuild is
                // required (it was previously a per-request GainCache key).
                effects.trigger_layout_recompute = true;
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/auto_gain" {
        if let Some(v) = parse_bool_arg(msg.args.first()) {
            ctx.renderer.live.write().auto_gain = v;
            effects.mark_dirty = true;
            // Per-frame gain-stage flag: no topology recompute.
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/auto_gain_ceiling" {
        if let Some(v) = parse_f32_arg(msg.args.first()) {
            // dBFS target; clamp to a sane range (ceiling at or below 0 dBFS).
            ctx.renderer.live.write().auto_gain_ceiling_db = v.clamp(-12.0, 0.0);
            effects.mark_dirty = true;
            // Per-frame gain-stage value: no topology recompute.
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
                    ctx.renderer.live.write().evaluation.cartesian.x_size = size;
                    Some("/omniphony/state/render_evaluation/cartesian/x_size")
                }
                "y_size" => {
                    ctx.renderer.live.write().evaluation.cartesian.y_size = size;
                    Some("/omniphony/state/render_evaluation/cartesian/y_size")
                }
                "z_size" => {
                    ctx.renderer.live.write().evaluation.cartesian.z_size = size;
                    Some("/omniphony/state/render_evaluation/cartesian/z_size")
                }
                "z_neg_size" => {
                    ctx.renderer.live.write().evaluation.cartesian.z_neg_size = size;
                    Some("/omniphony/state/render_evaluation/cartesian/z_neg_size")
                }
                _ => None,
            };
            if let Some(state_addr) = state_addr {
                effects.mark_dirty = true;
                effects.trigger_layout_recompute = true;
                // Cartesian grid resolution is evaluation-layer only: re-sample the
                // table, reuse the backend geometry.
                effects.evaluation_only = true;
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
            ctx.renderer.live.write().evaluation.position_interpolation = enabled;
            // No layout recompute: this flag only selects nearest-cell vs
            // trilinear at table-read time. The precomputed table content is
            // independent of it, and the renderer syncs the live value into the
            // evaluators each frame (see SpatialRenderer::render_frame). Rebuilding
            // the whole grid here just produced an identical table.
            effects.mark_dirty = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/render_evaluation/position_interpolation".to_string(),
                value: BroadcastValue::Int(if enabled { 1 } else { 0 }),
            });
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/render_evaluation/object_size_intervals" {
        let intervals = match msg.args.first() {
            Some(OscType::Int(i)) => Some((*i).max(0) as usize),
            Some(OscType::Float(f)) => Some(f.round().max(0.0) as usize),
            _ => None,
        };
        if let Some(intervals) = intervals {
            ctx.renderer.live.write().evaluation.object_size_intervals = intervals;
            effects.mark_dirty = true;
            effects.trigger_layout_recompute = true;
            // Object-size interval count is evaluation-layer only: re-sample the
            // tables, reuse the backend geometry.
            effects.evaluation_only = true;
            effects.broadcasts.push(BroadcastUpdate {
                addr: "/omniphony/state/render_evaluation/object_size_intervals".to_string(),
                value: BroadcastValue::Int(intervals as i32),
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
                            ctx.renderer.live.write().evaluation.polar.azimuth_values = res;
                            Some("/omniphony/state/render_evaluation/polar/azimuth_resolution")
                        }
                        "elevation_resolution" => {
                            ctx.renderer.live.write().evaluation.polar.elevation_values = res;
                            Some("/omniphony/state/render_evaluation/polar/elevation_resolution")
                        }
                        _ => None,
                    };
                    if let Some(state_addr) = state_addr {
                        effects.mark_dirty = true;
                        effects.trigger_layout_recompute = true;
                        // Polar grid resolution is evaluation-layer only.
                        effects.evaluation_only = true;
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
                    ctx.renderer.live.write().evaluation.polar.distance_res = res;
                    effects.mark_dirty = true;
                    effects.trigger_layout_recompute = true;
                    effects.evaluation_only = true;
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
                    ctx.renderer.live.write().evaluation.polar.distance_max = max_v;
                    effects.mark_dirty = true;
                    effects.trigger_layout_recompute = true;
                    effects.evaluation_only = true;
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
            ctx.renderer.live.write().use_loudness = v;
            effects.mark_dirty = true;
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/distance_model" {
        if let Some(OscType::String(model)) = msg.args.first() {
            if let Ok(model) = model.parse::<renderer::spatial_vbap::DistanceModel>() {
                ctx.renderer.live.write().distance_model = model;
                effects.mark_dirty = true;
                effects.trigger_layout_recompute = true;
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/distance_model_metric" {
        if let Some(OscType::String(metric)) = msg.args.first() {
            if let Ok(metric) = metric.parse::<renderer::spatial_vbap::DistanceMetric>() {
                ctx.renderer.live.write().distance_model_metric = metric;
                effects.mark_dirty = true;
                effects.trigger_layout_recompute = true;
            }
        }
        return Some(effects);
    }

    if let Some(rest) = addr.strip_prefix("/omniphony/control/hybrid/") {
        let mut live = ctx.renderer.live.write();
        let mut changed = false;
        match rest {
            "external_backend" | "internal_backend" => {
                if let Some(value) = parse_string_arg(msg.args.first()) {
                    let normalized = value.trim().to_ascii_lowercase();
                    // Any registered backend is a valid inner model, except a
                    // nested hybrid (which would recurse).
                    if normalized != "hybrid" && ctx.renderer.has_backend(&normalized) {
                        let slot = if rest == "external_backend" {
                            &mut live.hybrid.external_backend_id
                        } else {
                            &mut live.hybrid.internal_backend_id
                        };
                        if *slot != normalized {
                            *slot = normalized.clone();
                            changed = true;
                            effects.log_message =
                                Some(format!("OSC: hybrid/{rest} -> {normalized}"));
                        }
                    }
                }
            }
            "curve_smoothing" => {
                if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.clamp(0.0, 1.0)) {
                    if (live.hybrid.curve_smoothing - v).abs() > 1e-6 {
                        live.hybrid.curve_smoothing = v;
                        changed = true;
                        effects.log_message = Some(format!("OSC: hybrid/curve_smoothing -> {v}"));
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
                    effects.log_message = Some(format!(
                        "OSC: hybrid/curve -> {} points",
                        live.hybrid.curve.len()
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

    if addr == "/omniphony/control/room_ratio" {
        if msg.args.len() >= 3 {
            let w = parse_f32_arg(msg.args.first());
            let l = parse_f32_arg(msg.args.get(1));
            let h = parse_f32_arg(msg.args.get(2));
            if let (Some(w), Some(l), Some(h)) = (w, l, h) {
                ctx.renderer.live.write().room_ratio = [w, l, h];
                effects.mark_dirty = true;
                effects.trigger_layout_recompute = true;
                effects.log_message = Some(format!("OSC: room_ratio → [{}, {}, {}]", w, l, h));
            }
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/room_ratio_rear" {
        if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.max(0.01)) {
            ctx.renderer.live.write().room_ratio_rear = v;
            effects.mark_dirty = true;
            effects.trigger_layout_recompute = true;
            effects.log_message = Some(format!("OSC: room_ratio_rear → {}", v));
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/room_ratio_lower" {
        if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.max(0.01)) {
            ctx.renderer.live.write().room_ratio_lower = v;
            effects.mark_dirty = true;
            effects.trigger_layout_recompute = true;
            effects.log_message = Some(format!("OSC: room_ratio_lower → {}", v));
        }
        return Some(effects);
    }

    if addr == "/omniphony/control/room_ratio_center_blend" {
        if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.clamp(0.0, 1.0)) {
            ctx.renderer.live.write().room_ratio_center_blend = v;
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
                    ctx.renderer.live.write().use_distance_diffuse = v;
                    effects.mark_dirty = true;
                    effects.trigger_layout_recompute = true;
                }
                return Some(effects);
            }
            "metric" => {
                if let Some(OscType::String(metric)) = msg.args.first() {
                    if let Ok(metric) = metric.parse::<renderer::spatial_vbap::DistanceMetric>() {
                        ctx.renderer.live.write().distance_diffuse_metric = metric;
                        effects.mark_dirty = true;
                        effects.trigger_layout_recompute = true;
                    }
                }
                return Some(effects);
            }
            "threshold" => {
                if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.max(1e-6)) {
                    ctx.renderer.live.write().distance_diffuse_threshold = v;
                    effects.mark_dirty = true;
                    effects.trigger_layout_recompute = true;
                }
                return Some(effects);
            }
            "curve" => {
                if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.max(0.0)) {
                    ctx.renderer.live.write().distance_diffuse_curve = v;
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

#[cfg(test)]
mod freq_cutoff_clear_tests {
    use super::*;

    fn speaker_patch(json: &str) -> LayoutSpeakerPatch {
        serde_json::from_str(json).expect("valid LayoutSpeakerPatch")
    }

    /// The editor sends `freqLow: null` to remove a cutoff; an unrelated edit
    /// omits the field entirely. These must be distinguishable, which plain
    /// `Option<Option<f32>>` + `#[serde(default)]` was not (`null` collapsed to
    /// the outer `None`, so a clear was silently dropped).
    #[test]
    fn double_option_distinguishes_absent_null_and_value() {
        // Absent → leave untouched.
        assert_eq!(speaker_patch(r#"{"id":0}"#).freq_low, None);
        // Explicit null → clear the cutoff.
        assert_eq!(
            speaker_patch(r#"{"id":0,"freqLow":null}"#).freq_low,
            Some(None)
        );
        // A value → set the cutoff.
        assert_eq!(
            speaker_patch(r#"{"id":0,"freqLow":80.0}"#).freq_low,
            Some(Some(80.0))
        );
        // freq_high follows the same rule.
        assert_eq!(
            speaker_patch(r#"{"id":0,"freqHigh":null}"#).freq_high,
            Some(None)
        );
    }

    #[test]
    fn explicit_null_clears_an_existing_cutoff() {
        let mut speaker =
            renderer::speaker_layout::Speaker::from_polar("FL", 30.0, 0.0, 1.0, true, 0.0)
                .with_freq_low(120.0)
                .with_freq_high(18_000.0);
        assert_eq!(speaker.freq_low, Some(120.0));

        let changed =
            apply_layout_speaker_patch(&mut speaker, &speaker_patch(r#"{"id":0,"freqLow":null}"#));
        assert!(changed, "clearing a set cutoff is a change");
        assert_eq!(
            speaker.freq_low, None,
            "explicit null must blank the cutoff"
        );
        // freq_high was absent from the patch → left untouched.
        assert_eq!(speaker.freq_high, Some(18_000.0));
    }

    #[test]
    fn absent_cutoff_field_leaves_value_untouched() {
        let mut speaker =
            renderer::speaker_layout::Speaker::from_polar("FL", 30.0, 0.0, 1.0, true, 0.0)
                .with_freq_low(120.0);
        apply_layout_speaker_patch(
            &mut speaker,
            &speaker_patch(r#"{"id":0,"spatialize":true}"#),
        );
        assert_eq!(speaker.freq_low, Some(120.0));
    }
}
