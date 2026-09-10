//! Derives what the viewport draws from the live model, following the
//! Studio's `sources.js`, `speakers.js`, `scene/*.js`, `trails.js` and
//! `coordinates.js` rule for rule (see the phase 1 specs). Everything here is
//! CPU-side: it produces a `FrameData` for the renderer plus the screen-space
//! labels egui draws on top, and the pick lists the app uses for selection.

pub mod objects;
pub mod room;
pub mod speakers;
pub mod trails;
pub mod volumes;

use std::time::Instant;

use egui::{Pos2, Rect};
use glam::{Mat4, Quat, Vec3, Vec4};

use crate::model::app_state::RoomRatio;
use crate::osc::dispatch::Live;
use crate::render::camera::OrbitCamera;
use crate::render::{FrameData, MeshInstance, MeshItem, MeshKind, hex_linear, with_alpha};

pub use objects::{ObjectDisplayMode, SpeakerRef};
pub use room::RoomBounds;
pub use trails::{TrailMode, TrailSettings};
pub use volumes::{VolumeSettings, VolumeState};

/// Display toggles the Studio persists as `spatialviz.effective_render_prefs`
/// and `spatialviz.trail_prefs`.
#[derive(Clone, Debug)]
pub struct ViewSettings {
    pub objects_visible: bool,
    pub object_display_mode: ObjectDisplayMode,
    /// `app.objectSphereSize`, geometry radius units (default 0.07).
    pub object_sphere_size: f32,
    pub object_colors_enabled: bool,
    pub object_labels_enabled: bool,
    pub effective_render_enabled: bool,
    /// Band used for the effective-render centroid (`heatmapBandIndex`).
    pub heatmap_band_index: usize,
    pub speakers_visible: bool,
    /// `app.speakerLabelsEnabled` (default false in the Studio).
    pub speaker_labels_enabled: bool,
    /// `app.speakerSize` (default 0.08).
    pub speaker_size: f32,
    pub trails: TrailSettings,
}

impl Default for ViewSettings {
    fn default() -> Self {
        Self {
            objects_visible: true,
            object_display_mode: ObjectDisplayMode::Circle,
            object_sphere_size: 0.07,
            object_colors_enabled: false,
            object_labels_enabled: true,
            effective_render_enabled: false,
            heatmap_band_index: 0,
            speakers_visible: true,
            speaker_labels_enabled: false,
            speaker_size: 0.08,
            trails: TrailSettings::default(),
        }
    }
}

/// Current selection: at most one of the two is set.
#[derive(Clone, Debug, Default)]
pub struct Selection {
    pub object: Option<String>,
    pub speaker: Option<usize>,
}

/// A label to draw with egui over the viewport, in screen points.
pub struct Label {
    pub pos: Pos2,
    pub text: String,
    pub color: egui::Color32,
    /// Font size in points, from the sprite's world height and depth.
    pub size: f32,
    /// View depth, so far labels draw first.
    pub depth: f32,
}

pub struct FrameOutput {
    pub frame: FrameData,
    pub labels: Vec<Label>,
    /// `(id, scene position, pick radius)` for objects.
    pub pick_objects: Vec<(String, Vec3, f32)>,
    /// `(index, scene position, pick radius)` for speakers.
    pub pick_speakers: Vec<(usize, Vec3, f32)>,
}

/// Normalised ADM position → room-warped three.js scene position
/// (`coordinates.js normalizedOmniphonyToScenePosition`).
pub fn scene_position(adm: [f64; 3], room: &RoomRatio) -> Vec3 {
    use omniphony_geometry::f64 as geometry;
    let clamped = [
        adm[0].clamp(-1.0, 1.0),
        adm[1].clamp(-1.0, 1.0),
        adm[2].clamp(-1.0, 1.0),
    ];
    let scaled = geometry::room_scaled_position(
        clamped,
        [room.width, room.length, room.height],
        room.rear,
        room.lower,
        room.center_blend,
    );
    let s = geometry::adm_to_scene(scaled);
    Vec3::new(s[0] as f32, s[1] as f32, s[2] as f32)
}

/// Build the frame. `rect` is the viewport in points, `ppp` pixels per point.
pub fn build_frame(
    live: &Live,
    settings: &ViewSettings,
    camera: &OrbitCamera,
    rect: Rect,
    ppp: f32,
    selection: &Selection,
    volume_settings: &VolumeSettings,
    volume_state: &mut VolumeState,
    now: Instant,
) -> FrameOutput {
    let viewport = [rect.width(), rect.height()];
    let aspect = rect.width() / rect.height().max(1.0);
    let view_proj = camera.view_proj(aspect, viewport);
    let (cam_right, cam_up) = camera.basis();
    let cam_pos = camera.eye();
    let size_px = [
        (rect.width() * ppp).round().max(1.0) as u32,
        (rect.height() * ppp).round().max(1.0) as u32,
    ];
    let mut frame = FrameData::new(view_proj, cam_pos, cam_right, cam_up, size_px);
    // scene/setup.js background 0x0a0b10
    let bg = hex_linear(0x0a0b10);
    frame.clear = [bg[0] as f64, bg[1] as f64, bg[2] as f64, 1.0];

    let mut labels: Vec<Label> = Vec::with_capacity(128);
    let mut pick_objects = Vec::new();
    let mut pick_speakers = Vec::new();

    let project = |p: Vec3| -> Option<(Pos2, f32)> {
        let clip = view_proj * Vec4::new(p.x, p.y, p.z, 1.0);
        if clip.w <= 1e-4 {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        Some((
            Pos2::new(
                rect.min.x + (ndc.x + 1.0) * 0.5 * rect.width(),
                rect.min.y + (1.0 - ndc.y) * 0.5 * rect.height(),
            ),
            clip.w,
        ))
    };
    // Points per scene unit at depth `d` (perspective attenuation of sprites).
    let half_fov_tan = (camera.fov_y * 0.5).tan();
    let points_per_unit = |d: f32| rect.height() / (2.0 * d.max(0.05) * half_fov_tan);

    let room = live.app.room_ratio.clone();
    let bounds = RoomBounds::from_ratio(&room);
    room::emit_room(&bounds, cam_pos, &mut frame);
    room::emit_axes(&mut frame, &project, &points_per_unit, &mut labels);

    // Listener head: placeholder sphere until the glTF model lands (max
    // dimension 0.34 in the Studio).
    frame.meshes.push(MeshItem {
        kind: MeshKind::Sphere,
        instance: MeshInstance::new(
            Mat4::from_scale_rotation_translation(Vec3::splat(0.17), Quat::IDENTITY, Vec3::ZERO),
            with_alpha(hex_linear(0xb9a58f), 1.0),
            [0.0, 0.0, 0.0, 0.1],
        ),
        blend: false,
        depth_test: true,
        order: 0,
    });

    // Speakers.
    let speaker_visuals = speakers::collect(
        live,
        settings,
        &room,
        selection.object.as_deref(),
        selection.speaker,
        now,
    );
    let speaker_refs: Vec<SpeakerRef> = speaker_visuals
        .iter()
        .map(|s| SpeakerRef {
            scene_pos: s.scene_pos,
        })
        .collect();
    if settings.speakers_visible {
        for sp in &speaker_visuals {
            speakers::emit(sp, &mut frame);
            pick_speakers.push((
                sp.index,
                sp.scene_pos,
                speakers::SPEAKER_BASE_SIZE * sp.scale * 0.87,
            ));
            if settings.speaker_labels_enabled
                && let Some((p, depth)) = project(sp.scene_pos + Vec3::new(0.0, 0.12, 0.0))
            {
                labels.push(Label {
                    pos: p + egui::vec2(0.0, 0.03 * points_per_unit(depth)),
                    text: sp.name.clone(),
                    color: egui::Color32::WHITE,
                    size: (0.06 * points_per_unit(depth)).clamp(6.0, 48.0),
                    depth,
                });
            }
        }
    }

    // Objects, their labels and trails.
    let objects = objects::collect(
        live,
        settings,
        &room,
        &speaker_refs,
        selection.object.as_deref(),
        selection.speaker,
        now,
    );
    if settings.objects_visible {
        for obj in &objects {
            objects::emit(obj, settings, &mut frame, cam_right, cam_up);
            let mesh_scale =
                obj.level_scale * settings.object_sphere_size / objects::SOURCE_BASE_RADIUS;
            pick_objects.push((
                obj.id.clone(),
                obj.scene_pos,
                objects::SOURCE_BASE_RADIUS * mesh_scale,
            ));
            if settings.object_labels_enabled
                && let Some((p, depth)) = project(obj.scene_pos)
            {
                // Sprite 0.42×0.16 with 36 px glyphs on a 96 px canvas → 0.06
                // scene units of glyph height, centred on the mesh.
                labels.push(Label {
                    pos: p + egui::vec2(0.0, 0.03 * points_per_unit(depth)),
                    text: obj.label.clone(),
                    color: egui::Color32::WHITE,
                    size: (0.06 * points_per_unit(depth)).clamp(6.0, 48.0),
                    depth,
                });
            }
            if settings.trails.enabled
                && let Some(trail) = live.trails.get(&obj.id)
            {
                trails::emit(
                    trail,
                    &settings.trails,
                    &room,
                    &speaker_refs,
                    objects::trail_color(obj.base_color),
                    obj.level_scale,
                    now,
                    &mut frame,
                );
            }
        }
    }

    // Selection face shadows.
    let shadow_pos = selection
        .speaker
        .and_then(|i| speaker_visuals.iter().find(|s| s.index == i))
        .map(|s| s.scene_pos)
        .or_else(|| {
            selection
                .object
                .as_deref()
                .and_then(|id| objects.iter().find(|o| o.id == id))
                .map(|o| o.scene_pos)
        });
    if let Some(p) = shadow_pos {
        room::emit_face_shadows(p, &bounds, &mut frame);
    }

    frame.volumes = volumes::build(
        live,
        volume_settings,
        volume_state,
        &bounds,
        &room,
        selection.speaker,
        now,
    );

    FrameOutput {
        frame,
        labels,
        pick_objects,
        pick_speakers,
    }
}

/// Camera-facing circle as a line list (`createSourceOutline`, 64 points).
pub fn billboard_ring(
    center: Vec3,
    radius: f32,
    right: Vec3,
    up: Vec3,
    color: [f32; 4],
    out: &mut Vec<crate::render::LineVertex>,
) {
    const N: usize = 64;
    let mut prev = center + right * radius;
    for i in 1..=N {
        let a = i as f32 / N as f32 * std::f32::consts::TAU;
        let (s, c) = a.sin_cos();
        let p = center + (right * c + up * s) * radius;
        out.push(crate::render::LineVertex {
            pos: prev.to_array(),
            color,
        });
        out.push(crate::render::LineVertex {
            pos: p.to_array(),
            color,
        });
        prev = p;
    }
}

/// `dbfsToScale`: −100..0 dBFS → `min..max`.
pub fn dbfs_to_scale(dbfs: f64, min: f32, max: f32) -> f32 {
    let n = ((dbfs.clamp(-100.0, 0.0) + 100.0) / 100.0) as f32;
    min + n * (max - min)
}

/// Level after the Studio's decay: untouched for 250 ms, then −45 dB/s down to
/// −100 dBFS (`decayMeters`, speakers.js).
pub fn decayed_level(level: f64, seen: Option<Instant>, now: Instant) -> f64 {
    let Some(seen) = seen else { return level };
    let idle = now.saturating_duration_since(seen).as_secs_f64();
    if idle <= 0.25 {
        level
    } else {
        (level - 45.0 * (idle - 0.25)).max(-100.0)
    }
}
