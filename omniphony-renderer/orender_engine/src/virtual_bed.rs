//! Fixed-channel placement: where each labelled channel of a stream goes.
//!
//! Every fixed channel — a plain multichannel bed, the bed of an object
//! stream, the frames before the first metadata payload — is either routed
//! direct to its speaker or turned into a fixed-position "virtual object" and
//! VBAP-panned. *Where* a virtualised channel goes is the placement policy
//! of the stream's source family ([`renderer::placement`]): a direction on
//! the listener's sphere, a corner of the room model, or the user's own
//! entry. Shared by the `orender` CLI and the embedded engine for identical
//! behaviour.

use crate::osc::ObjectMeta;
use bridge_api::{RChannelLabel, RChannelPose};
use renderer::live_params::SurroundPlacement;
use renderer::placement::{PlacementMode, SourceFamily};
use renderer::speaker_layout::SpeakerLayout;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The room warp the output applies, taken together so a pose resolver reads
/// one value instead of four ([`renderer::live_params::RoomRatios`]): a pose
/// stated as an angle is pre-compensated for it ([`angles_to_normalized`]),
/// so it must be the warp that is actually undone downstream
/// ([`RoomRatios::for_output`]).
pub use renderer::live_params::RoomRatios;

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

/// How one stream's fixed channels are placed: its family's effective mode
/// and entries ([`renderer::placement::PlacementState::effective`]) plus the
/// poses its bridge declared for the current labels.
#[derive(Clone, Copy, Debug)]
pub struct PlacementPolicy<'a> {
    pub mode: PlacementMode,
    /// The family's entries: `spatialize` and `gain_db` in every mode, the
    /// pose in manual mode.
    pub layout: Option<&'a SpeakerLayout>,
    /// The angles the format states for its channels
    /// ([`bridge_api::FormatBridge::fixed_channel_poses`]); read in sphere
    /// mode only.
    pub declared: &'a [RChannelPose],
}

impl<'a> PlacementPolicy<'a> {
    /// The room model with no entries: every channel at its catalogue
    /// corner, LFE direct. What a fresh install does.
    pub const fn room() -> Self {
        Self {
            mode: PlacementMode::Room,
            layout: None,
            declared: &[],
        }
    }

    /// Manual mode over the given entries.
    pub const fn manual(layout: &'a SpeakerLayout) -> Self {
        Self {
            mode: PlacementMode::Manual,
            layout: Some(layout),
            declared: &[],
        }
    }

    /// Sphere mode over the given declared angles, no entries.
    pub const fn sphere(declared: &'a [RChannelPose]) -> Self {
        Self {
            mode: PlacementMode::Sphere,
            layout: None,
            declared,
        }
    }

    pub const fn with_layout(self, layout: Option<&'a SpeakerLayout>) -> Self {
        Self { layout, ..self }
    }

    pub const fn with_declared(self, declared: &'a [RChannelPose]) -> Self {
        Self { declared, ..self }
    }
}

/// A family's effective placement copied out of the live params, so a plan
/// can be built after the read lock is released.
#[derive(Clone, Debug, PartialEq)]
pub struct OwnedPlacement {
    pub mode: PlacementMode,
    pub layout: Option<SpeakerLayout>,
}

impl OwnedPlacement {
    pub fn from_live(live: &renderer::live_params::LiveParams, family: SourceFamily) -> Self {
        let effective = live.placement.effective(family, live.binaural.output_mode);
        Self {
            mode: effective.mode,
            layout: effective.layout.cloned(),
        }
    }

    pub fn policy<'a>(&'a self, declared: &'a [RChannelPose]) -> PlacementPolicy<'a> {
        PlacementPolicy {
            mode: self.mode,
            layout: self.layout.as_ref(),
            declared,
        }
    }
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
    room: RoomRatios,
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
        // The entry states an angle: kept whatever the room's extent along
        // it (a measured room can be lower than the entry's radius).
        let [x, y, z] = room.inverse_direction([sx, sy, sz]);
        (speaker.name.clone(), x, y, z)
    }
}

/// Normalized ADM position that renders at an absolute direction under the
/// room warp of the output in force ([`RoomRatios::for_output`], none on the
/// direct binaural path): spherical → real ADM → inverse room warp, the conversion the
/// polar branch of [`speaker_pose_to_normalized`] makes for a placement entry
/// that states an angle. A corner channel is deliberately carried around by
/// the room warp; a channel stated as an angle — a pose the bridge declared,
/// or a height-tier label — must land on that angle whatever the room is,
/// a room lower or shorter than the unit radius included: the direction is
/// drawn back into the room rather than clamped axis by axis
/// ([`RoomRatios::inverse_direction`]), which bent a 30° height to 24.8° in
/// a measured room of height ratio 0.4 (#803).
fn angles_to_normalized(azimuth_deg: f32, elevation_deg: f32, room: RoomRatios) -> (f32, f32, f32) {
    let (sx, sy, sz) = renderer::spatial_vbap::spherical_to_adm(azimuth_deg, elevation_deg, 1.0);
    let [x, y, z] = room.inverse_direction([sx, sy, sz]);
    (x, y, z)
}

/// The nominal direction of a label on the listener's sphere, `(azimuth,
/// elevation)` in degrees, for sphere mode when the format declares none.
/// ITU-R BS.2051 where it names the position (`M±030`, `M±110`, `U±045`,
/// `T+000`…), the tier's convention otherwise: the top tier at 45° of
/// elevation, the height tier at 30°, the wide pair at ±60°, the front
/// centre pair halfway to the centre. A 7.x source's side pair is `M±090`;
/// a 4.x/5.x source's surround pair is `M±110`. `None` for the labels that
/// are not a direction (`Object`, `Unknown`).
pub(crate) fn nominal_angle(label: RChannelLabel, use_7_1: bool) -> Option<(f32, f32)> {
    use RChannelLabel::*;
    Some(match label {
        L => (-30.0, 0.0),
        R => (30.0, 0.0),
        C | LFE | LFE2 => (0.0, 0.0),
        Ls => (if use_7_1 { -90.0 } else { -110.0 }, 0.0),
        Rs => (if use_7_1 { 90.0 } else { 110.0 }, 0.0),
        Lb => (-135.0, 0.0),
        Rb => (135.0, 0.0),
        Cb => (180.0, 0.0),
        Lsc => (-15.0, 0.0),
        Rsc => (15.0, 0.0),
        Lw => (-60.0, 0.0),
        Rw => (60.0, 0.0),
        Lsd => (-120.0, 0.0),
        Rsd => (120.0, 0.0),
        Tfl => (-45.0, 45.0),
        Tfr => (45.0, 45.0),
        Tsl => (-90.0, 45.0),
        Tsr => (90.0, 45.0),
        Tbl => (-135.0, 45.0),
        Tbr => (135.0, 45.0),
        Tfc => (0.0, 45.0),
        Tc => (0.0, 90.0),
        Lh => (-30.0, 30.0),
        Rh => (30.0, 30.0),
        Ch => (0.0, 30.0),
        Lhs => (-110.0, 30.0),
        Rhs => (110.0, 30.0),
        Object | Unknown => return None,
    })
}

/// Whether the source carries back channels (`Lb`/`Rb`/`Cb`), i.e. is a
/// 7.x bed. Without them the surround pair of a 4.x/5.x source has no
/// canonical corner ([`surround_placement_override`]), and a bed's back
/// entries stand in for its surround ones ([`find_bed_entry`]).
pub(crate) fn source_has_back(labels: &[RChannelLabel]) -> bool {
    labels
        .iter()
        .any(|l| matches!(l, RChannelLabel::Lb | RChannelLabel::Rb | RChannelLabel::Cb))
}

/// The bed entry for a channel label: the first entry whose name is one of
/// the label's spellings ([`bridge_api::labels::name_matches`], the same
/// table the output channel map and Studio's catalogue use). A 4.x/5.x
/// source (`use_7_1 == false`) has one surround pair where a 7.x bed has two,
/// so its `Ls`/`Rs` fall back to the bed's back entry (`Lb`/`Rb`) when there
/// is no surround one — the bundled 5.1 layout names its surrounds `BL`/`BR`.
fn find_bed_entry(
    layout: &SpeakerLayout,
    label: RChannelLabel,
    use_7_1: bool,
) -> Option<&renderer::speaker_layout::Speaker> {
    let find = |label: RChannelLabel| {
        layout
            .speakers
            .iter()
            .find(|speaker| bridge_api::labels::name_matches(&speaker.name, label))
    };
    find(label).or_else(|| {
        let folded = match label {
            RChannelLabel::Ls if !use_7_1 => RChannelLabel::Lb,
            RChannelLabel::Rs if !use_7_1 => RChannelLabel::Rb,
            _ => return None,
        };
        find(folded)
    })
}

/// The room model's pose for a label, as a **normalized cartesian** corner:
/// used for room mode when the bundled 5.1/7.1 layout has no entry, and for
/// the published catalogue. These mirror `layouts/legacy/5.1.yaml`/`7.1.yaml`
/// and Studio's fallback bed, so a corner channel lands exactly in its corner
/// after the room warp — cartesian, not polar/distance, which used to pull the
/// corners inward. Floor row at `z = 0`, top tier at the ceiling `z = 1`, and
/// the height tier on the wall above the floor speaker of the same name, at
/// the height that makes 30° of elevation in a cube (`z = tan 30° × the
/// horizontal distance`). `use_7_1` does not change these (the corners are
/// layout-independent); the surround pair and the height above it are
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
        RChannelLabel::Lsc => ("Lsc", -0.5, 1.0, 0.0),
        RChannelLabel::Rsc => ("Rsc", 0.5, 1.0, 0.0),
        RChannelLabel::Lw => ("Lw", -1.0, 0.5, 0.0),
        RChannelLabel::Rw => ("Rw", 1.0, 0.5, 0.0),
        RChannelLabel::Lsd => ("Lsd", -1.0, -0.5, 0.0),
        RChannelLabel::Rsd => ("Rsd", 1.0, -0.5, 0.0),
        // Top tier at the ceiling (z = 1), mirroring the floor corners.
        RChannelLabel::Tfl => ("TFL", -1.0, 1.0, 1.0),
        RChannelLabel::Tfr => ("TFR", 1.0, 1.0, 1.0),
        RChannelLabel::Tbl => ("TBL", -1.0, -1.0, 1.0),
        RChannelLabel::Tbr => ("TBR", 1.0, -1.0, 1.0),
        RChannelLabel::Tsl => ("TSL", -1.0, 0.0, 1.0),
        RChannelLabel::Tsr => ("TSR", 1.0, 0.0, 1.0),
        RChannelLabel::Tc => ("TC", 0.0, 0.0, 1.0),
        RChannelLabel::Tfc => ("TFC", 0.0, 1.0, 1.0),
        // Height tier: on the wall above its floor speaker, 30° up in a cube.
        RChannelLabel::Lh => ("Lh", -1.0, 1.0, HEIGHT_TIER_Z_CORNER),
        RChannelLabel::Rh => ("Rh", 1.0, 1.0, HEIGHT_TIER_Z_CORNER),
        RChannelLabel::Ch => ("Ch", 0.0, 1.0, HEIGHT_TIER_Z_WALL),
        RChannelLabel::Lhs => ("Lhs", -1.0, 0.0, HEIGHT_TIER_Z_WALL),
        RChannelLabel::Rhs => ("Rhs", 1.0, 0.0, HEIGHT_TIER_Z_WALL),
        _ => return None,
    };
    Some((name.to_string(), x, y, z))
}

/// `tan 30° × √2`: the normalized height of a height-tier speaker above a
/// corner floor speaker, so that it sits 30° up in a cube.
const HEIGHT_TIER_Z_CORNER: f32 = 0.816_496_6;
/// `tan 30° × 1`: the same above a speaker in the middle of a wall.
const HEIGHT_TIER_Z_WALL: f32 = 0.577_350_3;

/// Stable editor catalogue available even when no stream is active. Every
/// fixed-channel label has a canonical pose plus the accepted spellings for its
/// label (from `bridge_api::labels`), so Studio can match channel names with the
/// same alias tolerance as layout YAMLs; dynamic objects and unknown channels are
/// deliberately excluded.
pub fn fixed_channel_catalog_json() -> String {
    use RChannelLabel::{
        C, Cb, Ch, L, LFE, LFE2, Lb, Lh, Lhs, Ls, Lsc, Lsd, Lw, R, Rb, Rh, Rhs, Rs, Rsc, Rsd, Rw,
        Tbl, Tbr, Tc, Tfc, Tfl, Tfr, Tsl, Tsr,
    };
    const FIXED: [RChannelLabel; 29] = [
        L, R, C, LFE, Ls, Rs, Lb, Rb, Tfl, Tfr, Tbl, Tbr, Lsc, Rsc, Cb, Lsd, Rsd, Lw, Rw, LFE2,
        Tsl, Tsr, Tc, Tfc, Lh, Rh, Ch, Lhs, Rhs,
    ];
    let entries: Vec<serde_json::Value> = FIXED
        .iter()
        .filter_map(|&label| {
            let (_, x, y, z) = fallback_virtual_bed_pose(label, true)?;
            // `x`/`y`/`z` is the room-model corner, `azimuth`/`elevation` the
            // nominal direction of sphere mode (the 5.1 surround convention
            // for `Ls`/`Rs`: the catalogue has no source shape to hand), so
            // an editor can show either mode's default for a family that is
            // not playing.
            let (azimuth, elevation) = nominal_angle(label, false)?;
            Some(serde_json::json!({
                "label": bridge_api::labels::canonical_name(label),
                "aliases": bridge_api::labels::aliases_for(label),
                "group": if z > 0.0 { "height" } else { "floor" },
                "x": x,
                "y": y,
                "z": z,
                "azimuth": azimuth,
                "elevation": elevation,
                "spatialize": default_channel_spatialize(label),
            }))
        })
        .collect();
    serde_json::Value::Array(entries).to_string()
}

/// For a 4.x/5.x source (no back channels) the surround pair (`Ls`/`Rs`) has no
/// canonical corner, so `surround_placement` decides it: `Side` → the side
/// corner `(∓1, 0, 0)`, `Back` → the back corner `(∓1, −1, 0)` (sign by L/R).
/// The height-tier pair above it (`Lhs`/`Rhs`) follows, at its 30° height.
/// Returns the normalized override position, or `None` (no override) for any
/// other label or when the source already has back channels.
///
/// Room model only: a sphere direction, a declared angle and a user's own
/// entry all say where the pair is, and are never overridden.
fn surround_placement_override(
    label: RChannelLabel,
    use_7_1: bool,
    placement: SurroundPlacement,
) -> Option<(f32, f32, f32)> {
    if use_7_1 {
        return None;
    }
    let (sign, height) = match label {
        RChannelLabel::Ls => (-1.0, false),
        RChannelLabel::Rs => (1.0, false),
        RChannelLabel::Lhs => (-1.0, true),
        RChannelLabel::Rhs => (1.0, true),
        _ => return None,
    };
    let (y, z) = match (placement, height) {
        (SurroundPlacement::Side, false) => (0.0, 0.0),
        (SurroundPlacement::Back, false) => (-1.0, 0.0),
        (SurroundPlacement::Side, true) => (0.0, HEIGHT_TIER_Z_WALL),
        (SurroundPlacement::Back, true) => (-1.0, HEIGHT_TIER_Z_CORNER),
    };
    Some((sign, y, z))
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

/// Resolve a channel's pose as a **normalized ADM position** in [-1, 1]
/// under the policy of its family:
///
/// - manual: the family's own entry, converted per its `coord_mode`
///   ([`speaker_pose_to_normalized`]); a channel without one falls back to
///   the room model;
/// - room: the room model ([`room_pose`]);
/// - sphere: the direction the format declared, else the nominal direction
///   of the label ([`sphere_pose`]).
fn resolve_virtual_bed_pose(
    label: RChannelLabel,
    use_7_1: bool,
    policy: &PlacementPolicy<'_>,
    room: RoomRatios,
    surround_placement: SurroundPlacement,
) -> Option<(String, f32, f32, f32)> {
    match policy.mode {
        PlacementMode::Manual => {
            if let Some(found) = find_virtual_bed_entry(policy.layout, label, use_7_1) {
                return Some(speaker_pose_to_normalized(found, room));
            }
            room_pose(label, use_7_1, room, surround_placement)
        }
        PlacementMode::Room => room_pose(label, use_7_1, room, surround_placement),
        PlacementMode::Sphere => sphere_pose(label, use_7_1, policy.declared, room),
    }
}

/// The room model: the bundled 5.1/7.1 layout's entry, else the catalogue
/// corner ([`fallback_virtual_bed_pose`]), then the
/// [`surround_placement_override`] for a 4.x/5.x source. A corner is a
/// normalized position and is carried around by the room warp, the way an
/// object at that position is: `L` is the front-left corner of *the* room,
/// whatever angle that makes.
fn room_pose(
    label: RChannelLabel,
    use_7_1: bool,
    room: RoomRatios,
    surround_placement: SurroundPlacement,
) -> Option<(String, f32, f32, f32)> {
    if let Some((ox, oy, oz)) = surround_placement_override(label, use_7_1, surround_placement) {
        let name = fallback_virtual_bed_pose(label, use_7_1)
            .map(|(name, ..)| name)
            .unwrap_or_else(|| bridge_api::labels::canonical_name(label).to_string());
        return Some((name, ox, oy, oz));
    }

    let layouts = virtual_bed_layouts();
    let layout_opt = if use_7_1 {
        layouts.layout_7_1.as_ref()
    } else {
        layouts.layout_5_1.as_ref()
    };
    if let Some(found) = layout_opt.and_then(|layout| find_bed_entry(layout, label, use_7_1)) {
        return Some(speaker_pose_to_normalized(found, room));
    }

    // Cartesian corner: use x/y/z directly (clamped), exactly like the
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

/// The sphere: the direction the format declared for the label, else its
/// nominal direction ([`nominal_angle`]), converted so the channel renders at
/// that angle under the room in force ([`angles_to_normalized`]).
fn sphere_pose(
    label: RChannelLabel,
    use_7_1: bool,
    declared: &[RChannelPose],
    room: RoomRatios,
) -> Option<(String, f32, f32, f32)> {
    let (azimuth, elevation) = declared
        .iter()
        .find(|pose| pose.label == label)
        .map(|pose| (pose.azimuth_deg, pose.elevation_deg))
        .or_else(|| nominal_angle(label, use_7_1))?;
    let (x, y, z) = angles_to_normalized(azimuth, elevation, room);
    Some((
        bridge_api::labels::canonical_name(label).to_string(),
        x,
        y,
        z,
    ))
}

/// Where the bed renders each channel under `policy`, as a normalized ADM
/// position, in channel order: the pose [`plan_channel_render`] gives the
/// channel when it is virtualized — whether or not this one is — so the
/// same room corner, sphere direction or user entry. `None` for a label with
/// no pose (`Object`, `Unknown`). `out` is cleared and refilled.
///
/// The channel-object stages place what they synthesize from these
/// ([`crate::object_gen::PrepareCtx::bed_poses`]): a phantom between two
/// channels sits between them wherever the family's policy put them.
pub fn resolve_bed_poses(
    channel_labels: &[RChannelLabel],
    policy: &PlacementPolicy<'_>,
    room: RoomRatios,
    surround_placement: SurroundPlacement,
    out: &mut Vec<Option<[f64; 3]>>,
) {
    let use_7_1 = source_has_back(channel_labels);
    out.clear();
    out.extend(channel_labels.iter().map(|&label| {
        resolve_virtual_bed_pose(label, use_7_1, policy, room, surround_placement)
            .map(|(_, x, y, z)| [x as f64, y as f64, z as f64])
    }));
}

/// [`resolve_bed_poses`] under the room model with no entries: the poses a
/// fresh install gives, for the tests of the stages that consume them.
#[cfg(test)]
pub(crate) fn room_bed_poses(
    channel_labels: &[RChannelLabel],
    surround_placement: SurroundPlacement,
) -> Vec<Option<[f64; 3]>> {
    let mut poses = Vec::new();
    resolve_bed_poses(
        channel_labels,
        &PlacementPolicy::room(),
        RoomRatios::UNIT,
        surround_placement,
        &mut poses,
    );
    poses
}

pub fn build_virtual_bed_events(
    channel_labels: &[RChannelLabel],
    policy: &PlacementPolicy<'_>,
    room: RoomRatios,
    surround_placement: SurroundPlacement,
) -> Option<Vec<renderer::spatial_renderer::SpatialChannelEvent>> {
    let use_7_1 = source_has_back(channel_labels);

    let mut events: Vec<renderer::spatial_renderer::SpatialChannelEvent> =
        Vec::with_capacity(channel_labels.len());

    for (channel_idx, label) in channel_labels.iter().enumerate() {
        let (_name, x, y, z) =
            match resolve_virtual_bed_pose(*label, use_7_1, policy, room, surround_placement) {
                Some(v) => v,
                None => continue,
            };
        events.push(renderer::spatial_renderer::SpatialChannelEvent {
            channel_idx,
            is_bed: false,
            gain_db: Some(bed_entry_gain_db(policy.layout, *label, use_7_1)),
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

pub fn build_virtual_bed_objects(
    channel_labels: &[RChannelLabel],
    policy: &PlacementPolicy<'_>,
    output_layout: Option<&SpeakerLayout>,
    room: RoomRatios,
    surround_placement: SurroundPlacement,
) -> Option<Vec<ObjectMeta>> {
    let use_7_1 = source_has_back(channel_labels);

    // Used to anchor a direct channel onto its output speaker so Studio shows it
    // snapped to that speaker (its `directSpeakerIndex` decoration).
    let label_to_speaker = output_layout.map(|layout| layout.label_to_speaker_mapping());

    let mut objects: Vec<ObjectMeta> = Vec::with_capacity(channel_labels.len());
    for label in channel_labels {
        // Emit every channel so the editor/overlay can show them all: virtualized
        // channels carry a free position, direct channels (e.g. LFE) carry a
        // `direct_speaker_index` so Studio anchors them onto their speaker.
        let spatialize = channel_is_spatialized(policy.layout, *label, use_7_1);
        let (_source_name, x, y, z) =
            match resolve_virtual_bed_pose(*label, use_7_1, policy, room, surround_placement) {
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
        let gain = find_virtual_bed_entry(policy.layout, *label, use_7_1)
            .map(|entry| entry.gain_db)
            .unwrap_or(0.0);
        // Named by the canonical label whatever gave the pose (a layout
        // entry's own spelling, a corner's, a direction's), so a channel
        // keeps its name across the placement modes.
        let name = bridge_api::labels::canonical_name(*label).to_string();
        objects.push(ObjectMeta {
            name: name.clone(),
            x,
            y,
            z,
            coord_mode: "cartesian".to_string(),
            direct_speaker_index,
            gain,
            priority: 0.0,
            size: [0.0, 0.0, 0.0],
            fixed: true,
            label: name,
            // A bed channel at its canonical pose: `fixed` already says what
            // it is, so it is not one of the synthesized kinds.
            kind: crate::object_gen::ObjectKind::Dynamic,
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
/// channel label in the family's entries, if it has any ([`find_bed_entry`]).
fn find_virtual_bed_entry(
    layout: Option<&SpeakerLayout>,
    label: RChannelLabel,
    use_7_1: bool,
) -> Option<&renderer::speaker_layout::Speaker> {
    find_bed_entry(layout?, label, use_7_1)
}

/// The bed entry's `gain_db` as an audio-event gain: clamped into the event
/// domain (where `GAIN_DB_NEG_INF` = −128 is the −inf floor), unity when the
/// bed has no entry for the label. Carried as `f32` end to end so the editor's
/// 0.1 dB steps survive — the whole-dB `i8` lives only on the decoder side.
///
/// The event is what the render paths consume; `ObjectMeta.gain` is only the
/// Studio/OSC display value. Stamping the entry gain here is what makes the
/// bed editor's per-channel gain reach the sound (#220).
fn bed_entry_gain_db(layout: Option<&SpeakerLayout>, label: RChannelLabel, use_7_1: bool) -> f32 {
    find_virtual_bed_entry(layout, label, use_7_1)
        .map(|entry| {
            let db = entry.gain_db;
            if db.is_finite() {
                db.clamp(renderer::spatial_renderer::GAIN_DB_NEG_INF, 127.0)
            } else {
                0.0
            }
        })
        .unwrap_or(0.0)
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
/// family's placement `policy`.
pub fn plan_channel_render(
    mode: renderer::live_params::ChannelRenderMode,
    channel_labels: &[RChannelLabel],
    policy: &PlacementPolicy<'_>,
    output_layout: Option<&SpeakerLayout>,
    room: RoomRatios,
    surround_placement: SurroundPlacement,
) -> ChannelRenderPlan {
    use renderer::live_params::ChannelRenderMode;
    match mode {
        ChannelRenderMode::Host => ChannelRenderPlan::HostPassthrough,
        ChannelRenderMode::Spatial => build_virtual_bed_plan(
            channel_labels,
            policy,
            output_layout,
            room,
            surround_placement,
        ),
    }
}

/// Spatial mode: decide each channel's placement against the virtual bed. A
/// channel marked `spatialize:false` (e.g. LFE) routes direct to its speaker
/// (a bed id in `bed_indices` + a bed event); a `spatialize:true` channel is
/// virtualized at the bed's position (the `usize::MAX` sentinel in `bed_indices`
/// + an object event carrying the position). A frame may freely mix the two.
fn build_virtual_bed_plan(
    channel_labels: &[RChannelLabel],
    policy: &PlacementPolicy<'_>,
    output_layout: Option<&SpeakerLayout>,
    room: RoomRatios,
    surround_placement: SurroundPlacement,
) -> ChannelRenderPlan {
    let use_7_1 = source_has_back(channel_labels);

    // Label → output-speaker map, so a direct surround can be rerouted to a
    // back speaker (Back placement) only when the layout actually has one.
    let label_to_speaker = output_layout.map(|layout| layout.label_to_speaker_mapping());

    let mut routes: Vec<renderer::spatial_renderer::ChannelRoute> =
        Vec::with_capacity(channel_labels.len());
    let mut events: Vec<renderer::spatial_renderer::SpatialChannelEvent> =
        Vec::with_capacity(channel_labels.len());

    for (channel_idx, label) in channel_labels.iter().enumerate() {
        let spatialize = channel_is_spatialized(policy.layout, *label, use_7_1);
        let gain_db = bed_entry_gain_db(policy.layout, *label, use_7_1);
        if spatialize {
            // Virtualize: place an object at the policy's pose.
            match resolve_virtual_bed_pose(*label, use_7_1, policy, room, surround_placement) {
                Some((_name, x, y, z)) => {
                    routes.push(renderer::spatial_renderer::ChannelRoute::Virtual);
                    events.push(renderer::spatial_renderer::SpatialChannelEvent {
                        channel_idx,
                        is_bed: false,
                        gain_db: Some(gain_db),
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
                        gain_db: Some(gain_db),
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
    family: SourceFamily,
    declared_poses: &[RChannelPose],
) -> Option<Vec<ObjectMeta>> {
    if fixed_labels.is_empty() {
        return None;
    }
    let control = renderer.renderer_control();
    let topology = control.active_topology();
    let (placement, surround_placement, room) = {
        let live = control.live.read();
        (
            OwnedPlacement::from_live(&live, family),
            live.options.surround_placement,
            RoomRatios::for_output(&live, &topology),
        )
    };
    build_virtual_bed_objects(
        fixed_labels,
        &placement.policy(declared_poses),
        Some(&topology.speaker_layout),
        room,
        surround_placement,
    )
}

/// Everything [`plan_channel_render`] reads, kept so the next frame can tell
/// whether replanning is needed at all. One key for both planners — the
/// bed-only one and the fixed prefix of object streams — so the two cannot
/// disagree about what invalidates a plan.
///
/// The output layout is represented by the `geometry_generation` of the
/// topology the plan read it from rather than by a copy of the layout: every
/// change that reaches the speaker geometry bumps the generation before the
/// rebuild, and the rebuilt topology carries it
/// ([`renderer::live_params::RenderTopology::geometry_generation`]). Reading it
/// off the *active* topology rather than off the control matters: the control's
/// counter moves when the rebuild is requested, while the layout the plan
/// reads only changes once the rebuild lands — keyed on the former, a frame
/// planned in between would cache the old layout's routes for good.
///
/// The family's placement is compared by value because editing it bumps
/// nothing — it is a plain live param — so a generation counter would miss it
/// and the bed would silently stop following Studio's editor.
#[derive(Clone, PartialEq)]
struct ChannelPlanKey {
    labels: Vec<RChannelLabel>,
    /// The poses the bridge declared for these labels. Declaration-level like
    /// the labels, so a steady stream compares a short slice per frame.
    declared_poses: Vec<RChannelPose>,
    family: SourceFamily,
    mode: renderer::live_params::ChannelRenderMode,
    placement: OwnedPlacement,
    surround_placement: SurroundPlacement,
    room: RoomRatios,
    layout_generation: u64,
}

impl ChannelPlanKey {
    /// `topology` is the active one: the room the plan pans in
    /// ([`RoomRatios::for_output`]) and the generation that names its layout
    /// are read off it.
    fn capture(
        live: &renderer::live_params::LiveParams,
        mode: renderer::live_params::ChannelRenderMode,
        channel_labels: &[RChannelLabel],
        family: SourceFamily,
        declared_poses: &[RChannelPose],
        topology: &renderer::live_params::RenderTopology,
    ) -> Self {
        Self {
            labels: channel_labels.to_vec(),
            declared_poses: declared_poses.to_vec(),
            family,
            mode,
            placement: OwnedPlacement::from_live(live, family),
            surround_placement: live.options.surround_placement,
            room: RoomRatios::for_output(live, topology),
            layout_generation: topology.geometry_generation,
        }
    }

    /// Whether a plan built from this key is still valid for `live`.
    ///
    /// Ordered cheapest-first: the scalars and the label list reject almost
    /// every real change before the virtual bed is compared element by element.
    ///
    /// The key is destructured rather than read field by field so that adding a
    /// field to it without deciding how it compares here is a compile error, not
    /// a silently stale plan.
    fn matches(
        &self,
        live: &renderer::live_params::LiveParams,
        mode: renderer::live_params::ChannelRenderMode,
        channel_labels: &[RChannelLabel],
        family: SourceFamily,
        declared_poses: &[RChannelPose],
        topology: &renderer::live_params::RenderTopology,
    ) -> bool {
        let Self {
            labels,
            declared_poses: planned_poses,
            family: planned_family,
            mode: planned_mode,
            placement,
            surround_placement,
            room: planned_room,
            layout_generation: planned_generation,
        } = self;

        *planned_generation == topology.geometry_generation
            && *planned_mode == mode
            && *surround_placement == live.options.surround_placement
            && *planned_room == RoomRatios::for_output(live, topology)
            && *planned_family == family
            && labels.as_slice() == channel_labels
            && planned_poses.as_slice() == declared_poses
            && placement.mode
                == live
                    .placement
                    .effective_mode(family, live.binaural.output_mode)
            && placement.layout.as_ref() == live.placement.effective_layout(family)
    }
}

/// The key of the last plan plus the routing it applied: the part both
/// planners share. [`lookup`](Self::lookup) is the per-frame check,
/// [`apply_routes`](Self::apply_routes) the on-change renderer update.
#[derive(Default)]
struct PlanCache {
    key: Option<ChannelPlanKey>,
    applied_routes: Option<Vec<renderer::spatial_renderer::ChannelRoute>>,
}

impl PlanCache {
    fn reset(&mut self) {
        self.key = None;
        self.applied_routes = None;
    }

    /// `None` when the cached plan still holds; otherwise the key to plan
    /// against and the topology whose layout it was taken from. One lock
    /// acquisition: compare first, and only clone the inputs into a fresh key
    /// when the comparison actually failed.
    fn lookup(
        &self,
        control: &renderer::live_params::RendererControl,
        mode: Option<renderer::live_params::ChannelRenderMode>,
        channel_labels: &[RChannelLabel],
        family: SourceFamily,
        declared_poses: &[RChannelPose],
    ) -> Option<(
        ChannelPlanKey,
        std::sync::Arc<renderer::live_params::RenderTopology>,
    )> {
        // One load for the generation, the room and the layout the plan
        // reads: the three describe the same topology.
        let topology = control.active_topology();
        let live = control.live.read();
        let mode = mode.unwrap_or(live.channel_render_mode);
        if self.key.as_ref().is_some_and(|key| {
            key.matches(
                &live,
                mode,
                channel_labels,
                family,
                declared_poses,
                &topology,
            )
        }) {
            return None;
        }
        let key = ChannelPlanKey::capture(
            &live,
            mode,
            channel_labels,
            family,
            declared_poses,
            &topology,
        );
        Some((key, topology))
    }

    fn apply_routes(
        &mut self,
        renderer: &renderer::spatial_renderer::SpatialRenderer,
        routes: Vec<renderer::spatial_renderer::ChannelRoute>,
    ) {
        if self.applied_routes.as_deref() != Some(routes.as_slice()) {
            renderer.configure_channel_routing(&routes);
            self.applied_routes = Some(routes);
        }
    }
}

/// What a planned bed-only frame turned out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BedPlanKind {
    /// Renderable. The routing has been applied to the renderer and the events
    /// are available from [`BedChannelPlanner::events`].
    Events,
    /// The channel mode asks the host to handle the channels itself.
    HostPassthrough,
    /// Nothing maps to the output: the host should emit silence.
    Silence,
}

/// Shared bed-only (channel-content) planner for both hosts.
///
/// A bed frame's mapping depends only on the channel labels and a handful of
/// live params, none of which change per frame — but planning it is not cheap:
/// it builds a label→speaker map and, for every channel placed by room ratios,
/// solves a depth warp by bisection. Doing that on every frame cost real time on
/// short frames (TrueHD delivers 40 samples at a time, so ~1200 plans/second)
/// and, worse, deep-cloned the virtual bed and the output layout each time just
/// to read them.
///
/// So the inputs are captured on the first frame and compared on the next: a
/// steady stream replans zero times, and the comparison happens under the read
/// lock *before* anything is cloned.
#[derive(Default)]
pub struct BedChannelPlanner {
    cache: PlanCache,
    kind: Option<BedPlanKind>,
    events: Vec<renderer::spatial_renderer::SpatialChannelEvent>,
    /// Where the bed renders each channel ([`resolve_bed_poses`]), for the
    /// channel-object stages. Planned with the events, so it costs nothing
    /// on a steady stream.
    poses: Vec<Option<[f64; 3]>>,
}

impl BedChannelPlanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget everything (stream reset / new segment).
    ///
    /// The renderer's per-channel state is dropped on a segment reset, so the
    /// applied routing has to be forgotten too or the next plan would consider
    /// itself already applied.
    pub fn reset(&mut self) {
        self.cache.reset();
        self.kind = None;
        self.events.clear();
        self.poses.clear();
    }

    /// The events of the current plan, in channel order. Empty unless the last
    /// [`plan`](Self::plan) returned [`BedPlanKind::Events`].
    pub fn events(&self) -> &[renderer::spatial_renderer::SpatialChannelEvent] {
        &self.events
    }

    /// Where the bed renders each channel of the current plan, in channel
    /// order ([`resolve_bed_poses`]) — what the channel-object stages place
    /// their objects from ([`crate::object_gen::PrepareCtx::bed_poses`]).
    /// Empty unless the last [`plan`](Self::plan) returned
    /// [`BedPlanKind::Events`].
    pub fn poses(&self) -> &[Option<[f64; 3]>] {
        &self.poses
    }

    /// Plan this frame's bed mapping, reusing the previous plan when nothing it
    /// depends on has changed, and apply the routing to the renderer on change.
    pub fn plan(
        &mut self,
        renderer: &renderer::spatial_renderer::SpatialRenderer,
        channel_labels: &[RChannelLabel],
        family: SourceFamily,
        declared_poses: &[RChannelPose],
    ) -> BedPlanKind {
        let control = renderer.renderer_control();
        let lookup = self
            .cache
            .lookup(&control, None, channel_labels, family, declared_poses);
        let Some((key, topology)) = lookup else {
            // A hit means a previous plan, which set the kind with the key.
            return self.kind.unwrap_or(BedPlanKind::Silence);
        };

        let policy = key.placement.policy(&key.declared_poses);
        let kind = match plan_channel_render(
            key.mode,
            &key.labels,
            &policy,
            Some(&topology.speaker_layout),
            key.room,
            key.surround_placement,
        ) {
            ChannelRenderPlan::Events { events, routes } => {
                self.cache.apply_routes(renderer, routes);
                self.events = events;
                resolve_bed_poses(
                    &key.labels,
                    &policy,
                    key.room,
                    key.surround_placement,
                    &mut self.poses,
                );
                BedPlanKind::Events
            }
            ChannelRenderPlan::HostPassthrough => {
                self.events.clear();
                self.poses.clear();
                BedPlanKind::HostPassthrough
            }
            ChannelRenderPlan::Silence => {
                self.events.clear();
                self.poses.clear();
                BedPlanKind::Silence
            }
        };

        self.cache.key = Some(key);
        self.kind = Some(kind);
        kind
    }
}

/// Shared fixed-channel planner for object streams (engine + CLI decode
/// paths). Plans the fixed prefix of the channel list through
/// [`plan_channel_render`] — virtualized by default, per-entry direct opt-in
/// via the placement layout — caches the result on the same key as the
/// bed-only planner, and applies the routing to the renderer only on change.
#[derive(Default)]
pub struct FixedChannelPlanner {
    cache: PlanCache,
    /// Bed-entry trim per fixed channel index, cached at plan time so the
    /// per-metadata-frame event build ([`crate::spatial::build_spatial_channel_events`])
    /// indexes a slice instead of re-matching label aliases.
    trims: Vec<f32>,
}

impl FixedChannelPlanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget everything (stream reset / new segment).
    pub fn reset(&mut self) {
        self.cache.reset();
        self.trims.clear();
    }

    /// Fixed labels of the last planned prefix.
    pub fn fixed_labels(&self) -> &[RChannelLabel] {
        self.cache.key.as_ref().map_or(&[], |key| &key.labels)
    }

    /// Bed-entry trim (dB) per fixed channel index, from the last plan.
    ///
    /// The stream's own OAMD channel gains re-stamp the bed channels on every
    /// metadata frame, which would silently undo the plan events' trim; the
    /// hosts pass this slice to the event build so the two combine instead.
    pub fn fixed_trims(&self) -> &[f32] {
        &self.trims
    }

    /// Apply a route set computed elsewhere (the fixed-only render path),
    /// deduplicated against the last applied set.
    pub fn apply_routes(
        &mut self,
        renderer: &renderer::spatial_renderer::SpatialRenderer,
        routes: Vec<renderer::spatial_renderer::ChannelRoute>,
    ) {
        self.cache.apply_routes(renderer, routes);
    }

    /// Plan the fixed prefix (labels before the first `Object` channel) of an
    /// object stream and apply it. Mode is always spatial here: an object
    /// stream cannot pass through the host, so the live channel mode only
    /// applies to fixed-only streams. On replan the fixed channels' pose/gain
    /// events are appended to `out` (the renderer caches per-channel state,
    /// so they are only needed when the plan changes).
    pub fn plan_object_stream_fixed(
        &mut self,
        channel_labels: &[RChannelLabel],
        family: SourceFamily,
        declared_poses: &[RChannelPose],
        renderer: &renderer::spatial_renderer::SpatialRenderer,
        out: &mut Vec<renderer::spatial_renderer::SpatialChannelEvent>,
    ) {
        let fixed_end = channel_labels
            .iter()
            .position(|l| *l == RChannelLabel::Object)
            .unwrap_or(channel_labels.len());
        let fixed = &channel_labels[..fixed_end];

        let control = renderer.renderer_control();
        let Some((key, topology)) = self.cache.lookup(
            &control,
            Some(renderer::live_params::ChannelRenderMode::Spatial),
            fixed,
            family,
            declared_poses,
        ) else {
            return;
        };

        // Trim per fixed channel, mirroring the gain the plan events carry, so
        // the hosts can fold it into the stream's recurring channel-gain events.
        let use_7_1 = source_has_back(fixed);
        self.trims.clear();
        self.trims.extend(
            fixed
                .iter()
                .map(|l| bed_entry_gain_db(key.placement.layout.as_ref(), *l, use_7_1)),
        );

        match plan_channel_render(
            key.mode,
            &key.labels,
            &key.placement.policy(&key.declared_poses),
            Some(&topology.speaker_layout),
            key.room,
            key.surround_placement,
        ) {
            ChannelRenderPlan::Events { events, routes } => {
                self.cache.apply_routes(renderer, routes);
                out.extend(events);
            }
            ChannelRenderPlan::HostPassthrough | ChannelRenderPlan::Silence => {
                self.cache.apply_routes(renderer, Vec::new());
            }
        }

        self.cache.key = Some(key);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    const UNIT_ROOM: [f32; 3] = [1.0, 1.0, 1.0];

    fn ratios(ratio: [f32; 3], rear: f32, lower: f32, center_blend: f32) -> RoomRatios {
        RoomRatios {
            ratio,
            rear,
            lower,
            center_blend,
        }
    }

    #[test]
    fn fixed_channel_catalog_covers_every_fixed_label_with_canonical_poses() {
        let catalog: serde_json::Value =
            serde_json::from_str(&fixed_channel_catalog_json()).expect("valid catalog JSON");
        let entries = catalog.as_array().expect("catalog array");
        let labels: Vec<&str> = entries
            .iter()
            .map(|entry| entry["label"].as_str().expect("label"))
            .collect();
        assert_eq!(
            labels,
            [
                "L", "R", "C", "LFE", "Ls", "Rs", "Lb", "Rb", "TFL", "TFR", "TBL", "TBR", "Lsc",
                "Rsc", "Cb", "Lsd", "Rsd", "Lw", "Rw", "LFE2", "TSL", "TSR", "TC", "TFC", "Lh",
                "Rh", "Ch", "Lhs", "Rhs",
            ]
        );

        let lfe = &entries[3];
        assert_eq!(lfe["group"], "floor");
        assert_eq!(lfe["spatialize"], false);
        for height in &entries[8..12] {
            assert_eq!(height["group"], "height");
            assert_eq!(height["z"], 1.0);
        }

        let entry = |label: &str| {
            entries
                .iter()
                .find(|entry| entry["label"] == label)
                .unwrap_or_else(|| panic!("missing {label}"))
        };
        let lfe2 = entry("LFE2");
        assert_eq!(lfe2["spatialize"], false);
        assert_eq!(lfe2["x"], 0.0);
        assert_eq!(lfe2["y"], 1.0);
        assert_eq!(lfe2["z"], 0.0);

        let tc = entry("TC");
        assert_eq!(tc["x"], 0.0);
        assert_eq!(tc["y"], 0.0);
        assert_eq!(tc["z"], 1.0);

        let tfc = entry("TFC");
        assert_eq!(tfc["group"], "height");
        assert_eq!(tfc["x"], 0.0);
        assert_eq!(tfc["y"], 1.0);
        assert_eq!(tfc["z"], 1.0);
        assert_eq!(tfc["azimuth"], 0.0);
        assert_eq!(tfc["elevation"], 45.0);

        // Every entry carries both the room corner (x/y/z) and the sphere
        // direction (azimuth/elevation): the height tier's corner is on the
        // wall above its floor speaker, its direction 30° up.
        let lhs = entry("Lhs");
        assert_eq!(lhs["group"], "height");
        assert_eq!(lhs["x"], -1.0);
        assert_eq!(lhs["y"], 0.0);
        assert!((lhs["z"].as_f64().unwrap() - HEIGHT_TIER_Z_WALL as f64).abs() < 1e-6);
        assert_eq!(lhs["azimuth"], -110.0);
        assert_eq!(lhs["elevation"], 30.0);
        for entry in entries {
            assert!(entry["azimuth"].is_number() && entry["elevation"].is_number());
            assert!(entry.get("coord_mode").is_none());
        }

        // Every entry carries its label's accepted spellings, so Studio can match
        // channel names with the renderer's own alias tolerance.
        for entry in entries {
            let label = entry["label"].as_str().expect("string label");
            let aliases = entry["aliases"].as_array().expect("aliases array");
            assert!(!aliases.is_empty(), "no aliases published for {}", label);
        }
        // Spot-check that the spellings actually resolve back to their own label.
        let tfl: Vec<&str> = entry("TFL")["aliases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(tfl.contains(&"TOPFRONTLEFT"));
        assert!(tfl.contains(&"HL"));
    }

    /// Bed entries as a family's placement layout holds them: any count,
    /// no triangulation requirement (unlike [`vbed`]).
    fn entries(speakers: Vec<renderer::speaker_layout::Speaker>) -> SpeakerLayout {
        SpeakerLayout {
            radius_m: 1.0,
            speakers,
        }
    }

    const FIXED_LABELS: [RChannelLabel; 29] = {
        use RChannelLabel::*;
        [
            L, R, C, LFE, LFE2, Ls, Rs, Lb, Rb, Cb, Lsc, Rsc, Lw, Rw, Lsd, Rsd, Tfl, Tfr, Tsl, Tsr,
            Tbl, Tbr, Tc, Tfc, Lh, Rh, Ch, Lhs, Rhs,
        ]
    };

    /// A bed entry is found by exactly the spellings the single source of
    /// truth (`bridge_api::labels`) gives its label — the table the output
    /// channel map and Studio's catalogue match with — whatever the case or
    /// the separators, and by no other label's. The one exception is the
    /// explicit 5.1 fold: a 4.x/5.x source's `Ls`/`Rs` read a back entry.
    #[test]
    fn bed_entries_match_the_so_t_spellings_both_ways() {
        use RChannelLabel::*;
        for entry_label in FIXED_LABELS {
            for alias in bridge_api::labels::aliases_for(entry_label) {
                // `ALIAS`, `alias` and `A_L_I_A_S`-style spellings alike.
                let spaced: String = alias
                    .chars()
                    .flat_map(|c| ['_', c.to_ascii_lowercase()])
                    .collect();
                for name in [alias.to_string(), alias.to_ascii_lowercase(), spaced] {
                    let bed = entries(vec![renderer::speaker_layout::Speaker::new(
                        &name, 0.0, 0.0,
                    )]);
                    for use_7_1 in [true, false] {
                        for label in FIXED_LABELS {
                            let folded =
                                !use_7_1 && matches!((label, entry_label), (Ls, Lb) | (Rs, Rb));
                            assert_eq!(
                                find_bed_entry(&bed, label, use_7_1).is_some(),
                                label == entry_label || folded,
                                "entry {name:?} ({entry_label:?}) looked up as {label:?}, \
                                 use_7_1={use_7_1}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// The bug: a bed entry spelled `Top_Front_Left` or `HL` resolved in the
    /// output channel map but not for the bed's own gain, spatialize flag or
    /// pose, which silently fell back to the defaults.
    #[test]
    fn bed_entry_spelled_another_way_still_reaches_the_plan() {
        use renderer::speaker_layout::Speaker;
        for name in ["Top_Front_Left", "HL", "top front left"] {
            let mut tfl = Speaker::new(name, -40.0, 20.0);
            tfl.gain_db = -4.5;
            tfl.spatialize = false;
            let bed = entries(vec![tfl]);
            let labels = [RChannelLabel::L, RChannelLabel::Tfl];
            let policy = PlacementPolicy::manual(&bed);
            assert_eq!(
                bed_entry_gain_db(Some(&bed), RChannelLabel::Tfl, false),
                -4.5
            );
            assert!(!channel_is_spatialized(
                Some(&bed),
                RChannelLabel::Tfl,
                false
            ));
            let (_, x, y, z) = resolve_virtual_bed_pose(
                RChannelLabel::Tfl,
                false,
                &policy,
                RoomRatios::UNIT,
                SurroundPlacement::Side,
            )
            .expect("pose");
            let corner = fallback_virtual_bed_pose(RChannelLabel::Tfl, false).expect("corner");
            assert_ne!(
                (x, y, z),
                (corner.1, corner.2, corner.3),
                "{name:?}: entry's own pose"
            );
            match plan_channel_render(
                renderer::live_params::ChannelRenderMode::Spatial,
                &labels,
                &policy,
                None,
                RoomRatios::UNIT,
                SurroundPlacement::Side,
            ) {
                ChannelRenderPlan::Events { events, routes } => {
                    assert_eq!(
                        routes[1],
                        renderer::spatial_renderer::ChannelRoute::Direct(RChannelLabel::Tfl)
                    );
                    assert_eq!(events[1].gain_db, Some(-4.5));
                }
                other => panic!("expected events, got {:?}", PlanKind::from(&other)),
            }
        }
    }

    /// The 5.1 fold prefers a surround entry to a back one wherever the back
    /// one sits in the bed, and a 7.x source never folds.
    #[test]
    fn five_one_surround_folds_onto_a_back_entry_only_as_a_fallback() {
        use renderer::speaker_layout::Speaker;
        let mut back = Speaker::new("BL", -135.0, 0.0);
        back.gain_db = -3.0;
        let mut side = Speaker::new("SL", -90.0, 0.0);
        side.gain_db = -1.0;
        let only_back = entries(vec![back.clone()]);
        let both = entries(vec![back, side]);
        assert_eq!(
            bed_entry_gain_db(Some(&only_back), RChannelLabel::Ls, false),
            -3.0
        );
        assert_eq!(
            bed_entry_gain_db(Some(&only_back), RChannelLabel::Ls, true),
            0.0
        );
        assert_eq!(
            bed_entry_gain_db(Some(&both), RChannelLabel::Ls, false),
            -1.0
        );
        assert_eq!(
            bed_entry_gain_db(Some(&both), RChannelLabel::Lb, false),
            -3.0
        );
    }

    #[test]
    fn fallback_pose_exists_for_every_non_object_label() {
        use RChannelLabel::*;
        let fixed = [
            L, R, C, LFE, Ls, Rs, Tfl, Tfr, Tsl, Tsr, Tbl, Tbr, Lsc, Rsc, Lb, Rb, Cb, Tc, Lsd, Rsd,
            Lw, Rw, Tfc, LFE2, Lh, Rh, Ch, Lhs, Rhs,
        ];
        for label in fixed {
            assert!(
                fallback_virtual_bed_pose(label, true).is_some(),
                "missing fallback for {label:?}"
            );
        }
        assert!(fallback_virtual_bed_pose(Object, true).is_none());
        assert!(fallback_virtual_bed_pose(Unknown, true).is_none());
    }

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
            &PlacementPolicy::room(),
            ratios(UNIT_ROOM, 1.0, 1.0, 0.0),
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
            &PlacementPolicy::room(),
            ratios(UNIT_ROOM, 1.0, 1.0, 0.0),
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
            &PlacementPolicy::room(),
            ratios(UNIT_ROOM, 1.0, 1.0, 0.0),
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
            &PlacementPolicy::room(),
            ratios(UNIT_ROOM, 1.0, 1.0, 0.0),
            SurroundPlacement::Side,
        )
        .unwrap();
        let objects = build_virtual_bed_objects(
            &labels,
            &PlacementPolicy::room(),
            None,
            ratios(UNIT_ROOM, 1.0, 1.0, 0.0),
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

    /// Azimuth/elevation the speaker stage (and the cascaded binaural mode,
    /// which pans through it) renders a normalized pose at, once the room
    /// warp has been applied to it.
    fn rendered_angles(pos: (f32, f32, f32), room: [f32; 3], rear: f32) -> (f32, f32) {
        let [px, py, pz] = omniphony_geometry::f32::room_scaled_position(
            [pos.0, pos.1, pos.2],
            room,
            rear,
            1.0,
            0.0,
        );
        (
            px.atan2(py).to_degrees(),
            pz.atan2((px * px + py * py).sqrt()).to_degrees(),
        )
    }

    /// In sphere mode the height tier renders at its nominal angles, and goes
    /// on doing so in a room that is not a cube: a corner-shaped pose is
    /// carried around by the depth warp, an angle is not. The second room is
    /// the engine's default.
    #[test]
    fn height_tier_sits_at_its_angles_in_any_room() {
        use RChannelLabel::{Ch, Lh, Lhs, Rh, Rhs};
        const TIER: [(RChannelLabel, f32, f32); 5] = [
            (Lh, -30.0, 30.0),
            (Rh, 30.0, 30.0),
            (Ch, 0.0, 30.0),
            (Lhs, -110.0, 30.0),
            (Rhs, 110.0, 30.0),
        ];
        for (room, rear) in [(UNIT_ROOM, 1.0f32), ([1.0, 2.0, 1.0], 2.0f32)] {
            for (label, want_az, want_el) in TIER {
                let (_, x, y, z) = resolve_virtual_bed_pose(
                    label,
                    true,
                    &PlacementPolicy::sphere(&[]),
                    ratios(room, rear, 1.0, 0.0),
                    SurroundPlacement::Side,
                )
                .unwrap_or_else(|| panic!("no pose for {label:?}"));
                let (az, el) = rendered_angles((x, y, z), room, rear);
                assert!(
                    (el - want_el).abs() < 0.05,
                    "{label:?} elevation in room {room:?}: want {want_el}, got {el}"
                );
                assert!(
                    (az - want_az).abs() < 0.05,
                    "{label:?} azimuth in room {room:?}: want {want_az}, got {az}"
                );
            }
        }
    }

    /// The three modes, on a 7.1 source's `Ls` in the engine's default room:
    /// sphere renders the declared angle (or the nominal one without a
    /// declaration), room renders the corner whatever was declared, manual
    /// renders the user's entry and ignores the declaration too.
    #[test]
    fn each_mode_places_ls_from_its_own_source() {
        let room = [1.0, 2.0, 1.0];
        let rear = 2.0;
        let declared = [RChannelPose {
            label: RChannelLabel::Ls,
            azimuth_deg: -110.0,
            elevation_deg: 0.0,
        }];
        // `use_7_1 = true`: no surround-placement override on Ls.
        let resolve = |policy: &PlacementPolicy<'_>| {
            let (_, x, y, z) = resolve_virtual_bed_pose(
                RChannelLabel::Ls,
                true,
                policy,
                ratios(room, rear, 1.0, 0.0),
                SurroundPlacement::Side,
            )
            .expect("Ls resolves");
            rendered_angles((x, y, z), room, rear)
        };

        let (az, el) = resolve(&PlacementPolicy::sphere(&declared));
        assert!((az + 110.0).abs() < 0.05, "sphere, declared: got {az}");
        assert!(el.abs() < 0.05, "sphere, declared: elevation {el}");

        // No declaration: the nominal angle of a 7.x side pair.
        let (az, _) = resolve(&PlacementPolicy::sphere(&[]));
        assert!((az + 90.0).abs() < 0.05, "sphere, nominal: got {az}");

        // Room: the side corner (-1, 0, 0), -90° in any room, declared or not.
        let (az, _) = resolve(&PlacementPolicy::room().with_declared(&declared));
        assert!((az + 90.0).abs() < 0.05, "room: got {az}");

        // Manual: the user's entry, declared or not.
        let bed = bed_with_ls_at(-135.0);
        let (az, _) = resolve(&PlacementPolicy::manual(&bed).with_declared(&declared));
        assert!((az + 135.0).abs() < 0.05, "manual: got {az}");

        // Manual without an entry for the label falls back to the room.
        let (az, _) = resolve(&PlacementPolicy::manual(&bed_with_l_at(-45.0)));
        assert!((az + 90.0).abs() < 0.05, "manual fallback: got {az}");
    }

    /// Azimuth and elevation the direct binaural stage reads off a normalized
    /// pose, straight off the coordinates with no room warp
    /// (`renderer::binaural`), and the pose's Euclidean radius.
    fn direct_binaural_angles(pos: (f32, f32, f32)) -> (f32, f32, f32) {
        let (x, y, z) = pos;
        let horizontal = (x * x + y * y).sqrt();
        (
            x.atan2(y).to_degrees(),
            z.atan2(horizontal).to_degrees(),
            (horizontal * horizontal + z * z).sqrt(),
        )
    }

    /// Azimuth/elevation a normalized pose renders at once warped with a
    /// whole room (all five ratios and the blend).
    fn rendered_angles_in(pos: (f32, f32, f32), room: RoomRatios) -> (f32, f32) {
        let [px, py, pz] = room.scale([pos.0, pos.1, pos.2]);
        (
            px.atan2(py).to_degrees(),
            pz.atan2((px * px + py * py).sqrt()).to_degrees(),
        )
    }

    /// A channel placed by angle keeps its angle in a measured room that is
    /// lower, or shorter to the rear, than its unit radius (#803): the
    /// height tier at 30° and the top tier at 45° render at those
    /// elevations through the room's warp, and so does a polar manual
    /// entry, where the clamping inverse bent them (30° read 24.8°).
    #[test]
    fn angle_poses_keep_their_angles_in_a_low_measured_room() {
        use RChannelLabel::{Ch, L, Lh, Lhs, Ls, Tbl, Tfl};
        use renderer::binaural::brir::MeasuredRoom;
        // The reviewer's room: 5 m wide, 2 m behind, 3 m ahead, a 1 m
        // ceiling over the ears and 1.2 m of floor.
        let room = MeasuredRoom {
            box_m: [[-2.5, -2.0, -1.2], [2.5, 3.0, 1.0]],
            estimated: false,
        }
        .ratios(0.5);
        assert!((room.ratio[2] - 0.4).abs() < 1e-6 && (room.rear - 0.8).abs() < 1e-6);
        let manual = bed_with_l_at(-30.0);
        let cases: [(RChannelLabel, bool, PlacementPolicy<'_>, f32, f32); 7] = [
            (Lh, false, PlacementPolicy::sphere(&[]), -30.0, 30.0),
            (Ch, false, PlacementPolicy::sphere(&[]), 0.0, 30.0),
            (Lhs, false, PlacementPolicy::sphere(&[]), -110.0, 30.0),
            (Tfl, true, PlacementPolicy::sphere(&[]), -45.0, 45.0),
            (Tbl, true, PlacementPolicy::sphere(&[]), -135.0, 45.0),
            (Ls, false, PlacementPolicy::sphere(&[]), -110.0, 0.0),
            (L, false, PlacementPolicy::manual(&manual), -30.0, 0.0),
        ];
        for (label, use_7_1, policy, want_az, want_el) in &cases {
            let (_, x, y, z) =
                resolve_virtual_bed_pose(*label, *use_7_1, policy, room, SurroundPlacement::Side)
                    .unwrap_or_else(|| panic!("no pose for {label:?}"));
            assert!(
                x.abs() <= 1.0 && y.abs() <= 1.0 && z.abs() <= 1.0,
                "{label:?}: inside the cube, got ({x}, {y}, {z})"
            );
            let (az, el) = rendered_angles_in((x, y, z), room);
            assert!(
                (az - want_az).abs() < 0.05 && (el - want_el).abs() < 0.05,
                "{label:?}: renders at {az:.2}/{el:.2}, stated {want_az}/{want_el}"
            );
        }
    }

    /// A channel placed by angle — a sphere direction or a polar manual entry
    /// — renders at that angle on every output, in the engine's default room
    /// too: pre-compensated for no warp on the direct binaural path, which
    /// applies none, and for the live room wherever the speaker stage does
    /// (#781). On the direct path it also sits on the unit sphere (Euclidean
    /// radius 1). That is not an equal distance cue: the binaural stage
    /// measures cues with the cube norm (`cue_distance_norm`), so C reads 1
    /// and L about 0.87.
    #[test]
    fn angle_poses_render_at_their_angles_on_every_output() {
        use RChannelLabel::{C, L, Lb, Ls, Tfl};
        use renderer::live_params::{BinauralMode, OutputMode};
        let renderer = small_renderer(SpeakerLayout::preset("7.1.4").expect("preset layout"));
        let control = renderer.renderer_control();
        let (room, rear) = {
            let live = control.live.read();
            (live.room_ratio, live.room_ratio_rear)
        };
        assert_ne!(room, UNIT_ROOM, "the engine's default room is not a cube");
        let manual = bed_with_l_at(-30.0);
        // (label, use_7_1, policy, azimuth, elevation)
        let cases: [(RChannelLabel, bool, PlacementPolicy<'_>, f32, f32); 6] = [
            (L, false, PlacementPolicy::sphere(&[]), -30.0, 0.0),
            (C, false, PlacementPolicy::sphere(&[]), 0.0, 0.0),
            (Ls, false, PlacementPolicy::sphere(&[]), -110.0, 0.0),
            (Lb, true, PlacementPolicy::sphere(&[]), -135.0, 0.0),
            (Tfl, true, PlacementPolicy::sphere(&[]), -45.0, 45.0),
            (L, false, PlacementPolicy::manual(&manual), -30.0, 0.0),
        ];
        let resolve = |label, use_7_1, policy: &PlacementPolicy<'_>| {
            let room = RoomRatios::for_output(&control.live.read(), &control.active_topology());
            let (_, x, y, z) =
                resolve_virtual_bed_pose(label, use_7_1, policy, room, SurroundPlacement::Side)
                    .unwrap_or_else(|| panic!("no pose for {label:?}"));
            (x, y, z)
        };

        control.live.write().binaural.output_mode = OutputMode::Binaural;
        for (label, use_7_1, policy, want_az, want_el) in &cases {
            let (az, el, dist) = direct_binaural_angles(resolve(*label, *use_7_1, policy));
            assert!(
                (az - want_az).abs() < 0.05,
                "direct {label:?}: azimuth {az}"
            );
            assert!(
                (el - want_el).abs() < 0.05,
                "direct {label:?}: elevation {el}"
            );
            assert!(
                (dist - 1.0).abs() < 1e-4,
                "direct {label:?}: Euclidean radius {dist}"
            );
        }

        // Cascaded, a BRIR source (which runs the cascade whatever the mode
        // says), and speakers: the speaker stage warps, so the pose is
        // pre-compensated for the live room.
        let outputs: [fn(&mut renderer::live_params::LiveParams); 3] = [
            |live| live.binaural.mode = BinauralMode::Cascaded,
            |live| {
                live.binaural.mode = BinauralMode::Direct;
                live.binaural.hrir_source =
                    renderer::binaural::HrirSource::Brir("room.sofa".into());
            },
            |live| live.binaural.output_mode = OutputMode::SpeakerArray,
        ];
        for (output, set) in outputs.iter().enumerate() {
            set(&mut control.live.write());
            for (label, use_7_1, policy, want_az, want_el) in &cases {
                let (az, el) = rendered_angles(resolve(*label, *use_7_1, policy), room, rear);
                assert!(
                    (az - want_az).abs() < 0.05,
                    "output {output} {label:?}: azimuth {az}"
                );
                assert!(
                    (el - want_el).abs() < 0.05,
                    "output {output} {label:?}: elevation {el}"
                );
            }
        }
    }

    /// The Side/Back choice is the room model's, for a source that has no
    /// back pair: it moves a 5.x `Ls` and the height above it, and leaves a
    /// sphere direction and a manual entry alone.
    #[test]
    fn surround_placement_only_moves_room_corners() {
        let resolve = |label: RChannelLabel, policy: &PlacementPolicy<'_>, placement| {
            let (_, x, y, z) =
                resolve_virtual_bed_pose(label, false, policy, RoomRatios::UNIT, placement)
                    .expect("resolves");
            (x, y, z)
        };
        use SurroundPlacement::{Back, Side};
        assert_eq!(
            resolve(RChannelLabel::Ls, &PlacementPolicy::room(), Back),
            (-1.0, -1.0, 0.0)
        );
        assert_eq!(
            resolve(RChannelLabel::Lhs, &PlacementPolicy::room(), Back),
            (-1.0, -1.0, HEIGHT_TIER_Z_CORNER)
        );
        assert_eq!(
            resolve(RChannelLabel::Lhs, &PlacementPolicy::room(), Side),
            (-1.0, 0.0, HEIGHT_TIER_Z_WALL)
        );
        // A sphere direction is not a corner: -110° stays -110° in both.
        let sphere_side = resolve(RChannelLabel::Ls, &PlacementPolicy::sphere(&[]), Side);
        let sphere_back = resolve(RChannelLabel::Ls, &PlacementPolicy::sphere(&[]), Back);
        assert_eq!(sphere_side, sphere_back);
        let (az, _) = rendered_angles(sphere_side, UNIT_ROOM, 1.0);
        assert!((az + 110.0).abs() < 0.05, "sphere Ls: got {az}");
        // A manual entry is the user's word.
        let bed = bed_with_ls_at(-100.0);
        let manual_side = resolve(RChannelLabel::Ls, &PlacementPolicy::manual(&bed), Side);
        let manual_back = resolve(RChannelLabel::Ls, &PlacementPolicy::manual(&bed), Back);
        assert_eq!(manual_side, manual_back);
        let (az, _) = rendered_angles(manual_side, UNIT_ROOM, 1.0);
        assert!((az + 100.0).abs() < 0.05, "manual Ls: got {az}");
    }

    fn bed_with_ls_at(azimuth: f32) -> SpeakerLayout {
        use renderer::speaker_layout::Speaker;
        SpeakerLayout::from_speakers(vec![
            Speaker::new("L", -30.0, 0.0),
            Speaker::new("R", 30.0, 0.0),
            Speaker::new("Ls", azimuth, 0.0),
        ])
        .expect("valid virtual bed")
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
            &PlacementPolicy::room(),
            Some(&output),
            ratios(UNIT_ROOM, 1.0, 1.0, 0.0),
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
        center.gain_db = -6.0;
        let bed = vbed(vec![
            Speaker::new("L", -30.0, 0.0),
            center,
            Speaker::new("R", 30.0, 0.0),
        ]);
        let labels = [RChannelLabel::L, RChannelLabel::C, RChannelLabel::LFE];
        let objects = build_virtual_bed_objects(
            &labels,
            &PlacementPolicy::manual(&bed),
            None,
            ratios(UNIT_ROOM, 1.0, 1.0, 0.0),
            SurroundPlacement::Side,
        )
        .expect("all channels emitted");
        let c = objects
            .iter()
            .find(|o| o.name.eq_ignore_ascii_case("C"))
            .expect("C object present");
        assert_eq!(c.gain, -6.0, "C gain comes from the virtual bed");
        for obj in objects.iter().filter(|o| !o.name.eq_ignore_ascii_case("C")) {
            assert_eq!(obj.gain, 0.0, "{} has no configured gain", obj.name);
        }
    }

    #[test]
    fn bed_entry_gain_reaches_the_audio_events() {
        // The display objects above are not what the renderer consumes: the
        // audio events are. A bed gain that only reaches `ObjectMeta.gain`
        // shows a trimmed channel in Studio while rendering it at unity —
        // exactly the bug reported in #220. Both event kinds must carry it:
        // a spatialized channel (C) and a direct-routed one (LFE).
        use renderer::speaker_layout::Speaker;
        let mut c = Speaker::new("C", 0.0, 0.0);
        c.gain_db = -6.5;
        let mut lfe = Speaker::new("LFE", 45.0, -10.0);
        lfe.gain_db = -6.5;
        lfe.spatialize = false;
        let bed = vbed(vec![
            Speaker::new("L", -30.0, 0.0),
            c,
            Speaker::new("R", 30.0, 0.0),
            lfe,
        ]);
        let labels = [
            RChannelLabel::L,
            RChannelLabel::C,
            RChannelLabel::R,
            RChannelLabel::LFE,
        ];

        let plan = plan_channel_render(
            renderer::live_params::ChannelRenderMode::Spatial,
            &labels,
            &PlacementPolicy::manual(&bed),
            None,
            ratios(UNIT_ROOM, 1.0, 1.0, 0.0),
            SurroundPlacement::Side,
        );
        let ChannelRenderPlan::Events { events, .. } = plan else {
            panic!("expected Events");
        };
        let gain_of = |idx: usize| {
            events
                .iter()
                .find(|e| e.channel_idx == idx)
                .expect("event")
                .gain_db
        };

        assert_eq!(gain_of(1), Some(-6.5), "spatialized C carries the bed gain");
        assert_eq!(
            gain_of(3),
            Some(-6.5),
            "direct-routed LFE carries the bed gain"
        );
        assert_eq!(gain_of(0), Some(0.0), "unset entries stay at unity");
    }

    #[test]
    fn bed_entry_gain_clamps_into_the_event_domain() {
        // The entry gain is an `i32`, the event gain an `i8` whose −128 means
        // −inf. A bare cast would wrap −200 dB into +56 dB; it must clamp to
        // the −inf sentinel instead (and symmetrically on the positive side).
        use renderer::speaker_layout::Speaker;
        let mut c = Speaker::new("C", 0.0, 0.0);
        c.gain_db = -200.0;
        let bed = vbed(vec![
            Speaker::new("L", -30.0, 0.0),
            c,
            Speaker::new("R", 30.0, 0.0),
        ]);
        let plan = plan_channel_render(
            renderer::live_params::ChannelRenderMode::Spatial,
            &[RChannelLabel::L, RChannelLabel::C, RChannelLabel::R],
            &PlacementPolicy::manual(&bed),
            None,
            ratios(UNIT_ROOM, 1.0, 1.0, 0.0),
            SurroundPlacement::Side,
        );
        let ChannelRenderPlan::Events { events, .. } = plan else {
            panic!("expected Events");
        };
        let c_event = events
            .iter()
            .find(|e| e.channel_idx == 1)
            .expect("C event present");
        assert_eq!(
            c_event.gain_db,
            Some(renderer::spatial_renderer::GAIN_DB_NEG_INF),
            "clamped to the −inf floor"
        );
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
            &PlacementPolicy::manual(&bed),
            None,
            ratios(room, 1.0, 1.0, 0.5),
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
            &PlacementPolicy::manual(&bed),
            None,
            ratios(room, 1.0, 1.0, 0.5),
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
            &PlacementPolicy::room(),
            None,
            ratios(UNIT_ROOM, 1.0, 1.0, 0.0),
            SurroundPlacement::Side,
        )
        .unwrap();
        let back = build_virtual_bed_objects(
            &labels,
            &PlacementPolicy::room(),
            None,
            ratios(UNIT_ROOM, 1.0, 1.0, 0.0),
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
            &PlacementPolicy::room(),
            None,
            ratios(UNIT_ROOM, 1.0, 1.0, 0.0),
            SurroundPlacement::Side,
        )
        .unwrap();
        let back = build_virtual_bed_objects(
            &labels,
            &PlacementPolicy::room(),
            None,
            ratios(UNIT_ROOM, 1.0, 1.0, 0.0),
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
            &PlacementPolicy::room(),
            None,
            ratios(UNIT_ROOM, 1.0, 1.0, 0.0),
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
            &PlacementPolicy::room(),
            None,
            ratios(UNIT_ROOM, 1.0, 1.0, 0.0),
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
            &PlacementPolicy::manual(&bed),
            None,
            ratios(UNIT_ROOM, 1.0, 1.0, 0.0),
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
            &PlacementPolicy::manual(&bed),
            None,
            ratios(UNIT_ROOM, 1.0, 1.0, 0.0),
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

    // ── What `BedChannelPlanner`'s cache key has to cover ─────────────────
    //
    // The planner reuses a plan until one of its inputs changes, so each of
    // these pins an input that genuinely moves the output: drop it from the key
    // and the bed silently stops following that control.

    use renderer::speaker_layout::Speaker;

    /// Positions of a spatial plan, in channel order. `None` for a direct
    /// (one-hot) channel, which carries no position.
    fn planned_positions(plan: &ChannelRenderPlan) -> Vec<Option<[f64; 3]>> {
        match plan {
            ChannelRenderPlan::Events { events, .. } => events.iter().map(|e| e.position).collect(),
            other => panic!("expected a spatial plan, got {:?}", PlanKind::from(other)),
        }
    }

    fn plan_bed(bed: Option<&SpeakerLayout>, room_ratio: [f32; 3]) -> ChannelRenderPlan {
        plan_channel_render(
            renderer::live_params::ChannelRenderMode::Spatial,
            &BED_5_1,
            &bed.map_or(PlacementPolicy::room(), PlacementPolicy::manual),
            None,
            ratios(room_ratio, 1.0, 1.0, 0.0),
            SurroundPlacement::Side,
        )
    }

    /// A bed whose L sits at an intermediate depth. The canonical fallback poses
    /// all sit on the unit cube's faces, where the room-ratio depth warp is the
    /// identity — only an off-axis pose shows the warp at all.
    fn bed_with_l_at(azimuth: f32) -> SpeakerLayout {
        vbed(vec![
            Speaker::new("L", azimuth, 0.0),
            Speaker::new("R", 30.0, 0.0),
            Speaker::new("C", 0.0, 0.0),
        ])
    }

    /// The premise of caching: the plan is a pure function of its inputs, so
    /// replanning an unchanged frame can only reproduce the same answer.
    #[test]
    fn identical_inputs_plan_identically() {
        let bed = bed_with_l_at(-45.0);
        let first = plan_bed(Some(&bed), UNIT_ROOM);
        let second = plan_bed(Some(&bed), UNIT_ROOM);
        assert_eq!(planned_positions(&first), planned_positions(&second));
        match (&first, &second) {
            (
                ChannelRenderPlan::Events { routes: a, .. },
                ChannelRenderPlan::Events { routes: b, .. },
            ) => assert_eq!(a, b),
            _ => panic!("expected two spatial plans"),
        }
    }

    /// Editing the virtual bed moves a channel — and nothing bumps a generation
    /// counter when it happens, so the key must compare the bed itself.
    #[test]
    fn virtual_bed_edit_moves_a_planned_channel() {
        assert_ne!(
            planned_positions(&plan_bed(Some(&bed_with_l_at(-30.0)), UNIT_ROOM)),
            planned_positions(&plan_bed(Some(&bed_with_l_at(-110.0)), UNIT_ROOM)),
            "moving L in the virtual bed must move the planned object"
        );
    }

    /// Same for the room ratios, which warp the depth of every off-axis channel.
    #[test]
    fn room_ratio_change_moves_a_planned_channel() {
        let bed = bed_with_l_at(-45.0);
        assert_ne!(
            planned_positions(&plan_bed(Some(&bed), UNIT_ROOM)),
            planned_positions(&plan_bed(Some(&bed), [1.0, 2.5, 1.0])),
            "a deeper room must move the off-axis objects"
        );
    }

    /// A live placement edit — an entry, or the family's mode — reaches a
    /// running object stream on the next frame: the fixed-prefix planner
    /// compares the family's effective placement by value, like the bed
    /// planner, so it does not depend on the OSC handler remembering to bump
    /// the options epoch. An unchanged frame stays cached.
    #[test]
    fn live_placement_edit_replans_an_object_stream_prefix() {
        use renderer::placement::PlacementMode;
        use renderer::speaker_layout::Speaker;
        let renderer = crate::renderer_build::build_spatial_renderer(
            &crate::renderer_build::SpatialRendererParams::from_render_config(None),
            SpeakerLayout::preset("7.1.4").expect("preset layout"),
            48_000,
            bridge_api::RVbapCartesianDefaults {
                x_size: 9,
                y_size: 9,
                z_size: 5,
                z_neg_size: 0,
                allow_negative_z: true,
            },
            bridge_api::RVbapTableMode::Cartesian,
            None,
        )
        .expect("renderer");
        let control = renderer.renderer_control();
        let dolby = test_family(&control, "dolby");
        let dts = test_family(&control, "dts");
        let labels = [
            RChannelLabel::L,
            RChannelLabel::R,
            RChannelLabel::C,
            RChannelLabel::LFE,
            RChannelLabel::Object,
        ];

        let mut planner = FixedChannelPlanner::new();
        let mut out = Vec::new();
        planner.plan_object_stream_fixed(&labels, dolby, &[], &renderer, &mut out);
        assert!(!out.is_empty(), "initial plan emits the prefix events");
        assert_eq!(
            planner.fixed_trims(),
            &[0.0, 0.0, 0.0, 0.0],
            "no entries → unity trims"
        );
        let l_room = out
            .iter()
            .find(|e| e.channel_idx == 0)
            .and_then(|e| e.position)
            .expect("L event");

        out.clear();
        planner.plan_object_stream_fixed(&labels, dolby, &[], &renderer, &mut out);
        assert!(out.is_empty(), "nothing changed → cached plan");

        // The edit: LFE trimmed to −6.5 dB in the generic entries, which the
        // Dolby family inherits.
        let mut lfe = Speaker::new("LFE", 45.0, -10.0);
        lfe.gain_db = -6.5;
        lfe.spatialize = false;
        control
            .live
            .write()
            .placement
            .family_mut(SourceFamily::GENERIC)
            .layout = Some(vbed(vec![
            Speaker::new("L", -30.0, 0.0),
            Speaker::new("C", 0.0, 0.0),
            Speaker::new("R", 30.0, 0.0),
            lfe,
        ]));

        out.clear();
        planner.plan_object_stream_fixed(&labels, dolby, &[], &renderer, &mut out);
        let lfe_event = out
            .iter()
            .find(|e| e.channel_idx == 3)
            .expect("LFE event after replan, without any epoch bump");
        assert_eq!(
            lfe_event.gain_db,
            Some(-6.5),
            "replanned event carries the trim"
        );
        assert_eq!(
            planner.fixed_trims(),
            &[0.0, 0.0, 0.0, -6.5],
            "trims exposed for the stream-gain sum"
        );
        // Room mode: the entries' poses are not used, L is still the corner.
        let l_after = out
            .iter()
            .find(|e| e.channel_idx == 0)
            .and_then(|e| e.position)
            .expect("L event");
        assert_eq!(l_after, l_room, "room mode ignores the entry's pose");

        // Switching the family to manual replans too, and now L is the entry.
        control.live.write().placement.family_mut(dolby).mode = Some(PlacementMode::Manual);
        out.clear();
        planner.plan_object_stream_fixed(&labels, dolby, &[], &renderer, &mut out);
        let l_manual = out
            .iter()
            .find(|e| e.channel_idx == 0)
            .and_then(|e| e.position)
            .expect("L event after the mode change");
        assert_ne!(l_manual, l_room, "manual mode places L at the entry");

        // Another family is another plan, even with the same labels.
        out.clear();
        planner.plan_object_stream_fixed(&labels, dts, &[], &renderer, &mut out);
        assert!(!out.is_empty(), "a family change replans");
    }

    /// A family as a bridge's catalogue declares it (the renderer knows none
    /// by name): `auro` a sphere, the rest a room.
    pub(crate) fn test_family(
        control: &renderer::live_params::RendererControl,
        name: &str,
    ) -> SourceFamily {
        let mode = if name == "auro" {
            PlacementMode::Sphere
        } else {
            PlacementMode::Room
        };
        let mut live = control.live.write();
        live.placement.declare(name, name, mode);
        live.placement.find(name).expect("declared")
    }

    fn small_renderer(layout: SpeakerLayout) -> renderer::spatial_renderer::SpatialRenderer {
        crate::renderer_build::build_spatial_renderer(
            &crate::renderer_build::SpatialRendererParams::from_render_config(None),
            layout,
            48_000,
            bridge_api::RVbapCartesianDefaults {
                x_size: 9,
                y_size: 9,
                z_size: 5,
                z_neg_size: 0,
                allow_negative_z: true,
            },
            bridge_api::RVbapTableMode::Cartesian,
            None,
        )
        .expect("renderer")
    }

    fn event_position(
        events: &[renderer::spatial_renderer::SpatialChannelEvent],
        channel_idx: usize,
    ) -> Option<[f64; 3]> {
        events
            .iter()
            .find(|e| e.channel_idx == channel_idx)
            .and_then(|e| e.position)
    }

    /// A room-ratio edit moves the fixed prefix of a running object stream.
    ///
    /// Sphere mode places a channel at an angle *under the room in force*, so
    /// its normalized pose depends on the ratios. The room OSC handler bumps
    /// the geometry generation, not the options epoch the fixed planner used
    /// to cache on: the prefix kept the old room's poses until the next track.
    #[test]
    fn room_ratio_edit_replans_an_object_stream_prefix() {
        use renderer::placement::PlacementMode;
        let renderer = small_renderer(SpeakerLayout::preset("7.1.4").expect("preset layout"));
        let control = renderer.renderer_control();
        let dolby = test_family(&control, "dolby");
        control.live.write().placement.family_mut(dolby).mode = Some(PlacementMode::Sphere);
        let labels = [
            RChannelLabel::L,
            RChannelLabel::R,
            RChannelLabel::C,
            RChannelLabel::LFE,
            RChannelLabel::Object,
        ];

        let mut planner = FixedChannelPlanner::new();
        let mut out = Vec::new();
        planner.plan_object_stream_fixed(&labels, dolby, &[], &renderer, &mut out);
        let l_cube = event_position(&out, 0).expect("L event");

        // What the room OSC handler does: new ratios, then a geometry bump,
        // and the recompute lands a topology whose speakers are placed in
        // the new room. That topology's room is the one the plan reads
        // (#803), keyed by the generation it carries.
        {
            let mut live = control.live.write();
            live.room_ratio[1] *= 2.0;
            live.room_ratio_rear *= 2.0;
        }
        control.bump_geometry_generation();
        out.clear();
        planner.plan_object_stream_fixed(&labels, dolby, &[], &renderer, &mut out);
        assert!(
            out.is_empty(),
            "until the rebuild lands, the stage still pans in the old room → cached plan"
        );
        let plan = control.prepare_topology_rebuild().expect("rebuild plan");
        control.publish_topology(plan.build_topology().expect("topology"));
        planner.plan_object_stream_fixed(&labels, dolby, &[], &renderer, &mut out);
        let l_deep = event_position(&out, 0).expect("L event after the room edit");
        assert_ne!(l_deep, l_cube, "a deeper room moves the sphere-mode L");

        out.clear();
        planner.plan_object_stream_fixed(&labels, dolby, &[], &renderer, &mut out);
        assert!(out.is_empty(), "nothing changed since → cached plan");
    }

    /// Switching the output to headphones moves a family nobody chose a mode
    /// for from its room corners to the sphere, on the next frame and without
    /// an epoch bump; a family with a mode of its own stays where it was.
    #[test]
    fn headphones_replan_a_default_family_onto_the_sphere() {
        use renderer::live_params::OutputMode;
        use renderer::placement::PlacementMode;
        let renderer = small_renderer(SpeakerLayout::preset("7.1.4").expect("preset layout"));
        let control = renderer.renderer_control();
        let dolby = test_family(&control, "dolby");
        let dts = test_family(&control, "dts");
        control.live.write().placement.family_mut(dts).mode = Some(PlacementMode::Room);
        let labels = [
            RChannelLabel::L,
            RChannelLabel::R,
            RChannelLabel::C,
            RChannelLabel::LFE,
            RChannelLabel::Object,
        ];
        let plan = |planner: &mut FixedChannelPlanner, family| {
            let mut out = Vec::new();
            planner.plan_object_stream_fixed(&labels, family, &[], &renderer, &mut out);
            out
        };

        let mut dolby_planner = FixedChannelPlanner::new();
        let mut dts_planner = FixedChannelPlanner::new();
        let l_room = event_position(&plan(&mut dolby_planner, dolby), 0).expect("L event");
        let dts_l = event_position(&plan(&mut dts_planner, dts), 0).expect("initial DTS L");

        control.live.write().binaural.output_mode = OutputMode::Binaural;
        let l_headphones =
            event_position(&plan(&mut dolby_planner, dolby), 0).expect("L event after the switch");
        assert_ne!(l_headphones, l_room, "the default family left the room");
        control.live.write().placement.family_mut(dolby).mode = Some(PlacementMode::Sphere);
        assert!(
            plan(&mut dolby_planner, dolby).is_empty(),
            "the default on headphones is the sphere itself → cached plan"
        );
        // The output's room warp is part of the plan key (the direct path
        // applies none), so the room family may replan, but onto its corner.
        let dts_after = plan(&mut dts_planner, dts);
        assert!(
            dts_after.is_empty() || event_position(&dts_after, 0) == Some(dts_l),
            "a chosen room is not the output's to change"
        );
        assert!(plan(&mut dts_planner, dts).is_empty(), "then a cached plan");
    }

    /// Switching the binaural stage between its direct and cascaded paths
    /// replans a sphere-mode prefix on the next frame: the two warp the room
    /// differently, so the same angle is a different normalized pose (#781).
    #[test]
    fn binaural_path_switch_replans_a_sphere_prefix() {
        use renderer::live_params::{BinauralMode, OutputMode};
        let renderer = small_renderer(SpeakerLayout::preset("7.1.4").expect("preset layout"));
        let control = renderer.renderer_control();
        let dolby = test_family(&control, "dolby");
        control.live.write().binaural.output_mode = OutputMode::Binaural;
        let labels = [
            RChannelLabel::L,
            RChannelLabel::R,
            RChannelLabel::C,
            RChannelLabel::LFE,
            RChannelLabel::Object,
        ];
        let mut planner = FixedChannelPlanner::new();
        let mut plan = || {
            let mut out = Vec::new();
            planner.plan_object_stream_fixed(&labels, dolby, &[], &renderer, &mut out);
            out
        };
        let pose = |p: [f64; 3]| (p[0] as f32, p[1] as f32, p[2] as f32);

        let l_direct = event_position(&plan(), 0).expect("L event");
        let (az, ..) = direct_binaural_angles(pose(l_direct));
        assert!((az + 30.0).abs() < 0.05, "direct L: azimuth {az}");

        control.live.write().binaural.mode = BinauralMode::Cascaded;
        let l_cascaded = event_position(&plan(), 0).expect("L event after the switch");
        assert_ne!(
            l_cascaded, l_direct,
            "the cascade warps the room → new pose"
        );
        assert!(plan().is_empty(), "nothing changed since → cached plan");
    }

    /// A route that depends on the output layout — here `LFE2`, which folds
    /// onto the `LFE` sub until the layout has a speaker of its own — follows
    /// a layout edit once the rebuilt topology lands, in both planners.
    ///
    /// The edit bumps the control's geometry generation *before* the rebuild
    /// runs; a frame planned in that window still sees the old layout. Keyed
    /// on the control's counter, that frame's plan (old routes) was cached
    /// for good; keyed on the active topology's, the landing replans.
    #[test]
    fn layout_rebuild_reroutes_both_planners_once_it_lands() {
        use renderer::spatial_renderer::ChannelRoute;
        let layout = SpeakerLayout::preset("7.1.4").expect("preset layout");
        let renderer = small_renderer(layout.clone());
        let control = renderer.renderer_control();
        let dolby = test_family(&control, "dolby");
        let prefix = [
            RChannelLabel::L,
            RChannelLabel::R,
            RChannelLabel::C,
            RChannelLabel::LFE,
            RChannelLabel::LFE2,
        ];
        let object_labels = [&prefix[..], &[RChannelLabel::Object]].concat();

        let mut fixed = FixedChannelPlanner::new();
        let mut bed = BedChannelPlanner::new();
        let mut out = Vec::new();
        let plan_both =
            |fixed: &mut FixedChannelPlanner, bed: &mut BedChannelPlanner, out: &mut Vec<_>| {
                fixed.plan_object_stream_fixed(&object_labels, dolby, &[], &renderer, out);
                assert_eq!(
                    bed.plan(&renderer, &prefix, dolby, &[]),
                    BedPlanKind::Events
                );
                (
                    fixed.cache.applied_routes.clone().expect("fixed routes"),
                    bed.cache.applied_routes.clone().expect("bed routes"),
                )
            };

        let (fixed_routes, bed_routes) = plan_both(&mut fixed, &mut bed, &mut out);
        assert_eq!(fixed_routes[4], ChannelRoute::Direct(RChannelLabel::LFE));
        assert_eq!(bed_routes[4], ChannelRoute::Direct(RChannelLabel::LFE));

        // The edit is requested: generation bumped, rebuild not landed yet.
        control.bump_geometry_generation();
        plan_both(&mut fixed, &mut bed, &mut out);

        // The rebuild lands with an LFE2 speaker.
        let mut with_lfe2 = layout;
        let mut lfe2 = with_lfe2
            .speakers
            .iter()
            .find(|s| s.name == "LFE")
            .expect("preset LFE")
            .clone();
        lfe2.name = "LFE2".to_string();
        with_lfe2.speakers.push(lfe2);
        let room = RoomRatios::of_live(&control.live.read());
        let plan = control
            .prepare_topology_rebuild_for_layout(with_lfe2, room)
            .expect("rebuild plan");
        control.publish_topology(plan.build_topology().expect("topology"));

        let (fixed_routes, bed_routes) = plan_both(&mut fixed, &mut bed, &mut out);
        assert_eq!(fixed_routes[4], ChannelRoute::Direct(RChannelLabel::LFE2));
        assert_eq!(bed_routes[4], ChannelRoute::Direct(RChannelLabel::LFE2));
    }

    /// The bed planner hands the stages the poses its own events carry, for
    /// every channel (direct ones too), under the family's policy.
    #[test]
    fn bed_planner_publishes_the_poses_of_its_plan() {
        use renderer::placement::PlacementMode;
        let renderer = small_renderer(SpeakerLayout::preset("7.1.4").expect("preset layout"));
        let control = renderer.renderer_control();
        let dts = test_family(&control, "dts");
        control.live.write().placement.family_mut(dts).mode = Some(PlacementMode::Sphere);
        let mut planner = BedChannelPlanner::new();
        assert_eq!(
            planner.plan(&renderer, &BED_5_1, dts, &[]),
            BedPlanKind::Events
        );
        let poses = planner.poses().to_vec();
        assert_eq!(poses.len(), BED_5_1.len());
        assert!(poses.iter().all(Option::is_some), "LFE included: {poses:?}");
        for event in planner.events() {
            if let Some(position) = event.position {
                assert_eq!(poses[event.channel_idx], Some(position));
            }
        }

        control.live.write().channel_render_mode = renderer::live_params::ChannelRenderMode::Host;
        assert_eq!(
            planner.plan(&renderer, &BED_5_1, dts, &[]),
            BedPlanKind::HostPassthrough
        );
        assert!(planner.poses().is_empty());
    }

    /// The bed comparison is a derived `PartialEq`; if it ever stopped looking
    /// at the speakers, every cached plan would go stale without a symptom.
    #[test]
    fn layout_equality_detects_a_moved_speaker() {
        let bed = bed_with_l_at(-30.0);
        assert_eq!(bed, bed.clone());
        assert_ne!(bed, bed_with_l_at(-31.0));
        assert_ne!(
            Some(bed),
            None,
            "resetting the bed to defaults must not compare equal to having one"
        );
    }
}
