use crate::command_table::{self, Command};
use crate::context::RuntimeControlContext;
use crate::osc_contract;
use omniphony_geometry::f32 as geometry;
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

/// How the clients learn that a control write changed the live state.
///
/// Every write that marks the config dirty goes through one notification path
/// in the engine (`notify_changed` in `orender_engine::osc::dispatch`): mark
/// the config dirty, broadcast `/state/config/saved = 0` so the Save button
/// lights, then publish the new value to *every* registered client — not just
/// the one that sent it, which already knows. A change no Save is for
/// ([`ControlEffects::view`], [`ControlEffects::transient`]) takes the same
/// publication without the first two steps. The variants only differ in how the value travels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Notify {
    /// Broadcast the full live-state bundle right away. For discrete edits (a
    /// toggle, a menu choice, a typed value).
    #[default]
    Snapshot,
    /// Let the bundle ride the OSC loop's live-state poll
    /// (`RendererControl::bump_live_state`, at most one bundle per poll tick),
    /// so a slider drag's burst of writes coalesces into a few bundles instead
    /// of one per tick.
    CoalescedSnapshot,
    /// Dirty flag and `/state/config/saved` only: the write publishes its value
    /// through a dedicated state address of its own (in `broadcasts`), and the
    /// next bundle carries it anyway.
    DirtyOnly,
}

#[derive(Debug, Clone, Default)]
pub struct ControlEffects {
    /// The write changed state the config file holds: mark it dirty and notify
    /// the clients, the way [`ControlEffects::notify`] says.
    pub mark_dirty: bool,
    /// The write changed live state that no Save is for
    /// (docs/persistence-policy.md): view state, which `persist` writes right
    /// away, or transient state, which is never written (a mute, a manual head
    /// pose). Publish it the way [`ControlEffects::notify`] says and leave the
    /// config clean.
    pub publish_only: bool,
    /// How clients learn about a `mark_dirty` or `publish_only` change.
    /// Ignored otherwise.
    pub notify: Notify,
    pub trigger_layout_recompute: bool,
    /// When `trigger_layout_recompute` is set, whether this change is
    /// evaluation-layer only (mode / grid resolution) — i.e. the backend geometry
    /// (triangulation + decorator metrics) is unchanged, so the recompute can
    /// reuse the existing gain models and rebuild only the evaluation wrapper.
    /// Default `false` = treat as a geometry change (full rebuild), which is the
    /// safe assumption; handlers known to be evaluation-only opt in.
    pub evaluation_only: bool,
    /// The change is where the evaluation grid comes from, alone: request
    /// the live grid (`RendererControl::request_live_grid`) instead of a
    /// rebuild, which it starts only when no topology on that grid is at
    /// hand.
    pub grid_request: bool,
    pub broadcasts: Vec<BroadcastUpdate>,
    pub log_message: Option<String>,
    /// Config fields to write straight to `config.yaml` (a targeted write, see
    /// [`crate::persist::persist_ops`]) instead of waiting for an explicit
    /// Save. The engine layer performs the I/O in
    /// `apply_control_effects`.
    pub persist: Vec<crate::persist::PersistOp>,
    /// Why the message, or part of it, was refused: returned to its sender
    /// (`/state/control_error`, `invalid_arguments`). The rest of the effects
    /// still apply, so a grouped write reports the pairs it dropped and keeps
    /// the ones it took.
    pub rejected: Option<String>,
}

impl ControlEffects {
    /// A config edit announced the way `notify` says.
    pub fn dirty(notify: Notify) -> Self {
        Self {
            mark_dirty: true,
            notify,
            ..Self::default()
        }
    }

    /// A view change announced the way `notify` says, persisted right away by
    /// `persist` and never marking the config dirty.
    pub fn view(notify: Notify, persist: crate::persist::PersistOp) -> Self {
        Self {
            publish_only: true,
            notify,
            persist: vec![persist],
            ..Self::default()
        }
    }

    /// A transient change announced the way `notify` says: never persisted,
    /// never marking the config dirty.
    pub fn transient(notify: Notify) -> Self {
        Self {
            publish_only: true,
            notify,
            ..Self::default()
        }
    }

    /// A message refused whole, for `reason`: nothing changes.
    pub fn rejected(reason: impl Into<String>) -> Self {
        Self {
            rejected: Some(reason.into()),
            ..Self::default()
        }
    }
}

/// Validate and apply a master gain (linear) from any control address.
///
/// `/control/gain` and `/control/realtime/master_gain` used to write the field
/// each in their own way — the realtime path with no check at all, so a NaN or
/// negative gain went straight to the audio thread (and on to the config as a
/// NaN dB value). Both now land here. Returns the applied gain, or `None` when
/// the value is rejected (non-finite or negative: a negative linear gain is a
/// polarity flip, never what a gain control means).
pub fn set_master_gain(control: &renderer::live_params::RendererControl, gain: f32) -> Option<f32> {
    if !gain.is_finite() || gain < 0.0 {
        log::warn!("OSC master gain: rejected value {gain}");
        return None;
    }
    control.live.write().master_gain = gain;
    Some(gain)
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
    /// The speaker's saved output gain, when the layout carries one.
    gain_db: Option<f32>,
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

/// Payload bytes per gain-table chunk to a datagram client (excluding the
/// 8-byte version+index header), kept well under the UDP MTU once OSC blob
/// framing is added.
pub const GAINTABLE_CHUNK_BYTES: usize = 1024;

/// Payload bytes per gain-table chunk to a stream client (#680): the table in
/// a few packets rather than hundreds of datagrams, each well under the
/// stream's packet bound (`osc_contract::stream::MAX_PACKET`). Nothing is
/// lost on a stream, so the NACK path never has to resend these; when asked,
/// it chunks the same way for the same client.
pub const GAINTABLE_STREAM_CHUNK_BYTES: usize = 512 << 10;

/// Stable 31-bit id of a serialized table, so a re-request returns the same
/// version while a topology rebuild yields a new one.
pub fn gaintable_version(bytes: &[u8]) -> u32 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    (hasher.finish() & 0x7fff_ffff) as u32
}

/// Chunk an already-serialized gain table into OSC broadcasts: a `meta` header
/// (JSON: version, total_len, chunk_count, chunk_bytes) followed by `chunk` blobs,
/// each prefixed with `version` + chunk index, `chunk_bytes` of table each
/// ([`GAINTABLE_CHUNK_BYTES`] for a datagram client,
/// [`GAINTABLE_STREAM_CHUNK_BYTES`] for a stream one). `only = Some((version, missing))`
/// re-emits just those chunk indices when the version still matches (NACK resend),
/// else falls back to a full send. `None` = full send with `meta`.
///
/// Pure: deterministic for given bytes, so a NACK resend reproduces the same
/// chunking the client expects.
pub fn gaintable_chunk_broadcasts(
    bytes: &[u8],
    only: Option<(u32, Vec<u32>)>,
    chunk_bytes: usize,
) -> Vec<BroadcastUpdate> {
    let chunk_bytes = chunk_bytes.max(1);
    let version = gaintable_version(bytes);
    let chunk_count = bytes.len().div_ceil(chunk_bytes).max(1);

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
            "chunk_bytes": chunk_bytes,
        })
        .to_string();
        out.push(BroadcastUpdate {
            addr: osc_contract::STATE_DEBUG_SPEAKER_GAINTABLE_META.to_string(),
            value: BroadcastValue::String(meta),
        });
    }
    for ci in indices {
        let start = ci * chunk_bytes;
        let end = (start + chunk_bytes).min(bytes.len());
        let mut blob = Vec::with_capacity(8 + (end - start));
        blob.extend_from_slice(&version.to_le_bytes());
        blob.extend_from_slice(&(ci as u32).to_le_bytes());
        blob.extend_from_slice(&bytes[start..end]);
        out.push(BroadcastUpdate {
            addr: osc_contract::STATE_DEBUG_SPEAKER_GAINTABLE_CHUNK.to_string(),
            value: BroadcastValue::Blob(blob),
        });
    }
    out
}

/// Any OSC numeric argument, as `f64`.
///
/// OSC has four numeric tags — `i`, `h`, `f`, `d` — and the *sender* picks one,
/// not the receiver: Studio's sliders send `f`, a scripted client sends `i` for
/// the same control, and most Python bindings turn `1.0` into `d`. Which tag
/// arrived says nothing about what was meant, so the parsers below read the
/// value and treat the tag as a transport detail.
///
/// This is the one place that decides what counts as a number. Handlers used to
/// answer that question inline and disagreed with each other, so whether a
/// control accepted your message depended on which address you sent it to — and
/// a rejected message was dropped in silence, with no error and no log.
fn numeric(arg: &OscType) -> Option<f64> {
    match arg {
        OscType::Int(i) => Some(*i as f64),
        OscType::Long(l) => Some(*l as f64),
        OscType::Float(f) => Some(*f as f64),
        OscType::Double(d) => Some(*d),
        _ => None,
    }
}

/// A boolean argument: the `T`/`F` tags, or any number read as zero/non-zero.
pub fn parse_bool_arg(arg: Option<&OscType>) -> Option<bool> {
    match arg? {
        OscType::Bool(b) => Some(*b),
        other => numeric(other).map(|v| v != 0.0),
    }
}

pub fn parse_positive_u32_arg(arg: Option<&OscType>) -> Option<u32> {
    numeric(arg?).filter(|v| *v > 0.0).map(|v| v as u32)
}

pub fn parse_nonnegative_u32_arg(arg: Option<&OscType>) -> Option<u32> {
    numeric(arg?).filter(|v| *v >= 0.0).map(|v| v as u32)
}

pub fn parse_positive_f32_arg(arg: Option<&OscType>) -> Option<f32> {
    numeric(arg?).filter(|v| *v > 0.0).map(|v| v as f32)
}

pub fn parse_nonnegative_f32_arg(arg: Option<&OscType>) -> Option<f32> {
    numeric(arg?).filter(|v| *v >= 0.0).map(|v| v as f32)
}

pub fn parse_f32_arg(arg: Option<&OscType>) -> Option<f32> {
    numeric(arg?).map(|v| v as f32)
}

/// A plugin parameter value from an OSC argument, as sent: a number, a bool
/// or a string. A non-finite number is refused; the store reads the rest in
/// the parameter's declared type.
pub fn parse_param_value(arg: &OscType) -> Option<renderer::backend_params::ParamValue> {
    use renderer::backend_params::ParamValue;
    match arg {
        OscType::Float(f) => f.is_finite().then_some(ParamValue::Float(*f)),
        OscType::Double(d) => d.is_finite().then_some(ParamValue::Float(*d as f32)),
        OscType::Int(i) => Some(ParamValue::Int(*i as i64)),
        OscType::Long(i) => Some(ParamValue::Int(*i)),
        OscType::Bool(b) => Some(ParamValue::Bool(*b)),
        OscType::String(s) => Some(ParamValue::Text(s.clone())),
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
        let (azimuth, elevation, distance) = geometry::hydrate_from_cartesian(x, y, z);
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
        let (x, y, z) = geometry::hydrate_from_spherical(azimuth, elevation, distance);
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
    if let Some(gain_db) = patch.gain_db.filter(|value| value.is_finite()) {
        speaker.gain_db = gain_db.max(renderer::live_params::SPEAKER_GAIN_FLOOR_DB);
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
        let (azimuth, elevation, distance) = geometry::hydrate_from_cartesian(x, y, z);
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
    command_table::find(SIMPLE_CONTROL_COMMANDS, &msg.addr).and_then(|run| run(msg, ctx))
}

/// A handler of [`SIMPLE_CONTROL_COMMANDS`]; `None` passes the message on
/// (to the host).
pub type SimpleHandler = fn(&OscMessage, &RuntimeControlContext) -> Option<ControlEffects>;

/// The core's control addresses that are not live options (see
/// `command_table`). `metering/rate_hz` and `diag/rate_hz` are
/// `live_control`'s, against `RendererControl`.
pub static SIMPLE_CONTROL_COMMANDS: &[Command<SimpleHandler>] = &[
    Command::exact(osc_contract::CONTROL_CONFIG_LAYOUT, config_layout),
    Command::exact(
        osc_contract::CONTROL_CONFIG_LAYOUT_APPLY,
        config_layout_apply,
    ),
    Command::exact(osc_contract::CONTROL_CONFIG_SPEAKERS, config_speakers),
    Command::exact(osc_contract::CONTROL_SPEAKER_TEST, speaker_test),
    Command::exact(osc_contract::CONTROL_OBJECT_TEST, object_test),
    Command::exact(
        osc_contract::CONTROL_OBJECT_TEST_ROTATION,
        object_test_rotation,
    ),
    Command::exact(osc_contract::CONTROL_OBJECT_TEST_CLIP, object_test_clip),
    Command::exact(
        osc_contract::CONTROL_SPEAKER_TEST_IDLE_FEED,
        speaker_test_idle_feed,
    ),
    Command::exact(osc_contract::CONTROL_BINAURAL_EAR_GAIN, binaural_ear_gain),
    Command::exact(osc_contract::CONTROL_BINAURAL_EAR_MUTE, binaural_ear_mute),
    Command::exact(osc_contract::CONTROL_HEAD_ORIENTATION, head_orientation),
    Command::exact(osc_contract::CONTROL_HEAD_QUAT, head_quat),
    Command::exact(
        osc_contract::CONTROL_BINAURAL_HRTF_UPLOAD_BEGIN,
        binaural_hrtf_upload_begin,
    ),
    Command::exact(
        osc_contract::CONTROL_BINAURAL_HRTF_UPLOAD_CHUNK,
        binaural_hrtf_upload_chunk,
    ),
    Command::exact(
        osc_contract::CONTROL_BINAURAL_HRTF_UPLOAD_END,
        binaural_hrtf_upload_end,
    ),
    Command::exact(osc_contract::CONTROL_HEAD_CALIBRATE, head_calibrate),
    Command::exact(osc_contract::CONTROL_HEAD_RECENTER, head_recenter),
    Command::exact(
        osc_contract::CONTROL_RENDER_BACKEND_RESTORE,
        render_backend_restore,
    ),
    Command::exact(osc_contract::CONTROL_BACKEND_PARAM, backend_param),
    Command::exact(
        osc_contract::CONTROL_RENDER_EVALUATION_MODE_FROM_FILE,
        render_evaluation_mode_from_file,
    ),
    Command::exact(osc_contract::CONTROL_LAYOUT_RADIUS_M, layout_radius_m),
    Command::exact(
        osc_contract::CONTROL_SPREAD_FROM_DISTANCE,
        spread_from_distance,
    ),
    Command::exact(
        osc_contract::CONTROL_SPREAD_SIZE_TO_SPREAD_MODE,
        spread_size_to_spread_mode,
    ),
    Command::exact(osc_contract::CONTROL_SPREAD_MIN, spread_min),
    Command::exact(osc_contract::CONTROL_SPREAD_MAX, spread_max),
    Command::exact(
        osc_contract::CONTROL_SPREAD_DISTANCE_RANGE,
        spread_distance_range,
    ),
    Command::exact(
        osc_contract::CONTROL_SPREAD_DISTANCE_CURVE,
        spread_distance_curve,
    ),
    Command::prefix(osc_contract::CONTROL_HYBRID_PREFIX, hybrid),
    Command::prefix(osc_contract::CONTROL_OBJECT_PREFIX, object),
];

// VBAP spread tuning is a generic backend param now (baked into the backend
// at build, keyed by backend id "vbap"). These dedicated addresses are kept
// as thin aliases over `set_backend_param` so existing OSC clients keep
// working; the canonical path is `/omniphony/control/backend/param`. All of
// them trigger a topology rebuild (the values are baked, not per-request).
macro_rules! spread_param_with_recompute {
    ($name:ident, $key:literal) => {
        fn $name(msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
            let mut effects = ControlEffects::default();
            if let Some(value) = parse_f32_arg(msg.args.first()) {
                ctx.renderer.set_backend_param(
                    "vbap",
                    $key,
                    renderer::backend_params::ParamValue::Float(value),
                );
                effects.mark_dirty = true;
                effects.trigger_layout_recompute = true;
            }
            Some(effects)
        }
    };
}

spread_param_with_recompute!(spread_min, "spread_min");
spread_param_with_recompute!(spread_max, "spread_max");
spread_param_with_recompute!(spread_distance_range, "spread_distance_range");
spread_param_with_recompute!(spread_distance_curve, "spread_distance_curve");

/// The hybrid curve: a point list, kept out of the registry. The legs,
/// smoothing and metric under the same prefix are registry aliases, handled
/// before this table; any other tail is consumed and ignored.
fn hybrid(msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    let mut effects = ControlEffects::default();
    let rest = msg.addr.strip_prefix(osc_contract::CONTROL_HYBRID_PREFIX)?;
    let mut live = ctx.renderer.live.write();
    let mut changed = false;
    match rest {
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
    Some(effects)
}

/// `object/<index>/mute`; any other tail passes on.
fn object(msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    let mut effects = ControlEffects::default();
    let rest = msg.addr.strip_prefix(osc_contract::CONTROL_OBJECT_PREFIX)?;
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
                // Transient, like a speaker mute: its own state address
                // publishes it, and no Save is for it.
                effects.broadcasts.push(BroadcastUpdate {
                    addr: format!("/omniphony/state/object/{}/mute", idx),
                    value: BroadcastValue::Int(if muted { 1 } else { 0 }),
                });
                effects.log_message = Some(format!("OSC: object[{}] mute → {}", idx, muted));
            }
        }
        return Some(effects);
    }
    None
}

/// Gain-table chunking at both sizes (#680, step 3).
#[cfg(test)]
mod gaintable_chunk_tests {
    use super::*;

    fn chunks(updates: &[BroadcastUpdate]) -> Vec<(u32, Vec<u8>)> {
        updates
            .iter()
            .filter_map(|u| match &u.value {
                BroadcastValue::Blob(blob) => Some((
                    u32::from_le_bytes(blob[4..8].try_into().unwrap()),
                    blob[8..].to_vec(),
                )),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_table_chunks_back_to_itself_at_either_size() {
        let table: Vec<u8> = (0..(3 << 19)).map(|i| (i * 7 % 251) as u8).collect();
        for (size, count) in [
            (GAINTABLE_CHUNK_BYTES, 1536),
            (GAINTABLE_STREAM_CHUNK_BYTES, 3),
        ] {
            let updates = gaintable_chunk_broadcasts(&table, None, size);
            let BroadcastValue::String(meta) = &updates[0].value else {
                panic!("meta first")
            };
            let meta: serde_json::Value = serde_json::from_str(meta).unwrap();
            assert_eq!(meta["chunk_count"], count);
            assert_eq!(meta["chunk_bytes"], size);
            let chunks = chunks(&updates);
            assert_eq!(chunks.len(), count);
            let joined: Vec<u8> = chunks.into_iter().flat_map(|(_, bytes)| bytes).collect();
            assert_eq!(joined, table);
        }
    }

    /// A NACK is answered with the chunks of the same chunking.
    #[test]
    fn a_nack_resends_the_chunks_of_that_size() {
        let table = vec![9u8; GAINTABLE_STREAM_CHUNK_BYTES * 2 + 10];
        let version = gaintable_version(&table);
        let resent = chunks(&gaintable_chunk_broadcasts(
            &table,
            Some((version, vec![2])),
            GAINTABLE_STREAM_CHUNK_BYTES,
        ));
        assert_eq!(resent, [(2, vec![9u8; 10])]);
    }
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

    /// Every OSC numeric tag must reach the same value.
    ///
    /// The sender picks the tag: Studio sends `f`, a scripted client sends `i`,
    /// most Python bindings turn `1.0` into `d`. Before these parsers agreed,
    /// which of those a control accepted depended on the address you sent it
    /// to, and the ones it did not accept were dropped without a word.
    #[test]
    fn every_numeric_tag_parses_to_the_same_value() {
        let tags = [
            OscType::Int(3),
            OscType::Long(3),
            OscType::Float(3.0),
            OscType::Double(3.0),
        ];
        for tag in &tags {
            assert_eq!(parse_f32_arg(Some(tag)), Some(3.0), "{tag:?}");
            assert_eq!(parse_positive_f32_arg(Some(tag)), Some(3.0), "{tag:?}");
            assert_eq!(parse_nonnegative_f32_arg(Some(tag)), Some(3.0), "{tag:?}");
            assert_eq!(parse_positive_u32_arg(Some(tag)), Some(3), "{tag:?}");
            assert_eq!(parse_nonnegative_u32_arg(Some(tag)), Some(3), "{tag:?}");
            assert_eq!(parse_bool_arg(Some(tag)), Some(true), "{tag:?}");
        }
    }

    /// A `T`/`F` argument is the natural way to send a toggle, and it used to
    /// work on some addresses and be silently ignored on others.
    #[test]
    fn bool_tags_and_numbers_both_read_as_booleans() {
        assert_eq!(parse_bool_arg(Some(&OscType::Bool(true))), Some(true));
        assert_eq!(parse_bool_arg(Some(&OscType::Bool(false))), Some(false));
        for zero in [
            OscType::Int(0),
            OscType::Long(0),
            OscType::Float(0.0),
            OscType::Double(0.0),
        ] {
            assert_eq!(parse_bool_arg(Some(&zero)), Some(false), "{zero:?}");
        }
        assert_eq!(parse_bool_arg(Some(&OscType::Float(-1.0))), Some(true));
    }

    /// Widening must not have swallowed the sign filters.
    #[test]
    fn sign_filters_still_reject_out_of_range_values() {
        for negative in [OscType::Int(-1), OscType::Double(-0.5)] {
            assert_eq!(
                parse_positive_u32_arg(Some(&negative)),
                None,
                "{negative:?}"
            );
            assert_eq!(
                parse_positive_f32_arg(Some(&negative)),
                None,
                "{negative:?}"
            );
            assert_eq!(
                parse_nonnegative_u32_arg(Some(&negative)),
                None,
                "{negative:?}"
            );
        }
        assert_eq!(parse_positive_u32_arg(Some(&OscType::Int(0))), None);
        assert_eq!(parse_nonnegative_u32_arg(Some(&OscType::Int(0))), Some(0));
        // NaN is not a value any of these should accept.
        assert_eq!(
            parse_positive_f32_arg(Some(&OscType::Float(f32::NAN))),
            None
        );
        assert_eq!(
            parse_nonnegative_f32_arg(Some(&OscType::Float(f32::NAN))),
            None
        );
    }

    /// Widening the numeric tags must not turn non-numbers into numbers.
    #[test]
    fn non_numeric_arguments_are_still_rejected() {
        for junk in [
            OscType::String("7".to_string()),
            OscType::Blob(vec![1, 2, 3]),
            OscType::Nil,
            OscType::Char('7'),
        ] {
            assert_eq!(parse_f32_arg(Some(&junk)), None, "{junk:?}");
            assert_eq!(parse_bool_arg(Some(&junk)), None, "{junk:?}");
            assert_eq!(parse_positive_u32_arg(Some(&junk)), None, "{junk:?}");
        }
        assert_eq!(parse_f32_arg(None), None);
        assert_eq!(parse_bool_arg(None), None);
    }
}

/// What a write does to the Save button (docs/persistence-policy.md).
#[cfg(test)]
mod persistence_class_tests {
    use super::*;

    fn apply(addr: &str, args: Vec<OscType>) -> (ControlEffects, RuntimeControlContext) {
        let ctx = RuntimeControlContext::new(crate::test_support::fixture_control());
        let msg = OscMessage {
            addr: addr.to_string(),
            args,
        };
        let effects = apply_simple_osc_control(&msg, &ctx).expect("handled");
        (effects, ctx)
    }

    /// Mutes and a manual head pose are listening gestures: published to
    /// every client, never saved, so they must not light the Save button.
    #[test]
    fn transient_writes_leave_the_config_clean() {
        let (effects, ctx) = apply(
            osc_contract::CONTROL_CONFIG_SPEAKERS,
            vec![OscType::String(
                r#"{"speakerEdits":[{"id":0,"muted":true}]}"#.into(),
            )],
        );
        assert!(!effects.mark_dirty, "speaker mute");
        assert!(effects.publish_only, "speaker mute is still published");
        assert!(ctx.renderer.live.read().speakers[&0].muted);

        let (effects, _) = apply(
            osc_contract::CONTROL_HEAD_ORIENTATION,
            vec![
                OscType::Float(30.0),
                OscType::Float(0.0),
                OscType::Float(0.0),
            ],
        );
        assert!(!effects.mark_dirty, "head orientation");
        assert!(effects.publish_only);

        let (effects, ctx) = apply(
            &format!("{}3/mute", osc_contract::CONTROL_OBJECT_PREFIX),
            vec![OscType::Int(1)],
        );
        assert!(!effects.mark_dirty, "object mute");
        assert_eq!(
            effects.broadcasts.len(),
            1,
            "object mute is still published"
        );
        assert!(ctx.renderer.live.read().objects[&3].muted);
    }

    /// A delay in the same patch as a mute is a setting: the patch dirties.
    #[test]
    fn a_delay_next_to_a_mute_still_waits_for_save() {
        let (effects, _) = apply(
            osc_contract::CONTROL_CONFIG_SPEAKERS,
            vec![OscType::String(
                r#"{"speakerEdits":[{"id":0,"muted":true,"delayMs":2.5}]}"#.into(),
            )],
        );
        assert!(effects.mark_dirty);
    }

    /// A replaced layout seeds the live output gains from the speakers it
    /// carries, as a boot does.
    #[test]
    fn a_replaced_layout_seeds_its_speaker_gains() {
        let (_, ctx) = apply(
            osc_contract::CONTROL_CONFIG_LAYOUT,
            vec![OscType::String(
                r#"{"replaceLayout":{"speakers":[{"name":"L","azimuth":30,"gainDb":-6},{"name":"R","azimuth":-30}]}}"#
                    .into(),
            )],
        );
        let live = ctx.renderer.live.read();
        assert!((live.speakers[&0].gain - 0.501).abs() < 1e-3);
        assert!(!live.speakers.contains_key(&1), "unity needs no entry");
    }
}

fn config_layout(msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    let effects = ControlEffects::default();
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
            // swap invalidates every entry: reseed them from the new
            // speakers' delays and gains.
            // (The live params are written after the layout's lock is
            // released, so the live write guard never nests with it.)
            let speakers = ctx.renderer.with_editable_layout(|layout| {
                if let Some(radius_m) = replace.radius_m {
                    layout.radius_m = radius_m.max(0.01);
                }
                layout.speakers = new_speakers;
                renderer::live_params::speaker_live_from_layout(layout)
            });
            ctx.renderer.live.write().speakers = speakers;
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
    Some(effects)
}

fn config_layout_apply(_msg: &OscMessage, _ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    Some(ControlEffects {
        mark_dirty: true,
        trigger_layout_recompute: true,
        log_message: Some("OSC: layout config apply".to_string()),
        ..Default::default()
    })
}

fn config_speakers(msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    let mut effects = ControlEffects::default();
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
                // A mute is a listening gesture, not a setting: it is
                // published, never saved (docs/persistence-policy.md).
                if let Some(muted) = speaker_patch.muted {
                    ctx.renderer
                        .live
                        .write()
                        .speakers
                        .entry(speaker_patch.id)
                        .or_default()
                        .muted = muted;
                    ctx.renderer.mark_speaker_params_dirty();
                    effects.publish_only = true;
                }
            }
        }
        if changed {
            effects.mark_dirty = true;
        }
    }
    Some(effects)
}

fn speaker_test(msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    let addr = msg.addr.as_str();
    let mut effects = ControlEffects::default();
    // Start/stop the per-speaker test signal. A negative index stops: the
    // client owns the trigger policy (hold, fixed burst, toggle), so the
    // renderer only ever sees "play this" or "stop".
    let idx = match msg.args.first() {
        Some(OscType::Int(v)) => *v,
        _ => {
            log::warn!("OSC {addr}: expected [speaker_idx, level, isolation]");
            return Some(effects);
        }
    };
    let next = if idx < 0 {
        None
    } else {
        let level = match msg.args.get(1) {
            Some(OscType::Float(v)) => v.clamp(0.0, 1.0),
            _ => 0.1,
        };
        let isolation = parse_string_arg(msg.args.get(2))
            .and_then(|v| renderer::live_params::TestIsolation::from_str(&v))
            .unwrap_or_default();
        Some(renderer::live_params::SpeakerTest {
            speaker_idx: idx as usize,
            level,
            isolation,
        })
    };
    let mut live = ctx.renderer.live.write();
    if live.speaker_test != next {
        live.speaker_test = next;
        // Deliberately NOT mark_dirty: the test is transient and must never
        // reach the config or a live-handoff sidecar.
        effects.log_message = Some(match next {
            Some(t) => format!(
                "OSC: speaker_test -> speaker {} at {:.3} ({})",
                t.speaker_idx,
                t.level,
                t.isolation.as_str()
            ),
            None => "OSC: speaker_test -> off".to_string(),
        });
    }
    Some(effects)
}

fn object_test(msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    let addr = msg.addr.as_str();
    let mut effects = ControlEffects::default();
    // Start/move/stop the object test. Studio sends one of these per pointer
    // move while dragging, so the common case is a position update on an
    // already-running test: keep it allocation-free and let the renderer
    // ramp, rather than treating a move as a stop-then-start.
    let Some(on) = parse_bool_arg(msg.args.first()) else {
        log::warn!("OSC {addr}: expected [on, x, y, z, level, size, isolation]");
        return Some(effects);
    };
    let next = if !on {
        None
    } else {
        let axis = |i: usize| match msg.args.get(i) {
            Some(OscType::Float(v)) => v.clamp(-1.0, 1.0),
            _ => 0.0,
        };
        // Default y = 1.0 (front centre) rather than 0.0 if absent, matching
        // the renderer's neutral object position.
        let position = [
            axis(1),
            match msg.args.get(2) {
                Some(OscType::Float(v)) => v.clamp(-1.0, 1.0),
                _ => 1.0,
            },
            axis(3),
        ];
        let level = match msg.args.get(4) {
            Some(OscType::Float(v)) => v.clamp(0.0, 1.0),
            _ => 0.1,
        };
        let size = match msg.args.get(5) {
            Some(OscType::Float(v)) => v.clamp(0.0, 1.0),
            _ => 0.0,
        };
        let isolation = parse_string_arg(msg.args.get(6))
            .and_then(|v| renderer::live_params::TestIsolation::from_str(&v))
            .unwrap_or_default();
        // Absent = pink noise, so a client that predates the signal
        // selector keeps getting exactly what it used to.
        let signal = parse_string_arg(msg.args.get(7))
            .and_then(|v| renderer::live_params::ObjectTestSignal::from_str(&v))
            .unwrap_or_default();
        Some(renderer::live_params::ObjectTest {
            position,
            size: [size; 3],
            level,
            isolation,
            signal,
        })
    };
    let mut live = ctx.renderer.live.write();
    if live.object_test != next {
        let was_running = live.object_test.is_some();
        let was_signal = live.object_test.map(|t| t.signal);
        live.object_test = next;
        // Deliberately NOT mark_dirty: transient like `speaker_test`, and it
        // must never reach the config or a live-handoff sidecar.
        //
        // Log only the edges. A drag is a burst of position updates, and
        // logging each one would bury the session log in noise about noise.
        effects.log_message = match (was_running, next) {
            (false, Some(t)) => Some(format!(
                "OSC: object_test -> on at [{:.2}, {:.2}, {:.2}] {:.3} ({}, {})",
                t.position[0],
                t.position[1],
                t.position[2],
                t.level,
                t.isolation.as_str(),
                t.signal.as_str()
            )),
            (true, None) => Some("OSC: object_test -> off".to_string()),
            (true, Some(t)) => was_signal
                .filter(|prev| *prev != t.signal)
                .map(|_| format!("OSC: object_test signal -> {}", t.signal.as_str())),
            _ => None,
        };
    }
    Some(effects)
}

fn object_test_rotation(msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    let addr = msg.addr.as_str();
    let mut effects = ControlEffects::default();
    let Some(axis_name) = parse_string_arg(msg.args.first()) else {
        log::warn!("OSC {addr}: expected [axis, radius, period, azimuth, elevation]");
        return Some(effects);
    };
    let float_at = |i: usize, fallback: f32| match msg.args.get(i) {
        Some(OscType::Float(v)) => *v,
        _ => fallback,
    };
    let axis = match renderer::live_params::RotationAxis::from_str(&axis_name) {
        Some(renderer::live_params::RotationAxis::Free { .. }) => {
            renderer::live_params::RotationAxis::Free {
                azimuth_deg: float_at(3, 0.0),
                elevation_deg: float_at(4, 0.0),
            }
        }
        Some(other) => other,
        None => {
            log::warn!("OSC {addr}: unknown axis {axis_name:?}");
            return Some(effects);
        }
    };
    let next = renderer::live_params::ObjectTestRotation {
        axis,
        // 4 covers every distance worth reaching: √3 gets to a room corner
        // from the centre, 2√3 gets there from the opposite one.
        radius: float_at(1, 0.0).clamp(0.0, 4.0),
        // Floored well above zero: a period approaching it is not a fast
        // orbit, it is a discontinuity.
        period_s: float_at(2, 4.0).clamp(0.05, 600.0),
    };
    let mut live = ctx.renderer.live.write();
    if live.object_test_rotation != next {
        let was_active = live.object_test_rotation.is_active();
        live.object_test_rotation = next;
        // Deliberately NOT mark_dirty: transient like the test itself.
        // Log only the edges — a diameter slider drag is a burst.
        effects.log_message = match (was_active, next.is_active()) {
            (false, true) => Some(format!(
                "OSC: object_test rotation -> {} axis, radius {:.2}, {:.2} s/turn",
                next.axis.as_str(),
                next.radius,
                next.period_s
            )),
            (true, false) => Some("OSC: object_test rotation -> off".to_string()),
            _ => None,
        };
    }
    Some(effects)
}

fn object_test_clip(msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    let mut effects = ControlEffects::default();
    // Choosing the file the `clip` signal plays. Everything expensive
    // happens here, on the control thread: read, downmix, resample,
    // normalise. The render path gets an array and an index.
    let path = parse_string_arg(msg.args.first()).unwrap_or_default();
    let path = path.trim().to_string();
    let state = if path.is_empty() {
        ctx.renderer.live.write().object_test_clip = None;
        effects.log_message = Some("OSC: object_test clip -> cleared".to_string());
        "{}".to_string()
    } else {
        let rate = ctx
            .renderer
            .sample_rate
            .load(std::sync::atomic::Ordering::Relaxed)
            .max(1);
        match renderer::object_test::clip::load(&path, rate) {
            Ok(clip) => {
                let name = std::path::Path::new(&clip.path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| clip.path.clone());
                effects.log_message = Some(format!(
                    "OSC: object_test clip -> {} ({:.1} s, {} Hz, {} ch{})",
                    name,
                    clip.duration_s(),
                    clip.source_rate,
                    clip.source_channels,
                    if clip.truncated { ", truncated" } else { "" }
                ));
                let json = format!(
                    "{{\"name\":{},\"path\":{},\"seconds\":{:.3},\"sourceRate\":{},\"channels\":{},\"truncated\":{}}}",
                    serde_json::Value::from(name.as_str()),
                    serde_json::Value::from(clip.path.as_str()),
                    clip.duration_s(),
                    clip.source_rate,
                    clip.source_channels,
                    clip.truncated
                );
                ctx.renderer.live.write().object_test_clip = Some(std::sync::Arc::new(clip));
                json
            }
            Err(e) => {
                // Left as it was on failure: dropping a working clip because
                // the next pick was unreadable would be a second surprise on
                // top of the first.
                effects.log_message = Some(format!("OSC: object_test clip refused: {e}"));
                format!("{{\"error\":{}}}", serde_json::Value::from(e.as_str()))
            }
        }
    };
    effects.broadcasts.push(BroadcastUpdate {
        addr: osc_contract::STATE_OBJECT_TEST_CLIP.to_string(),
        value: BroadcastValue::String(state),
    });
    Some(effects)
}

fn speaker_test_idle_feed(msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    let addr = msg.addr.as_str();
    let mut effects = ControlEffects::default();
    // Arm/disarm the idle feed that keeps the output chain warm while the
    // client's test pane is open. Arming always bumps the generation (even
    // when already armed) so the decode loop refreshes its keepalive
    // deadline on every re-arm.
    let Some(on) = parse_bool_arg(msg.args.first()) else {
        log::warn!("OSC {addr}: expected [on]");
        return Some(effects);
    };
    // Deliberately NOT mark_dirty: transient like speaker_test. Log only
    // the arm/disarm edges, not the periodic re-arms.
    let mut live = ctx.renderer.live.write();
    if on {
        if live.speaker_test_idle_feed_gen == 0 {
            effects.log_message = Some("OSC: speaker_test idle feed -> armed".to_string());
        }
        live.speaker_test_idle_feed_gen = live.speaker_test_idle_feed_gen.wrapping_add(1).max(1);
    } else if live.speaker_test_idle_feed_gen != 0 {
        live.speaker_test_idle_feed_gen = 0;
        effects.log_message = Some("OSC: speaker_test idle feed -> off".to_string());
    }
    Some(effects)
}

fn binaural_ear_gain(msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    let mut effects = ControlEffects::default();
    // Headphone L/R output gain: [ear_idx (0|1), linear_gain]. Dedicated
    // params — the ears no longer ride the first two per-speaker slots
    // (those drive the virtual FL/FR in cascaded mode).
    let idx = parse_nonnegative_u32_arg(msg.args.first());
    let gain = parse_f32_arg(msg.args.get(1));
    if let (Some(idx @ 0..=1), Some(gain)) = (idx, gain) {
        if gain.is_finite() && (0.0..=4.0).contains(&gain) {
            let mut live = ctx.renderer.live.write();
            let ear = &mut live.binaural.ears[idx as usize];
            if ear.gain != gain {
                ear.gain = gain;
                effects.mark_dirty = true;
                effects.log_message = Some(format!("OSC: binaural ear_gain {idx} -> {gain}"));
            }
        }
    }
    Some(effects)
}

fn binaural_ear_mute(msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    let mut effects = ControlEffects::default();
    // Headphone L/R mute: [ear_idx (0|1), 0|1].
    let idx = parse_nonnegative_u32_arg(msg.args.first());
    let mute = parse_bool_arg(msg.args.get(1));
    if let (Some(idx @ 0..=1), Some(muted)) = (idx, mute) {
        let mut live = ctx.renderer.live.write();
        let ear = &mut live.binaural.ears[idx as usize];
        if ear.muted != muted {
            ear.muted = muted;
            effects.mark_dirty = true;
            effects.log_message = Some(format!("OSC: binaural ear_mute {idx} -> {muted}"));
        }
    }
    Some(effects)
}

fn head_orientation(msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    let mut effects = ControlEffects::default();
    // Static head pose from Euler degrees [yaw, pitch, roll]. The live
    // head-tracking input (SensorsOSC) lands in M2; this lets Studio / tests
    // drive the pose directly. No topology rebuild (binaural is topology-free).
    let yaw = parse_f32_arg(msg.args.first()).unwrap_or(0.0);
    let pitch = parse_f32_arg(msg.args.get(1)).unwrap_or(0.0);
    let roll = parse_f32_arg(msg.args.get(2)).unwrap_or(0.0);
    let mut live = ctx.renderer.live.write();
    live.binaural.head_pose = renderer::binaural::HeadPose::from_euler_deg(yaw, pitch, roll);
    // A manual pose, like the tracker's, is transient: never saved.
    effects.publish_only = true;
    effects.log_message = Some(format!("OSC: head/orientation -> {yaw},{pitch},{roll}"));
    Some(effects)
}

fn head_quat(msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    let mut effects = ControlEffects::default();
    // Static head pose from a raw quaternion [w, x, y, z].
    let w = parse_f32_arg(msg.args.first()).unwrap_or(1.0);
    let x = parse_f32_arg(msg.args.get(1)).unwrap_or(0.0);
    let y = parse_f32_arg(msg.args.get(2)).unwrap_or(0.0);
    let z = parse_f32_arg(msg.args.get(3)).unwrap_or(0.0);
    let mut live = ctx.renderer.live.write();
    live.binaural.head_pose = renderer::binaural::HeadPose::from_quat(w, x, y, z);
    // A manual pose, like the tracker's, is transient: never saved.
    effects.publish_only = true;
    effects.log_message = Some("OSC: head/quat".to_string());
    Some(effects)
}

/// ── SOFA upload (Studio → renderer, chunked OSC blobs) ──────────────────
/// For setups where Studio does not share a filesystem with the renderer:
/// begin [s name, i total_bytes] → chunk [i seq, b data]* → end [i chunks].
/// The file lands in <config dir>/hrtf/ and is activated on completion.
fn binaural_hrtf_upload_begin(
    msg: &OscMessage,
    _ctx: &RuntimeControlContext,
) -> Option<ControlEffects> {
    let mut effects = ControlEffects::default();
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
    Some(effects)
}

fn binaural_hrtf_upload_chunk(
    msg: &OscMessage,
    _ctx: &RuntimeControlContext,
) -> Option<ControlEffects> {
    let mut effects = ControlEffects::default();
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
    Some(effects)
}

fn binaural_hrtf_upload_end(
    msg: &OscMessage,
    ctx: &RuntimeControlContext,
) -> Option<ControlEffects> {
    let mut effects = ControlEffects::default();
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
                    let source = renderer::binaural::HrirSource::Sofa(path.clone());
                    let mut live = ctx.renderer.live.write();
                    renderer::options::remember_hrir_file(&mut live.binaural, &source);
                    live.binaural.hrir_source = source;
                    drop(live);
                    effects.mark_dirty = true;
                    effects.log_message = Some(format!("hrtf upload complete, activated: {path}"));
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
    Some(effects)
}

fn head_calibrate(msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    let mut effects = ControlEffects::default();
    let step = msg.args.first().and_then(|a| match a {
        rosc::OscType::String(s) => renderer::binaural::CalibrationStep::from_str(s),
        _ => None,
    });
    let Some(step) = step else {
        effects.log_message =
            Some("OSC: head/calibrate expects front | left | up | reset".to_string());
        return Some(effects);
    };
    let mut live = ctx.renderer.live.write();
    match live.binaural.tracking.calibrate(step) {
        Ok(done) => {
            if step == renderer::binaural::CalibrationStep::Front {
                // Looking ahead is the recenter: snap and persist it.
                live.binaural.head_pose = renderer::binaural::HeadPose::identity();
                effects.persist.push(crate::persist::PersistOp::HEAD_CENTER);
            }
            if done || step == renderer::binaural::CalibrationStep::Reset {
                effects.persist.push(crate::persist::PersistOp::HEAD_AXES);
            }
            // A calibration of the sensor on the listener's head: written
            // at once, never behind the Save button
            // (docs/persistence-policy.md).
            effects.publish_only = true;
            effects.log_message = Some(format!(
                "OSC: head/calibrate {step:?}{}",
                if done { " — axes calibrated" } else { "" }
            ));
        }
        Err(reason) => {
            // Nothing changed, but the step's state is published.
            effects.publish_only = true;
            effects.log_message = Some(format!("OSC: head/calibrate {step:?} refused: {reason}"));
        }
    }
    Some(effects)
}

fn head_recenter(_msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    let mut effects = ControlEffects::default();
    // Capture the current raw tracker orientation as "forward" and snap the
    // rendered pose to identity so the scene faces straight ahead.
    let mut live = ctx.renderer.live.write();
    live.binaural.tracking.recenter();
    live.binaural.head_pose = renderer::binaural::HeadPose::identity();
    // Persist the new reference to config right away so the centering survives
    // an engine rebuild (mpv track change) and a restart.
    effects.persist.push(crate::persist::PersistOp::HEAD_CENTER);
    effects.publish_only = true;
    effects.log_message = Some("OSC: head/recenter".to_string());
    Some(effects)
}

fn render_backend_restore(
    _msg: &OscMessage,
    _ctx: &RuntimeControlContext,
) -> Option<ControlEffects> {
    Some(ControlEffects {
        log_message: Some(
            "OSC: render_backend/restore is no longer supported after removing from_file"
                .to_string(),
        ),
        ..Default::default()
    })
}

/// Generic backend parameter set. Two forms:
///   `[string key, <scalar value>]`              -> currently selected backend
///   `[string backend_id, string key, <value>]`  -> an explicit backend
/// The explicit form lets the UI address an inner backend (e.g. the hybrid
/// barycenter tab) even though it is not the active selection. Values are
/// stored generically (no typed field per backend) and read at the next
/// topology rebuild via the schema.
fn backend_param(msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    let mut effects = ControlEffects::default();
    let (target, key, value_arg) = if msg.args.len() >= 3 {
        (
            parse_string_arg(msg.args.first()),
            parse_string_arg(msg.args.get(1)),
            msg.args.get(2),
        )
    } else {
        (None, parse_string_arg(msg.args.first()), msg.args.get(1))
    };
    let value = value_arg.and_then(parse_param_value);
    if let (Some(key), Some(value)) = (key, value) {
        let (backend_id, active, hybrid_legs) = {
            let live = ctx.renderer.live.read();
            (
                target.unwrap_or_else(|| live.backend_id().to_string()),
                live.backend_id().to_string(),
                (
                    live.hybrid.external_backend_id.clone(),
                    live.hybrid.internal_backend_id.clone(),
                ),
            )
        };
        if !ctx.renderer.set_backend_param(&backend_id, &key, value) {
            effects.log_message = Some(format!(
                "OSC: backend param {backend_id}.{key} refused (not a value of its declared type)"
            ));
            return Some(effects);
        }
        effects.mark_dirty = true;
        // Recompute only when the edited backend participates in the
        // active topology (the selection itself, or a leg of an active
        // hybrid). Params of an inactive backend are stored and persisted;
        // they are read at the next rebuild that involves that backend.
        let participates = backend_id == active
            || (active == "hybrid" && (backend_id == hybrid_legs.0 || backend_id == hybrid_legs.1));
        effects.trigger_layout_recompute = participates;
        effects.log_message = Some(format!(
            "OSC: backend param {backend_id}.{key} updated{}",
            if participates {
                ""
            } else {
                " (inactive backend, no rebuild)"
            }
        ));
    }
    Some(effects)
}

fn render_evaluation_mode_from_file(
    _msg: &OscMessage,
    _ctx: &RuntimeControlContext,
) -> Option<ControlEffects> {
    Some(ControlEffects {
        log_message: Some(
            "OSC: render_evaluation_mode/from_file is no longer supported".to_string(),
        ),
        ..Default::default()
    })
}

fn layout_radius_m(msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    let mut effects = ControlEffects::default();
    if let Some(v) = parse_f32_arg(msg.args.first()).map(|f| f.max(0.01)) {
        ctx.renderer
            .with_editable_layout(|layout| layout.radius_m = v);
        effects.mark_dirty = true;
        effects.log_message = Some(format!("OSC: layout radius_m → {}", v));
    }
    Some(effects)
}

fn spread_from_distance(msg: &OscMessage, ctx: &RuntimeControlContext) -> Option<ControlEffects> {
    let mut effects = ControlEffects::default();
    if let Some(v) = parse_bool_arg(msg.args.first()) {
        ctx.renderer.set_backend_param(
            "vbap",
            "spread_from_distance",
            renderer::backend_params::ParamValue::Bool(v),
        );
        effects.mark_dirty = true;
        effects.trigger_layout_recompute = true;
    }
    Some(effects)
}

fn spread_size_to_spread_mode(
    msg: &OscMessage,
    ctx: &RuntimeControlContext,
) -> Option<ControlEffects> {
    let mut effects = ControlEffects::default();
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
    Some(effects)
}
