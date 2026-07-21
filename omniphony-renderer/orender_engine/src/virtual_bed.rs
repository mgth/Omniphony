//! Virtual-bed rendering for bed-only / pre-metadata frames.
//!
//! When a stream carries no spatial-object metadata (a plain multichannel bed,
//! or the frames before the first major-sync metadata payload), each input
//! channel is
//! turned into a fixed-position "virtual object" placed at its speaker pose, so
//! the bed still renders through VBAP instead of being dropped. Shared by the
//! `orender` CLI and the embedded engine for identical behaviour.

use crate::osc::ObjectMeta;
use bridge_api::RChannelLabel;
use renderer::live_params::SurroundPlacement;
use renderer::speaker_layout::SpeakerLayout;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

#[inline]
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

fn inverse_map_depth_with_room_ratios(
    mapped_depth: f32,
    front_ratio: f32,
    rear_ratio: f32,
    center_blend: f32,
) -> f32 {
    let y = mapped_depth;
    if y >= 0.0 {
        let target = y.clamp(0.0, front_ratio.max(0.0));
        let mut lo = 0.0f32;
        let mut hi = 1.0f32;
        for _ in 0..28 {
            let mid = (lo + hi) * 0.5;
            let val = map_depth_with_room_ratios(mid, front_ratio, rear_ratio, center_blend);
            if val < target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (lo + hi) * 0.5
    } else {
        let target = y.clamp(-rear_ratio.max(0.0), 0.0);
        let mut lo = -1.0f32;
        let mut hi = 0.0f32;
        for _ in 0..28 {
            let mid = (lo + hi) * 0.5;
            let val = map_depth_with_room_ratios(mid, front_ratio, rear_ratio, center_blend);
            if val < target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (lo + hi) * 0.5
    }
}

fn inverse_room_ratio_map_for_virtual_object(
    target_x: f32,
    target_y: f32,
    target_z: f32,
    room_ratio: [f32; 3],
    room_ratio_rear: f32,
    room_ratio_lower: f32,
    room_ratio_center_blend: f32,
) -> (f32, f32, f32) {
    let width = room_ratio[0].max(0.01);
    let front = room_ratio[1].max(0.01);
    let height = room_ratio[2].max(0.01);
    let rear = room_ratio_rear.max(0.01);
    let lower = room_ratio_lower.max(0.01);

    let x = (target_x / width).clamp(-1.0, 1.0);
    let y = inverse_map_depth_with_room_ratios(target_y, front, rear, room_ratio_center_blend)
        .clamp(-1.0, 1.0);
    let z = if target_z >= 0.0 {
        (target_z / height).clamp(-1.0, 1.0)
    } else {
        (target_z / lower).clamp(-1.0, 1.0)
    };
    (x, y, z)
}

#[derive(Clone)]
struct VirtualBedLayouts {
    layout_5_1: Option<SpeakerLayout>,
    layout_7_1: Option<SpeakerLayout>,
}

static VIRTUAL_BED_LAYOUTS: OnceLock<VirtualBedLayouts> = OnceLock::new();

fn virtual_bed_layouts() -> &'static VirtualBedLayouts {
    VIRTUAL_BED_LAYOUTS.get_or_init(|| VirtualBedLayouts {
        layout_5_1: load_virtual_bed_layout("5.1.yaml"),
        layout_7_1: load_virtual_bed_layout("7.1.yaml"),
    })
}

fn load_virtual_bed_layout(file_name: &str) -> Option<SpeakerLayout> {
    // The 5.1 / 7.1 virtual-bed layouts are height-less, so they now live in
    // the layouts/legacy/ subfolder. Try that first, then the historical
    // top-level path (older installs / packaging that still ships them flat).
    // `mut` is unused when the install-dir pushes below are compiled out.
    #[cfg_attr(test, allow(unused_mut))]
    let mut bases: Vec<PathBuf> = vec![
        // cwd-relative first — matches the CLI run from the workspace root.
        PathBuf::from("layouts"),
        PathBuf::from("omniphony").join("layouts"),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("layouts"),
    ];
    // Fixed install dirs for the embedded host (mpv has no workspace cwd);
    // reached only when the cwd-relative lookups miss, so CLI parity holds.
    // Compiled out of unit tests: a system package (e.g. the AUR install)
    // shipping layouts here made test outcomes depend on machine state — the
    // installed 7.1.yaml's non-spatialized LFE placeholder pose (z=-0.5)
    // tripped the virtual-bed pose asserts on hosts with orender installed
    // while CI's clean environment passed.
    #[cfg(not(test))]
    {
        bases.push(PathBuf::from("/usr/lib/orender/layouts"));
        bases.push(PathBuf::from("/usr/share/orender/layouts"));
    }
    // Windows: the embedded host (mpv) has no workspace cwd, and the shared
    // install lives under %ProgramData%\omniphony (machine-wide, same as the
    // config + service). Search its layouts dir so layouts ship/resolve there.
    #[cfg(all(windows, not(test)))]
    if let Ok(program_data) = std::env::var("ProgramData") {
        let mut p = PathBuf::from(program_data);
        p.push("omniphony");
        p.push("layouts");
        bases.push(p);
    }
    let mut candidates: Vec<PathBuf> = Vec::with_capacity(bases.len() * 2);
    for base in &bases {
        candidates.push(base.join("legacy").join(file_name));
        candidates.push(base.join(file_name));
    }
    candidates.dedup();

    for path in candidates {
        if !path.exists() {
            continue;
        }
        match SpeakerLayout::from_file(&path) {
            Ok(layout) => {
                log::info!("Loaded virtual bed layout from {}", path.display());
                return Some(layout);
            }
            Err(e) => {
                log::warn!(
                    "Failed to load virtual bed layout '{}' ({}): {}",
                    file_name,
                    path.display(),
                    e
                );
            }
        }
    }

    log::warn!(
        "Virtual bed layout '{}' not found on disk, using built-in fallback positions",
        file_name
    );
    None
}

fn find_speaker_in_layout<'a>(
    layout: &'a SpeakerLayout,
    aliases: &[&str],
) -> Option<&'a renderer::speaker_layout::Speaker> {
    layout.speakers.iter().find(|speaker| {
        aliases
            .iter()
            .any(|alias| speaker.name.eq_ignore_ascii_case(alias))
    })
}

/// Convert a resolved bed speaker to a normalized ADM position in [-1, 1],
/// honouring its `coord_mode` exactly like the output speakers do
/// ([`SpeakerLayout::spatializable_positions_for_room`]):
///   - **cartesian**: the stored normalized x/y/z *are* the position; the
///     renderer applies the room warp forward, so no conversion is needed here.
///   - **polar**: spherical → real ADM → inverse room warp → normalized.
///
/// This is what keeps cartesian bed channels from landing at a fraction of their
/// depth: a cartesian entry's polar `distance` is derived from a *normalized*
/// cartesian vector (a unit-cube magnitude, not scene units), so running it back
/// through `spherical_to_adm` + the inverse room warp double-counted the room
/// ratio. Using x/y/z directly matches how the output speakers are placed.
fn speaker_pose_to_normalized(
    speaker: &renderer::speaker_layout::Speaker,
    room_ratio: [f32; 3],
    room_ratio_rear: f32,
    room_ratio_lower: f32,
    room_ratio_center_blend: f32,
) -> (String, f32, f32, f32) {
    if speaker.coord_mode.eq_ignore_ascii_case("cartesian") {
        (
            speaker.name.clone(),
            speaker.x.clamp(-1.0, 1.0),
            speaker.y.clamp(-1.0, 1.0),
            speaker.z.clamp(-1.0, 1.0),
        )
    } else {
        let (sx, sy, sz) = renderer::spatial_vbap::spherical_to_adm(
            speaker.azimuth,
            speaker.elevation,
            speaker.distance,
        );
        let (x, y, z) = inverse_room_ratio_map_for_virtual_object(
            sx,
            sy,
            sz,
            room_ratio,
            room_ratio_rear,
            room_ratio_lower,
            room_ratio_center_blend,
        );
        (speaker.name.clone(), x, y, z)
    }
}

fn label_aliases(label: RChannelLabel, use_7_1: bool) -> Option<&'static [&'static str]> {
    match label {
        RChannelLabel::L => Some(&["FL", "L", "FrontLeft", "LeftFront"]),
        RChannelLabel::R => Some(&["FR", "R", "FrontRight", "RightFront"]),
        RChannelLabel::C => Some(&["C", "FC", "Center", "Centre"]),
        RChannelLabel::LFE | RChannelLabel::LFE2 => {
            Some(&["LFE", "LFE1", "Sub", "Subwoofer", "SW"])
        }
        RChannelLabel::Ls => {
            if use_7_1 {
                Some(&["SL", "Ls", "LeftSurround", "SurroundLeft"])
            } else {
                Some(&[
                    "SL",
                    "Ls",
                    "BL",
                    "Lb",
                    "LeftSurround",
                    "SurroundLeft",
                    "BackLeft",
                    "LeftBack",
                ])
            }
        }
        RChannelLabel::Rs => {
            if use_7_1 {
                Some(&["SR", "Rs", "RightSurround", "SurroundRight"])
            } else {
                Some(&[
                    "SR",
                    "Rs",
                    "BR",
                    "Rb",
                    "RightSurround",
                    "SurroundRight",
                    "BackRight",
                    "RightBack",
                ])
            }
        }
        RChannelLabel::Lb => Some(&[
            "BL", "Lb", "Lrs", "BackLeft", "LeftBack", "RearLeft", "LeftRear",
        ]),
        RChannelLabel::Rb => Some(&[
            "BR",
            "Rb",
            "Rrs",
            "BackRight",
            "RightBack",
            "RearRight",
            "RightRear",
        ]),
        RChannelLabel::Cb => Some(&["BC", "Cb", "BackCenter", "RearCenter"]),
        // Height layer. Aliases cover the common naming schemes (TFL/TBL,
        // Dolby Ltf/Ltr, ADM Tp* / U* upper-layer) so a configured 7.1.4 layout
        // resolves these to its named top speakers.
        RChannelLabel::Tfl => Some(&[
            "TFL",
            "Tfl",
            "Ltf",
            "TpFL",
            "TopFrontLeft",
            "UpperFrontLeft",
        ]),
        RChannelLabel::Tfr => Some(&[
            "TFR",
            "Tfr",
            "Rtf",
            "TpFR",
            "TopFrontRight",
            "UpperFrontRight",
        ]),
        RChannelLabel::Tbl => Some(&[
            "TBL",
            "Tbl",
            "Ltr",
            "TpBL",
            "TopBackLeft",
            "TopRearLeft",
            "UpperBackLeft",
        ]),
        RChannelLabel::Tbr => Some(&[
            "TBR",
            "Tbr",
            "Rtr",
            "TpBR",
            "TopBackRight",
            "TopRearRight",
            "UpperBackRight",
        ]),
        RChannelLabel::Tsl => Some(&["TSL", "Tsl", "TpSL", "TopSideLeft", "UpperSideLeft"]),
        RChannelLabel::Tsr => Some(&["TSR", "Tsr", "TpSR", "TopSideRight", "UpperSideRight"]),
        RChannelLabel::Tfc => Some(&["TFC", "Tfc", "TpFC", "TopFrontCenter"]),
        _ => None,
    }
}

/// Last-resort bed pose as a **normalized cartesian** corner position, used only
/// when neither the live `virtual_bed` config nor an on-disk layout resolves the
/// channel. These mirror the canonical `layouts/legacy/5.1.yaml`/`7.1.yaml` and
/// Studio's `CANONICAL_BED`, so a corner channel lands exactly in its corner after
/// the room warp — cartesian, not polar/distance, which used to pull the corners
/// inward. Floor row at `z = 0`, height row at the ceiling `z = 1`. `use_7_1` no
/// longer changes these (the corners are layout-independent); the surround pair is
/// finalised by [`surround_placement_override`] for 4.x/5.x sources.
fn fallback_virtual_bed_pose(
    label: RChannelLabel,
    _use_7_1: bool,
) -> Option<(String, f32, f32, f32)> {
    let (name, x, y, z) = match label {
        RChannelLabel::L => ("FL", -1.0, 1.0, 0.0),
        RChannelLabel::R => ("FR", 1.0, 1.0, 0.0),
        RChannelLabel::C => ("C", 0.0, 1.0, 0.0),
        RChannelLabel::LFE | RChannelLabel::LFE2 => ("LFE", 0.0, 1.0, 0.0),
        RChannelLabel::Ls => ("SL", -1.0, 0.0, 0.0),
        RChannelLabel::Rs => ("SR", 1.0, 0.0, 0.0),
        RChannelLabel::Lb => ("BL", -1.0, -1.0, 0.0),
        RChannelLabel::Rb => ("BR", 1.0, -1.0, 0.0),
        RChannelLabel::Cb => ("BC", 0.0, -1.0, 0.0),
        // Height layer at the ceiling (z = 1), mirroring the floor corners.
        RChannelLabel::Tfl => ("TFL", -1.0, 1.0, 1.0),
        RChannelLabel::Tfr => ("TFR", 1.0, 1.0, 1.0),
        RChannelLabel::Tbl => ("TBL", -1.0, -1.0, 1.0),
        RChannelLabel::Tbr => ("TBR", 1.0, -1.0, 1.0),
        RChannelLabel::Tsl => ("TSL", -1.0, 0.0, 1.0),
        RChannelLabel::Tsr => ("TSR", 1.0, 0.0, 1.0),
        RChannelLabel::Tfc => ("TFC", 0.0, 1.0, 1.0),
        _ => return None,
    };
    Some((name.to_string(), x, y, z))
}

/// For a 4.x/5.x source (no back channels) the surround pair (`Ls`/`Rs`) has no
/// canonical placement, so `surround_placement` decides it: `Side` → the side
/// corner `(∓1, 0, 0)`, `Back` → the back corner `(∓1, −1, 0)` (sign by L/R).
/// Returns the normalized override position for `Ls`/`Rs`, or `None` (no
/// override) for any other label or when the source already has back channels.
pub(crate) fn surround_placement_override(
    label: RChannelLabel,
    use_7_1: bool,
    placement: SurroundPlacement,
) -> Option<(f32, f32, f32)> {
    if use_7_1 {
        return None;
    }
    let sign = match label {
        RChannelLabel::Ls => -1.0,
        RChannelLabel::Rs => 1.0,
        _ => return None,
    };
    let y = match placement {
        SurroundPlacement::Side => 0.0,
        SurroundPlacement::Back => -1.0,
    };
    Some((sign, y, 0.0))
}

/// Label a *direct* (non-spatialized) channel routes to, honouring
/// `surround_placement`. `Back` sends `Ls`/`Rs` to the back speaker when the
/// output layout actually has one, otherwise it keeps the side label. An
/// `LFE2` without a matching speaker folds onto the `LFE` sub, as the legacy
/// bed scheme did. `Object`/`Unknown` have no direct route.
fn direct_route_label(
    label: RChannelLabel,
    use_7_1: bool,
    placement: SurroundPlacement,
    label_to_speaker: Option<&HashMap<RChannelLabel, usize>>,
) -> Option<RChannelLabel> {
    let has_speaker = |l: RChannelLabel| label_to_speaker.is_some_and(|map| map.contains_key(&l));
    match label {
        RChannelLabel::Object | RChannelLabel::Unknown => return None,
        RChannelLabel::LFE2 if !has_speaker(RChannelLabel::LFE2) => {
            return Some(RChannelLabel::LFE);
        }
        _ => {}
    }
    if use_7_1 || placement != SurroundPlacement::Back {
        return Some(label);
    }
    let back = match label {
        RChannelLabel::Ls => RChannelLabel::Lb,
        RChannelLabel::Rs => RChannelLabel::Rb,
        _ => return Some(label),
    };
    if has_speaker(back) {
        Some(back)
    } else {
        Some(label)
    }
}

/// Resolve a channel's bed pose as a **normalized ADM position** in [-1, 1],
/// then apply the [`surround_placement_override`] for a 4.x/5.x surround pair.
#[allow(clippy::too_many_arguments)]
fn resolve_virtual_bed_pose(
    label: RChannelLabel,
    use_7_1: bool,
    input_layout: Option<&SpeakerLayout>,
    room_ratio: [f32; 3],
    room_ratio_rear: f32,
    room_ratio_lower: f32,
    room_ratio_center_blend: f32,
    surround_placement: SurroundPlacement,
) -> Option<(String, f32, f32, f32)> {
    resolve_virtual_bed_pose_raw(
        label,
        use_7_1,
        input_layout,
        room_ratio,
        room_ratio_rear,
        room_ratio_lower,
        room_ratio_center_blend,
    )
    .map(|(name, x, y, z)| {
        match surround_placement_override(label, use_7_1, surround_placement) {
            Some((ox, oy, oz)) => (name, ox, oy, oz),
            None => (name, x, y, z),
        }
    })
}

/// Resolve a channel's bed pose as a **normalized ADM position** in [-1, 1],
/// trying the user's virtual bed, then the bundled 5.1/7.1 layout, then the
/// built-in fallback. Each source is converted per its `coord_mode`
/// ([`speaker_pose_to_normalized`]); the fallback is always polar.
#[allow(clippy::too_many_arguments)]
fn resolve_virtual_bed_pose_raw(
    label: RChannelLabel,
    use_7_1: bool,
    input_layout: Option<&SpeakerLayout>,
    room_ratio: [f32; 3],
    room_ratio_rear: f32,
    room_ratio_lower: f32,
    room_ratio_center_blend: f32,
) -> Option<(String, f32, f32, f32)> {
    if let (Some(layout), Some(aliases)) = (input_layout, label_aliases(label, use_7_1)) {
        if let Some(found) = find_speaker_in_layout(layout, aliases) {
            return Some(speaker_pose_to_normalized(
                found,
                room_ratio,
                room_ratio_rear,
                room_ratio_lower,
                room_ratio_center_blend,
            ));
        }
    }

    let layouts = virtual_bed_layouts();
    let layout_opt = if use_7_1 {
        layouts.layout_7_1.as_ref()
    } else {
        layouts.layout_5_1.as_ref()
    };

    if let (Some(layout), Some(aliases)) = (layout_opt, label_aliases(label, use_7_1)) {
        if let Some(found) = find_speaker_in_layout(layout, aliases) {
            return Some(speaker_pose_to_normalized(
                found,
                room_ratio,
                room_ratio_rear,
                room_ratio_lower,
                room_ratio_center_blend,
            ));
        }
    }

    // Cartesian corner fallback: use x/y/z directly (clamped), exactly like the
    // cartesian branch of `speaker_pose_to_normalized`. No `spherical_to_adm` +
    // inverse-room-warp round-trip, which previously pulled corner channels off
    // their corner (the FL/FR-not-in-the-corner bug under hosts with no layout).
    fallback_virtual_bed_pose(label, use_7_1).map(|(name, x, y, z)| {
        (
            name,
            x.clamp(-1.0, 1.0),
            y.clamp(-1.0, 1.0),
            z.clamp(-1.0, 1.0),
        )
    })
}

#[allow(clippy::too_many_arguments)]
pub fn build_virtual_bed_events(
    channel_labels: &[RChannelLabel],
    input_layout: Option<&SpeakerLayout>,
    room_ratio: [f32; 3],
    room_ratio_rear: f32,
    room_ratio_lower: f32,
    room_ratio_center_blend: f32,
    surround_placement: SurroundPlacement,
) -> Option<Vec<renderer::spatial_renderer::SpatialChannelEvent>> {
    let has_back = channel_labels
        .iter()
        .any(|l| matches!(l, RChannelLabel::Lb | RChannelLabel::Rb | RChannelLabel::Cb));
    let use_7_1 = has_back;

    let mut events: Vec<renderer::spatial_renderer::SpatialChannelEvent> =
        Vec::with_capacity(channel_labels.len());

    for (channel_idx, label) in channel_labels.iter().enumerate() {
        let (_name, x, y, z) = match resolve_virtual_bed_pose(
            *label,
            use_7_1,
            input_layout,
            room_ratio,
            room_ratio_rear,
            room_ratio_lower,
            room_ratio_center_blend,
            surround_placement,
        ) {
            Some(v) => v,
            None => continue,
        };
        events.push(renderer::spatial_renderer::SpatialChannelEvent {
            channel_idx,
            is_bed: false,
            gain_db: Some(0),
            ramp_length: Some(0),
            size: None,
            position: Some([x as f64, y as f64, z as f64]),
            sample_pos: Some(0),
        });
    }

    if events.is_empty() {
        None
    } else {
        Some(events)
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build_virtual_bed_objects(
    channel_labels: &[RChannelLabel],
    virtual_bed: Option<&SpeakerLayout>,
    output_layout: Option<&SpeakerLayout>,
    room_ratio: [f32; 3],
    room_ratio_rear: f32,
    room_ratio_lower: f32,
    room_ratio_center_blend: f32,
    surround_placement: SurroundPlacement,
) -> Option<Vec<ObjectMeta>> {
    let has_back = channel_labels
        .iter()
        .any(|l| matches!(l, RChannelLabel::Lb | RChannelLabel::Rb | RChannelLabel::Cb));
    let use_7_1 = has_back;

    // Used to anchor a direct channel onto its output speaker so Studio shows it
    // snapped to that speaker (its `directSpeakerIndex` decoration).
    let label_to_speaker = output_layout.map(|layout| layout.label_to_speaker_mapping());

    let mut objects: Vec<ObjectMeta> = Vec::with_capacity(channel_labels.len());
    for label in channel_labels {
        // Emit every channel so the editor/overlay can show them all: virtualized
        // channels carry a free position, direct channels (e.g. LFE) carry a
        // `direct_speaker_index` so Studio anchors them onto their speaker.
        let spatialize = channel_is_spatialized(virtual_bed, *label, use_7_1);
        let (name, x, y, z) = match resolve_virtual_bed_pose(
            *label,
            use_7_1,
            virtual_bed,
            room_ratio,
            room_ratio_rear,
            room_ratio_lower,
            room_ratio_center_blend,
            surround_placement,
        ) {
            Some(v) => v,
            None => continue,
        };
        let direct_speaker_index = if spatialize {
            None
        } else {
            direct_route_label(
                *label,
                use_7_1,
                surround_placement,
                label_to_speaker.as_ref(),
            )
            .and_then(|route_label| {
                label_to_speaker
                    .as_ref()
                    .and_then(|m| m.get(&route_label).map(|&spk| spk as u32))
            })
        };
        // Per-channel gain from the virtual bed (dB); 0 = unity when unset.
        let gain = find_virtual_bed_entry(virtual_bed, *label, use_7_1)
            .map(|entry| entry.gain_db)
            .unwrap_or(0);
        objects.push(ObjectMeta {
            name,
            x,
            y,
            z,
            coord_mode: "cartesian".to_string(),
            direct_speaker_index,
            gain,
            priority: 0.0,
            size: [0.0, 0.0, 0.0],
            fixed: true,
            label: bridge_api::labels::canonical_name(*label).to_string(),
        });
    }
    if objects.is_empty() {
        None
    } else {
        Some(objects)
    }
}

/// Default placement for a channel label when the virtual bed has no entry for
/// it (or no virtual bed is configured): every channel is virtualized except the
/// LFE, which cannot be VBAP-panned and routes direct to the sub.
fn default_channel_spatialize(label: RChannelLabel) -> bool {
    !matches!(label, RChannelLabel::LFE | RChannelLabel::LFE2)
}

/// Find the virtual-bed entry (a [`renderer::speaker_layout::Speaker`]) for a
/// channel label, matching by the same name aliases used to resolve poses.
fn find_virtual_bed_entry(
    layout: Option<&SpeakerLayout>,
    label: RChannelLabel,
    use_7_1: bool,
) -> Option<&renderer::speaker_layout::Speaker> {
    let layout = layout?;
    let aliases = label_aliases(label, use_7_1)?;
    layout.speakers.iter().find(|speaker| {
        aliases
            .iter()
            .any(|alias| speaker.name.eq_ignore_ascii_case(alias))
    })
}

/// Whether a channel should be virtualized (`true`) or routed direct to its
/// speaker (`false`): the virtual bed's per-entry `spatialize` flag, falling
/// back to [`default_channel_spatialize`] when the bed has no entry for it.
fn channel_is_spatialized(
    layout: Option<&SpeakerLayout>,
    label: RChannelLabel,
    use_7_1: bool,
) -> bool {
    find_virtual_bed_entry(layout, label, use_7_1)
        .map(|entry| entry.spatialize)
        .unwrap_or_else(|| default_channel_spatialize(label))
}

/// What the renderer should do with a channel-based (non-object) frame, decided
/// once and applied identically by the CLI/spdif decode path and the embedded
/// mpv host. See [`renderer::live_params::ChannelRenderMode`].
pub enum ChannelRenderPlan {
    /// Let the host / sink handle the channels (no spatialization). The CLI
    /// writes the decoded channels straight out; the embedded mpv decoder
    /// declines so mpv falls back to its native decoder.
    HostPassthrough,
    /// Render the events through the virtual bed. `routes` has one entry per
    /// input channel: `Direct(label)` for a channel routed one-hot to the
    /// speaker its label resolves to, `Virtual` for a channel rendered as a
    /// VBAP object (its position is carried by the matching event). The
    /// renderer must `configure_channel_routing` to it.
    Events {
        events: Vec<renderer::spatial_renderer::SpatialChannelEvent>,
        routes: Vec<renderer::spatial_renderer::ChannelRoute>,
    },
    /// No renderable mapping for these labels → emit silence (advance the host
    /// by the frame's sample count without producing sound).
    Silence,
}

/// Decide how to render a channel-based frame for the given `mode`. Pure: no
/// renderer interaction, so both decode paths can call it and apply the result
/// the same way. In `Spatial` mode the placement of each channel — direct to a
/// speaker or virtualized at a position — is decided per channel by the
/// `virtual_bed` layout (falling back to canonical poses).
#[allow(clippy::too_many_arguments)]
pub fn plan_channel_render(
    mode: renderer::live_params::ChannelRenderMode,
    channel_labels: &[RChannelLabel],
    virtual_bed: Option<&SpeakerLayout>,
    output_layout: Option<&SpeakerLayout>,
    room_ratio: [f32; 3],
    room_ratio_rear: f32,
    room_ratio_lower: f32,
    room_ratio_center_blend: f32,
    surround_placement: SurroundPlacement,
) -> ChannelRenderPlan {
    use renderer::live_params::ChannelRenderMode;
    match mode {
        ChannelRenderMode::Host => ChannelRenderPlan::HostPassthrough,
        ChannelRenderMode::Spatial => build_virtual_bed_plan(
            channel_labels,
            virtual_bed,
            output_layout,
            room_ratio,
            room_ratio_rear,
            room_ratio_lower,
            room_ratio_center_blend,
            surround_placement,
        ),
    }
}

/// Spatial mode: decide each channel's placement against the virtual bed. A
/// channel marked `spatialize:false` (e.g. LFE) routes direct to its speaker
/// (a bed id in `bed_indices` + a bed event); a `spatialize:true` channel is
/// virtualized at the bed's position (the `usize::MAX` sentinel in `bed_indices`
/// + an object event carrying the position). A frame may freely mix the two.
#[allow(clippy::too_many_arguments)]
fn build_virtual_bed_plan(
    channel_labels: &[RChannelLabel],
    virtual_bed: Option<&SpeakerLayout>,
    output_layout: Option<&SpeakerLayout>,
    room_ratio: [f32; 3],
    room_ratio_rear: f32,
    room_ratio_lower: f32,
    room_ratio_center_blend: f32,
    surround_placement: SurroundPlacement,
) -> ChannelRenderPlan {
    let has_back = channel_labels
        .iter()
        .any(|l| matches!(l, RChannelLabel::Lb | RChannelLabel::Rb | RChannelLabel::Cb));
    let use_7_1 = has_back;

    // Label → output-speaker map, so a direct surround can be rerouted to a
    // back speaker (Back placement) only when the layout actually has one.
    let label_to_speaker = output_layout.map(|layout| layout.label_to_speaker_mapping());

    let mut routes: Vec<renderer::spatial_renderer::ChannelRoute> =
        Vec::with_capacity(channel_labels.len());
    let mut events: Vec<renderer::spatial_renderer::SpatialChannelEvent> =
        Vec::with_capacity(channel_labels.len());

    for (channel_idx, label) in channel_labels.iter().enumerate() {
        let spatialize = channel_is_spatialized(virtual_bed, *label, use_7_1);
        if spatialize {
            // Virtualize: place an object at the bed's (or fallback) pose.
            match resolve_virtual_bed_pose(
                *label,
                use_7_1,
                virtual_bed,
                room_ratio,
                room_ratio_rear,
                room_ratio_lower,
                room_ratio_center_blend,
                surround_placement,
            ) {
                Some((_name, x, y, z)) => {
                    routes.push(renderer::spatial_renderer::ChannelRoute::Virtual);
                    events.push(renderer::spatial_renderer::SpatialChannelEvent {
                        channel_idx,
                        is_bed: false,
                        gain_db: Some(0),
                        ramp_length: Some(0),
                        size: None,
                        position: Some([x as f64, y as f64, z as f64]),
                        sample_pos: Some(0),
                    });
                }
                // No resolvable pose: keep index alignment, route nowhere.
                None => routes.push(renderer::spatial_renderer::ChannelRoute::Virtual),
            }
        } else {
            // Direct: route to the matching output speaker (by label),
            // honouring Back placement for a 4.x/5.x surround when a back
            // speaker exists.
            match direct_route_label(
                *label,
                use_7_1,
                surround_placement,
                label_to_speaker.as_ref(),
            ) {
                Some(route_label) => {
                    routes.push(renderer::spatial_renderer::ChannelRoute::Direct(
                        route_label,
                    ));
                    events.push(renderer::spatial_renderer::SpatialChannelEvent {
                        channel_idx,
                        is_bed: true,
                        gain_db: Some(0),
                        ramp_length: Some(0),
                        size: None,
                        position: None,
                        sample_pos: Some(0),
                    });
                }
                // No direct slot for a channel asked to route direct: silent.
                None => routes.push(renderer::spatial_renderer::ChannelRoute::Virtual),
            }
        }
    }

    if events.is_empty() {
        ChannelRenderPlan::Silence
    } else {
        ChannelRenderPlan::Events { events, routes }
    }
}

/// Build the OSC display objects for an object stream's fixed prefix, using
/// the renderer's live placement options so the displayed poses match the
/// applied channel plan. `None` when the prefix is empty or unresolvable.
pub fn build_fixed_channel_objects(
    renderer: &renderer::spatial_renderer::SpatialRenderer,
    fixed_labels: &[RChannelLabel],
) -> Option<Vec<ObjectMeta>> {
    if fixed_labels.is_empty() {
        return None;
    }
    let control = renderer.renderer_control();
    let (
        virtual_bed_layout,
        surround_placement,
        room_ratio,
        room_ratio_rear,
        room_ratio_lower,
        room_ratio_center_blend,
    ) = {
        let live = control.live.read();
        (
            live.virtual_bed.clone(),
            live.surround_placement,
            live.room_ratio,
            live.room_ratio_rear,
            live.room_ratio_lower,
            live.room_ratio_center_blend,
        )
    };
    let layout = renderer.speaker_layout();
    build_virtual_bed_objects(
        fixed_labels,
        virtual_bed_layout.as_ref(),
        Some(&layout),
        room_ratio,
        room_ratio_rear,
        room_ratio_lower,
        room_ratio_center_blend,
        surround_placement,
    )
}

/// Shared fixed-channel planner for object streams (engine + CLI decode
/// paths). Plans the fixed prefix of the channel list through
/// [`plan_channel_render`] — virtualized by default, per-entry direct opt-in
/// via the placement layout — caches the result on `(labels, options_epoch)`,
/// and applies the routing to the renderer only on change.
#[derive(Default)]
pub struct FixedChannelPlanner {
    planned_labels: Vec<RChannelLabel>,
    planned_epoch: Option<u64>,
    applied_routes: Option<Vec<renderer::spatial_renderer::ChannelRoute>>,
}

impl FixedChannelPlanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget everything (stream reset / new segment).
    pub fn reset(&mut self) {
        self.planned_labels.clear();
        self.planned_epoch = None;
        self.applied_routes = None;
    }

    /// Fixed labels of the last planned prefix.
    pub fn fixed_labels(&self) -> &[RChannelLabel] {
        &self.planned_labels
    }

    /// Apply a route set computed elsewhere (the fixed-only render path),
    /// deduplicated against the last applied set.
    pub fn apply_routes(
        &mut self,
        renderer: &renderer::spatial_renderer::SpatialRenderer,
        routes: Vec<renderer::spatial_renderer::ChannelRoute>,
    ) {
        if self.applied_routes.as_deref() != Some(routes.as_slice()) {
            renderer.configure_channel_routing(&routes);
            self.applied_routes = Some(routes);
        }
    }

    /// Plan the fixed prefix (labels before the first `Object` channel) of an
    /// object stream and apply it. Mode is always spatial here: an object
    /// stream cannot pass through the host, so the live channel mode only
    /// applies to fixed-only streams. On replan the fixed channels' pose/gain
    /// events are appended to `out` (the renderer caches per-channel state,
    /// so they are only needed when the plan changes).
    #[allow(clippy::too_many_arguments)]
    pub fn plan_object_stream_fixed(
        &mut self,
        channel_labels: &[RChannelLabel],
        renderer: &renderer::spatial_renderer::SpatialRenderer,
        out: &mut Vec<renderer::spatial_renderer::SpatialChannelEvent>,
    ) {
        let fixed_end = channel_labels
            .iter()
            .position(|l| *l == RChannelLabel::Object)
            .unwrap_or(channel_labels.len());
        let fixed = &channel_labels[..fixed_end];

        let control = renderer.renderer_control();
        let epoch = control.options_epoch();
        if self.planned_epoch == Some(epoch) && self.planned_labels == fixed {
            return;
        }

        let (
            virtual_bed_layout,
            surround_placement,
            room_ratio,
            room_ratio_rear,
            room_ratio_lower,
            room_ratio_center_blend,
        ) = {
            let live = control.live.read();
            (
                live.virtual_bed.clone(),
                live.surround_placement,
                live.room_ratio,
                live.room_ratio_rear,
                live.room_ratio_lower,
                live.room_ratio_center_blend,
            )
        };
        let output_layout = renderer.speaker_layout();

        match plan_channel_render(
            renderer::live_params::ChannelRenderMode::Spatial,
            fixed,
            virtual_bed_layout.as_ref(),
            Some(&output_layout),
            room_ratio,
            room_ratio_rear,
            room_ratio_lower,
            room_ratio_center_blend,
            surround_placement,
        ) {
            ChannelRenderPlan::Events { events, routes } => {
                self.apply_routes(renderer, routes);
                out.extend(events);
            }
            ChannelRenderPlan::HostPassthrough | ChannelRenderPlan::Silence => {
                self.apply_routes(renderer, Vec::new());
            }
        }

        self.planned_labels = fixed.to_vec();
        self.planned_epoch = Some(epoch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const UNIT_ROOM: [f32; 3] = [1.0, 1.0, 1.0];

    #[test]
    fn maps_a_5_1_bed_with_fallback_poses() {
        // No input layout → resolves via bundled layouts or built-in fallbacks.
        let labels = [
            RChannelLabel::L,
            RChannelLabel::R,
            RChannelLabel::C,
            RChannelLabel::LFE,
            RChannelLabel::Ls,
            RChannelLabel::Rs,
        ];
        let events = build_virtual_bed_events(
            &labels,
            None,
            UNIT_ROOM,
            1.0,
            1.0,
            0.0,
            SurroundPlacement::Side,
        )
        .expect("5.1 bed must map to virtual events");
        assert_eq!(events.len(), labels.len());
        for (i, ev) in events.iter().enumerate() {
            assert_eq!(ev.channel_idx, i);
            assert!(!ev.is_bed);
            let pos = ev.position.expect("virtual event carries a position");
            assert!(
                pos.iter()
                    .all(|c| c.is_finite() && (-1.0..=1.0).contains(c)),
                "position {pos:?} must be finite and within the unit room"
            );
        }
    }

    #[test]
    fn maps_a_7_1_4_bed_including_height_channels() {
        // A full 7.1.4 input bed (e.g. a bed-only Atmos presentation). The height
        // layer must resolve to elevated poses rather than being dropped (which
        // used to leave the top channels silent).
        let labels = [
            RChannelLabel::L,
            RChannelLabel::R,
            RChannelLabel::C,
            RChannelLabel::LFE,
            RChannelLabel::Ls,
            RChannelLabel::Rs,
            RChannelLabel::Lb,
            RChannelLabel::Rb,
            RChannelLabel::Tfl,
            RChannelLabel::Tfr,
            RChannelLabel::Tbl,
            RChannelLabel::Tbr,
        ];
        let events = build_virtual_bed_events(
            &labels,
            None,
            UNIT_ROOM,
            1.0,
            1.0,
            0.0,
            SurroundPlacement::Side,
        )
        .expect("7.1.4 bed must map to virtual events");
        // Every channel resolves a pose — none are dropped.
        assert_eq!(events.len(), labels.len());
        // The four height channels (idx 8..12) sit above ear level (z > 0).
        for ev in &events[8..12] {
            let pos = ev.position.expect("height event carries a position");
            assert!(pos[2] > 0.1, "height channel must be elevated, got {pos:?}");
        }
        // The floor channels stay near ear level.
        for ev in &events[0..8] {
            let pos = ev.position.expect("floor event carries a position");
            assert!(
                pos[2].abs() < 0.2,
                "floor channel should be ~level, got {pos:?}"
            );
        }
    }

    #[test]
    fn left_and_right_beds_are_mirrored() {
        let labels = [RChannelLabel::L, RChannelLabel::R];
        let events = build_virtual_bed_events(
            &labels,
            None,
            UNIT_ROOM,
            1.0,
            1.0,
            0.0,
            SurroundPlacement::Side,
        )
        .unwrap();
        let l = events[0].position.unwrap();
        let r = events[1].position.unwrap();
        // L sits on the negative-x side, R on the positive-x side.
        assert!(l[0] < 0.0, "L x={} should be negative", l[0]);
        assert!(r[0] > 0.0, "R x={} should be positive", r[0]);
    }

    #[test]
    fn objects_match_events_for_the_same_bed() {
        let labels = [RChannelLabel::L, RChannelLabel::R, RChannelLabel::C];
        let events = build_virtual_bed_events(
            &labels,
            None,
            UNIT_ROOM,
            1.0,
            1.0,
            0.0,
            SurroundPlacement::Side,
        )
        .unwrap();
        let objects = build_virtual_bed_objects(
            &labels,
            None,
            None,
            UNIT_ROOM,
            1.0,
            1.0,
            0.0,
            SurroundPlacement::Side,
        )
        .unwrap();
        assert_eq!(events.len(), objects.len());
        for (ev, obj) in events.iter().zip(objects.iter()) {
            let pos = ev.position.unwrap();
            assert!((pos[0] - obj.x as f64).abs() < 1e-6);
            assert!((pos[1] - obj.y as f64).abs() < 1e-6);
            assert!((pos[2] - obj.z as f64).abs() < 1e-6);
        }
    }

    #[test]
    fn fallback_pose_is_cartesian_corners() {
        // The last-resort fallback (no live bed, no on-disk layout) must place the
        // bed channels at the exact cartesian corners, not a polar/distance
        // approximation that gets pulled inward by the room warp — so a host with
        // no layout (e.g. mpv with a non-workspace cwd) still shows FL/FR in the
        // corners and matches the editor.
        for (label, expect) in [
            (RChannelLabel::L, (-1.0_f32, 1.0_f32, 0.0_f32)),
            (RChannelLabel::R, (1.0, 1.0, 0.0)),
            (RChannelLabel::C, (0.0, 1.0, 0.0)),
            (RChannelLabel::Ls, (-1.0, 0.0, 0.0)),
            (RChannelLabel::Rs, (1.0, 0.0, 0.0)),
            (RChannelLabel::Lb, (-1.0, -1.0, 0.0)),
            (RChannelLabel::Rb, (1.0, -1.0, 0.0)),
        ] {
            // use_7_1 must not change the corner.
            for use_7_1 in [false, true] {
                let (_n, x, y, z) = fallback_virtual_bed_pose(label, use_7_1)
                    .unwrap_or_else(|| panic!("fallback pose for {label:?}"));
                assert_eq!((x, y, z), expect, "{label:?} (use_7_1={use_7_1})");
            }
        }
    }

    #[test]
    fn objects_anchor_direct_channels_to_their_speaker() {
        // Default 5.1: LFE is direct, the rest virtualized. With an output
        // layout, the LFE object must carry its speaker's index so Studio shows
        // it snapped there; the virtualized channels carry no direct index.
        let labels = [
            RChannelLabel::L,
            RChannelLabel::R,
            RChannelLabel::C,
            RChannelLabel::LFE,
            RChannelLabel::Ls,
            RChannelLabel::Rs,
        ];
        let output = SpeakerLayout::preset("5.1").expect("5.1 preset");
        // LFE is speaker index 3 in the 5.1 preset (FL,FR,C,LFE,BL,BR).
        let objects = build_virtual_bed_objects(
            &labels,
            None,
            Some(&output),
            UNIT_ROOM,
            1.0,
            1.0,
            0.0,
            SurroundPlacement::Side,
        )
        .expect("all channels emitted");
        assert_eq!(objects.len(), labels.len(), "every channel is shown");
        let lfe = objects
            .iter()
            .find(|o| o.name.eq_ignore_ascii_case("LFE"))
            .expect("LFE object present");
        assert_eq!(lfe.direct_speaker_index, Some(3), "LFE anchored to its sub");
        for obj in objects
            .iter()
            .filter(|o| !o.name.eq_ignore_ascii_case("LFE"))
        {
            assert!(
                obj.direct_speaker_index.is_none(),
                "{} is virtualized, no direct anchor",
                obj.name
            );
        }
    }

    #[test]
    fn objects_carry_per_channel_gain_from_virtual_bed() {
        use renderer::speaker_layout::Speaker;
        // A virtual bed sets C to -6 dB; channels with no explicit gain stay at 0.
        let mut center = Speaker::new("C", 0.0, 0.0);
        center.gain_db = -6;
        let bed = vbed(vec![
            Speaker::new("L", -30.0, 0.0),
            center,
            Speaker::new("R", 30.0, 0.0),
        ]);
        let labels = [RChannelLabel::L, RChannelLabel::C, RChannelLabel::LFE];
        let objects = build_virtual_bed_objects(
            &labels,
            Some(&bed),
            None,
            UNIT_ROOM,
            1.0,
            1.0,
            0.0,
            SurroundPlacement::Side,
        )
        .expect("all channels emitted");
        let c = objects
            .iter()
            .find(|o| o.name.eq_ignore_ascii_case("C"))
            .expect("C object present");
        assert_eq!(c.gain, -6, "C gain comes from the virtual bed");
        for obj in objects.iter().filter(|o| !o.name.eq_ignore_ascii_case("C")) {
            assert_eq!(obj.gain, 0, "{} has no configured gain", obj.name);
        }
    }

    #[test]
    fn cartesian_bed_channel_keeps_its_normalized_depth() {
        use renderer::speaker_layout::Speaker;
        // A cartesian bed entry stores normalized x/y/z directly, like the output
        // speakers. Its object position must be those coords verbatim, independent
        // of the room ratio — not run back through the polar pipeline + inverse
        // warp (the old path), which derived a normalized magnitude as a
        // scene-unit distance and so halved a front-placed channel's depth.
        let bed = vbed(vec![
            Speaker::from_cartesian("L", -1.0, 1.0, 0.0, true, 0.0),
            Speaker::from_cartesian("C", 0.0, 1.0, 0.0, true, 0.0),
            Speaker::from_cartesian("R", 1.0, 1.0, 0.0, true, 0.0),
        ]);
        let labels = [RChannelLabel::L, RChannelLabel::C, RChannelLabel::R];
        // Non-unit front ratio: the buggy path collapsed y=1.0 to ~0.5 here.
        let room = [1.0, 2.0, 1.0];
        let objects = build_virtual_bed_objects(
            &labels,
            Some(&bed),
            None,
            room,
            1.0,
            1.0,
            0.5,
            SurroundPlacement::Side,
        )
        .expect("objects emitted");
        let c = objects
            .iter()
            .find(|o| o.name.eq_ignore_ascii_case("C"))
            .expect("C object present");
        assert_eq!(c.coord_mode, "cartesian");
        assert!(c.x.abs() < 1e-6, "x={}", c.x);
        assert!(
            (c.y - 1.0).abs() < 1e-6,
            "cartesian y must stay 1.0, got {}",
            c.y
        );
        assert!(c.z.abs() < 1e-6, "z={}", c.z);
        // The plan path (audio rendering) must agree with the object path (Studio).
        let plan = plan_channel_render(
            renderer::live_params::ChannelRenderMode::Spatial,
            &labels,
            Some(&bed),
            None,
            room,
            1.0,
            1.0,
            0.5,
            SurroundPlacement::Side,
        );
        match plan {
            ChannelRenderPlan::Events { events, .. } => {
                let c_event = events
                    .iter()
                    .find(|e| e.channel_idx == 1)
                    .expect("C virtualized");
                let pos = c_event.position.expect("C carries a position");
                assert!((pos[1] - 1.0).abs() < 1e-6, "plan y must match object y");
            }
            other => panic!("expected Events, got {:?}", PlanKind::from(&other)),
        }
    }

    #[test]
    fn surround_placement_moves_5_1_surrounds_side_vs_back() {
        // 5.1 (no back channels): Side puts Ls/Rs at the side corner (y=0), Back
        // at the back corner (y=-1). Objects are emitted in label order, so Ls is
        // index 4 and Rs index 5. Front/centre channels are untouched.
        let labels = [
            RChannelLabel::L,
            RChannelLabel::R,
            RChannelLabel::C,
            RChannelLabel::LFE,
            RChannelLabel::Ls,
            RChannelLabel::Rs,
        ];
        let side = build_virtual_bed_objects(
            &labels,
            None,
            None,
            UNIT_ROOM,
            1.0,
            1.0,
            0.0,
            SurroundPlacement::Side,
        )
        .unwrap();
        let back = build_virtual_bed_objects(
            &labels,
            None,
            None,
            UNIT_ROOM,
            1.0,
            1.0,
            0.0,
            SurroundPlacement::Back,
        )
        .unwrap();
        assert!(
            (side[4].x + 1.0).abs() < 1e-6 && side[4].y.abs() < 1e-6,
            "Ls side = (-1,0), got ({},{})",
            side[4].x,
            side[4].y
        );
        assert!(
            (side[5].x - 1.0).abs() < 1e-6 && side[5].y.abs() < 1e-6,
            "Rs side = (1,0)"
        );
        assert!(
            (back[4].x + 1.0).abs() < 1e-6 && (back[4].y + 1.0).abs() < 1e-6,
            "Ls back = (-1,-1), got ({},{})",
            back[4].x,
            back[4].y
        );
        assert!(
            (back[5].x - 1.0).abs() < 1e-6 && (back[5].y + 1.0).abs() < 1e-6,
            "Rs back = (1,-1)"
        );
        // The centre channel is unaffected by the surround placement.
        assert!((side[2].x - back[2].x).abs() < 1e-6 && (side[2].y - back[2].y).abs() < 1e-6);
    }

    #[test]
    fn surround_placement_ignored_for_7_x() {
        // 7.1 carries Lb/Rb, so Ls/Rs are unambiguous side surrounds: the setting
        // must not move any channel.
        let labels = [
            RChannelLabel::L,
            RChannelLabel::R,
            RChannelLabel::C,
            RChannelLabel::LFE,
            RChannelLabel::Ls,
            RChannelLabel::Rs,
            RChannelLabel::Lb,
            RChannelLabel::Rb,
        ];
        let side = build_virtual_bed_objects(
            &labels,
            None,
            None,
            UNIT_ROOM,
            1.0,
            1.0,
            0.0,
            SurroundPlacement::Side,
        )
        .unwrap();
        let back = build_virtual_bed_objects(
            &labels,
            None,
            None,
            UNIT_ROOM,
            1.0,
            1.0,
            0.0,
            SurroundPlacement::Back,
        )
        .unwrap();
        for (s, b) in side.iter().zip(back.iter()) {
            assert!(
                (s.x - b.x).abs() < 1e-6 && (s.y - b.y).abs() < 1e-6 && (s.z - b.z).abs() < 1e-6,
                "7.x channel {} must ignore surround placement",
                s.name
            );
        }
    }

    #[test]
    fn direct_surround_routes_to_back_speaker_only_when_present() {
        // Back placement sends a direct (non-spatialized) surround to the back
        // bed (6/7) only when the output layout has that speaker; else the side
        // bed (4/5). Side never remaps, and 7.x is unaffected.
        let mut with_back: HashMap<RChannelLabel, usize> = HashMap::new();
        with_back.insert(RChannelLabel::Lb, 10);
        with_back.insert(RChannelLabel::Rb, 11);
        let without_back: HashMap<RChannelLabel, usize> = HashMap::new();

        assert_eq!(
            direct_route_label(
                RChannelLabel::Ls,
                false,
                SurroundPlacement::Back,
                Some(&with_back)
            ),
            Some(RChannelLabel::Lb)
        );
        assert_eq!(
            direct_route_label(
                RChannelLabel::Rs,
                false,
                SurroundPlacement::Back,
                Some(&with_back)
            ),
            Some(RChannelLabel::Rb)
        );
        assert_eq!(
            direct_route_label(
                RChannelLabel::Ls,
                false,
                SurroundPlacement::Back,
                Some(&without_back)
            ),
            Some(RChannelLabel::Ls),
            "no back speaker → side label"
        );
        assert_eq!(
            direct_route_label(
                RChannelLabel::Ls,
                false,
                SurroundPlacement::Side,
                Some(&with_back)
            ),
            Some(RChannelLabel::Ls),
            "Side never remaps"
        );
        assert_eq!(
            direct_route_label(
                RChannelLabel::Ls,
                true,
                SurroundPlacement::Back,
                Some(&with_back)
            ),
            Some(RChannelLabel::Ls),
            "7.x ignores the setting"
        );
        // LFE and front channels are never remapped.
        assert_eq!(
            direct_route_label(
                RChannelLabel::LFE,
                false,
                SurroundPlacement::Back,
                Some(&with_back)
            ),
            Some(RChannelLabel::LFE)
        );
    }

    const BED_5_1: [RChannelLabel; 6] = [
        RChannelLabel::L,
        RChannelLabel::R,
        RChannelLabel::C,
        RChannelLabel::LFE,
        RChannelLabel::Ls,
        RChannelLabel::Rs,
    ];

    #[test]
    fn plan_host_is_passthrough() {
        let plan = plan_channel_render(
            renderer::live_params::ChannelRenderMode::Host,
            &BED_5_1,
            None,
            None,
            UNIT_ROOM,
            1.0,
            1.0,
            0.0,
            SurroundPlacement::Side,
        );
        assert!(matches!(plan, ChannelRenderPlan::HostPassthrough));
    }

    fn vbed(speakers: Vec<renderer::speaker_layout::Speaker>) -> SpeakerLayout {
        SpeakerLayout::from_speakers(speakers).expect("valid virtual bed")
    }

    #[test]
    fn plan_spatial_default_virtualizes_all_but_lfe() {
        // No virtual bed configured → built-in defaults: every channel is a VBAP
        // object except LFE, which routes direct to its sub (bed id 3).
        let plan = plan_channel_render(
            renderer::live_params::ChannelRenderMode::Spatial,
            &BED_5_1,
            None,
            None,
            UNIT_ROOM,
            1.0,
            1.0,
            0.0,
            SurroundPlacement::Side,
        );
        match plan {
            ChannelRenderPlan::Events { events, routes } => {
                use renderer::spatial_renderer::ChannelRoute;
                // One entry per channel: LFE (idx 3) is direct, the rest virtual.
                assert_eq!(
                    routes,
                    vec![
                        ChannelRoute::Virtual,
                        ChannelRoute::Virtual,
                        ChannelRoute::Virtual,
                        ChannelRoute::Direct(RChannelLabel::LFE),
                        ChannelRoute::Virtual,
                        ChannelRoute::Virtual,
                    ]
                );
                // Six events: five virtual objects + the LFE bed.
                assert_eq!(events.len(), BED_5_1.len());
                let lfe = events.iter().find(|e| e.channel_idx == 3).unwrap();
                assert!(lfe.is_bed, "LFE must be a bed event");
                assert!(lfe.position.is_none());
                for ev in events.iter().filter(|e| e.channel_idx != 3) {
                    assert!(!ev.is_bed, "non-LFE channels are virtual objects");
                    assert!(ev.position.is_some());
                }
            }
            other => panic!("expected Events, got {:?}", PlanKind::from(&other)),
        }
    }

    #[test]
    fn plan_spatial_respects_explicit_per_channel_spatialize() {
        use renderer::speaker_layout::Speaker;
        // Flip the defaults: C routed direct, LFE virtualized. Other channels
        // keep their defaults (virtual).
        let bed = vbed(vec![
            Speaker::new_with_spatialize("C", 0.0, 0.0, false),
            Speaker::new_with_spatialize("LFE", 0.0, 0.0, true),
            Speaker::new_with_spatialize("FL", -30.0, 0.0, true),
        ]);
        let plan = plan_channel_render(
            renderer::live_params::ChannelRenderMode::Spatial,
            &BED_5_1,
            Some(&bed),
            None,
            UNIT_ROOM,
            1.0,
            1.0,
            0.0,
            SurroundPlacement::Side,
        );
        match plan {
            ChannelRenderPlan::Events { routes, .. } => {
                use renderer::spatial_renderer::ChannelRoute;
                // C (idx 2) is now direct; LFE (idx 3) is virtual.
                assert_eq!(
                    routes[2],
                    ChannelRoute::Direct(RChannelLabel::C),
                    "C explicitly direct"
                );
                assert_eq!(
                    routes[3],
                    ChannelRoute::Virtual,
                    "LFE explicitly virtualized"
                );
                assert_eq!(
                    routes[0],
                    ChannelRoute::Virtual,
                    "L keeps the virtual default"
                );
            }
            other => panic!("expected Events, got {:?}", PlanKind::from(&other)),
        }
    }

    #[test]
    fn plan_spatial_routes_direct_channels_by_label() {
        use renderer::speaker_layout::Speaker;
        // A direct back-centre routes by label: with the label language there
        // is no "no slot in the scheme" case anymore — the route carries Cb
        // and the renderer resolves (or silently skips) it against the active
        // layout. Index alignment is preserved either way.
        let bed = vbed(vec![
            Speaker::new_with_spatialize("BC", 180.0, 0.0, false),
            Speaker::new_with_spatialize("FL", -30.0, 0.0, true),
            Speaker::new_with_spatialize("FR", 30.0, 0.0, true),
        ]);
        let labels = [RChannelLabel::L, RChannelLabel::Cb, RChannelLabel::R];
        let plan = plan_channel_render(
            renderer::live_params::ChannelRenderMode::Spatial,
            &labels,
            Some(&bed),
            None,
            UNIT_ROOM,
            1.0,
            1.0,
            0.0,
            SurroundPlacement::Side,
        );
        match plan {
            ChannelRenderPlan::Events { events, routes } => {
                use renderer::spatial_renderer::ChannelRoute;
                assert_eq!(routes.len(), 3);
                assert_eq!(
                    routes[1],
                    ChannelRoute::Direct(RChannelLabel::Cb),
                    "Cb routes direct by label; the layout decides at render time"
                );
                // L and R virtualize (objects); Cb carries a direct (bed) event.
                assert_eq!(events.len(), 3);
                let cb = events.iter().find(|e| e.channel_idx == 1).unwrap();
                assert!(cb.is_bed);
            }
            other => panic!("expected Events, got {:?}", PlanKind::from(&other)),
        }
    }

    // Small helper so panics in the matches above print something readable.
    #[derive(Debug)]
    enum PlanKind {
        Host,
        Events,
        Silence,
    }
    impl From<&ChannelRenderPlan> for PlanKind {
        fn from(p: &ChannelRenderPlan) -> Self {
            match p {
                ChannelRenderPlan::HostPassthrough => PlanKind::Host,
                ChannelRenderPlan::Events { .. } => PlanKind::Events,
                ChannelRenderPlan::Silence => PlanKind::Silence,
            }
        }
    }
}
