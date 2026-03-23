use crate::runtime_osc::ObjectMeta;
use bridge_api::RChannelLabel;
use renderer::speaker_layout::SpeakerLayout;
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
    let mut candidates: Vec<PathBuf> = vec![
        PathBuf::from("layouts").join(file_name),
        PathBuf::from("omniphony").join("layouts").join(file_name),
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("layouts")
            .join(file_name),
    ];
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

fn find_speaker_in_layout(
    layout: &SpeakerLayout,
    aliases: &[&str],
) -> Option<(String, f32, f32, f32)> {
    for speaker in &layout.speakers {
        if aliases
            .iter()
            .any(|alias| speaker.name.eq_ignore_ascii_case(alias))
        {
            return Some((
                speaker.name.clone(),
                speaker.azimuth,
                speaker.elevation,
                speaker.distance,
            ));
        }
    }
    None
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
        _ => None,
    }
}

fn fallback_virtual_bed_pose(
    label: RChannelLabel,
    use_7_1: bool,
) -> Option<(String, f32, f32, f32)> {
    let (name, az, el, dist) = match label {
        RChannelLabel::L => ("FL", if use_7_1 { -26.0 } else { -30.0 }, 0.0, 2.0),
        RChannelLabel::R => ("FR", if use_7_1 { 26.0 } else { 30.0 }, 0.0, 2.0),
        RChannelLabel::C => ("C", 0.0, 0.0, 2.0),
        RChannelLabel::LFE | RChannelLabel::LFE2 => ("LFE", 0.0, 0.0, 1.0),
        RChannelLabel::Ls => ("SL", if use_7_1 { -100.0 } else { -110.0 }, 0.0, 1.0),
        RChannelLabel::Rs => ("SR", if use_7_1 { 100.0 } else { 110.0 }, 0.0, 1.0),
        RChannelLabel::Lb => ("BL", -142.5, 0.0, 1.0),
        RChannelLabel::Rb => ("BR", 142.5, 0.0, 1.0),
        RChannelLabel::Cb => ("BC", 180.0, 0.0, 1.0),
        _ => return None,
    };
    Some((name.to_string(), az, el, dist))
}

fn resolve_virtual_bed_pose(
    label: RChannelLabel,
    use_7_1: bool,
) -> Option<(String, f32, f32, f32)> {
    let layouts = virtual_bed_layouts();
    let layout_opt = if use_7_1 {
        layouts.layout_7_1.as_ref()
    } else {
        layouts.layout_5_1.as_ref()
    };

    if let (Some(layout), Some(aliases)) = (layout_opt, label_aliases(label, use_7_1)) {
        if let Some(found) = find_speaker_in_layout(layout, aliases) {
            return Some(found);
        }
    }

    fallback_virtual_bed_pose(label, use_7_1)
}

pub fn build_virtual_bed_events(
    channel_labels: &[RChannelLabel],
    room_ratio: [f32; 3],
    room_ratio_rear: f32,
    room_ratio_lower: f32,
    room_ratio_center_blend: f32,
) -> Option<Vec<renderer::spatial_renderer::SpatialChannelEvent>> {
    let has_back = channel_labels
        .iter()
        .any(|l| matches!(l, RChannelLabel::Lb | RChannelLabel::Rb | RChannelLabel::Cb));
    let use_7_1 = has_back;

    let mut events: Vec<renderer::spatial_renderer::SpatialChannelEvent> =
        Vec::with_capacity(channel_labels.len());

    for (channel_idx, label) in channel_labels.iter().enumerate() {
        let (_name, az_deg, el_deg, dist_m) = match resolve_virtual_bed_pose(*label, use_7_1) {
            Some(v) => v,
            None => continue,
        };

        let (sx, sy, sz) = renderer::spatial_vbap::spherical_to_adm(az_deg, el_deg, dist_m);
        let (x, y, z) = inverse_room_ratio_map_for_virtual_object(
            sx,
            sy,
            sz,
            room_ratio,
            room_ratio_rear,
            room_ratio_lower,
            room_ratio_center_blend,
        );
        events.push(renderer::spatial_renderer::SpatialChannelEvent {
            channel_idx,
            is_bed: false,
            gain_db: Some(0),
            ramp_length: Some(0),
            spread: None,
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
    room_ratio: [f32; 3],
    room_ratio_rear: f32,
    room_ratio_lower: f32,
    room_ratio_center_blend: f32,
) -> Option<Vec<ObjectMeta>> {
    let has_back = channel_labels
        .iter()
        .any(|l| matches!(l, RChannelLabel::Lb | RChannelLabel::Rb | RChannelLabel::Cb));
    let use_7_1 = has_back;

    let mut objects: Vec<ObjectMeta> = Vec::with_capacity(channel_labels.len());
    for label in channel_labels {
        let (name, az_deg, el_deg, dist_m) = match resolve_virtual_bed_pose(*label, use_7_1) {
            Some(v) => v,
            None => continue,
        };
        let (sx, sy, sz) = renderer::spatial_vbap::spherical_to_adm(az_deg, el_deg, dist_m);
        let (x, y, z) = inverse_room_ratio_map_for_virtual_object(
            sx,
            sy,
            sz,
            room_ratio,
            room_ratio_rear,
            room_ratio_lower,
            room_ratio_center_blend,
        );
        objects.push(ObjectMeta {
            name,
            x,
            y,
            z,
            coord_mode: "cartesian".to_string(),
            direct_speaker_index: None,
            gain: 0,
            priority: 0.0,
            divergence: 0.0,
        });
    }
    if objects.is_empty() {
        None
    } else {
        Some(objects)
    }
}
