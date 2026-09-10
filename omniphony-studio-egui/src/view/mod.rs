//! Derives what the viewport draws from the live model, following the
//! Studio's `sources.js`, `speakers.js`, `scene/*.js`, `trails.js` and
//! `coordinates.js` rule for rule (see the phase 1 specs). Everything here is
//! CPU-side: it produces a `FrameData` for the renderer plus the screen-space
//! labels egui draws on top, and the pick lists the app uses for selection.

pub mod gizmos;
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
pub use trails::TrailSettings;
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
    /// `app.speakerBandBarsEnabled`: the per-speaker frequency-extent gauge.
    pub speaker_band_bars_enabled: bool,
    /// `app.speakerFaceListenerEnabled`: aim the cubes at the listener and
    /// show the driver face.
    pub speaker_face_listener_enabled: bool,
    /// `app.roomGeometryExpanded`: the dimension guides, shown while the room
    /// panel is open. Not persisted, as in the web — it follows the panel.
    pub room_guides_visible: bool,
    /// Which edit gizmo the editor has armed, if any.
    pub gizmo: gizmos::GizmoState,
    /// Which blend-curve point the hybrid panel has selected, if any: it is
    /// what the iso-distance shape is drawn for.
    pub hybrid_point: Option<usize>,
    /// `app.speakerSize` (default 0.08).
    pub speaker_size: f32,
    /// `app.vbapCartesianFaceGridEnabled` ("Grid", default false).
    pub vbap_grid: bool,
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
            speaker_band_bars_enabled: false,
            speaker_face_listener_enabled: false,
            room_guides_visible: false,
            gizmo: gizmos::GizmoState::default(),
            hybrid_point: None,
            speaker_size: 0.08,
            vbap_grid: false,
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

/// A speaker's frequency-extent gauge, drawn over the viewport.
///
/// The web makes it a billboard sprite with the depth test off, which is a
/// screen-space overlay by another name; egui draws it directly rather than
/// carrying a texture through the renderer for four rectangles and three
/// ticks.
pub struct BandBar {
    /// The speaker it belongs to: the bar is a pick target too, as in the web.
    pub speaker: usize,
    /// Centre, in screen points.
    pub pos: Pos2,
    /// Height in points, from the sprite's world height at this depth.
    pub height: f32,
    /// The pass-band in hertz; zero means "open at this end".
    pub low: f32,
    pub high: f32,
    /// The lit segment's colour, the band's own.
    pub color: egui::Color32,
    pub depth: f32,
}

impl BandBar {
    /// The canvas the web draws on is 64×256, and every number below is one of
    /// its pixels scaled to this bar's height.
    const CANVAS_W: f32 = 64.0;
    const CANVAS_H: f32 = 256.0;
    const PAD_Y: f32 = 10.0;
    const TRACK_W: f32 = 22.0;
    const TRACK_H: f32 = 236.0;
    const RADIUS: f32 = 8.0;

    /// Where a frequency sits on the track, 0 at the bottom (20 Hz) and 1 at
    /// the top (20 kHz), on a log axis.
    pub fn log_pos(hz: f32) -> f32 {
        let hz = hz.clamp(20.0, 20_000.0);
        (hz.ln() - 20.0f32.ln()) / (20_000.0f32.ln() - 20.0f32.ln())
    }

    /// The pass-band as drawn: an end the layout does not cut is open, and
    /// reads as the end of the axis.
    pub fn pass_band(low: f32, high: f32) -> (f32, f32) {
        (
            if low > 0.0 { low } else { 20.0 },
            if high > 0.0 { high } else { 20_000.0 },
        )
    }

    /// The bar's extent in screen points.
    pub fn rect(&self) -> egui::Rect {
        let h = self.height;
        egui::Rect::from_center_size(self.pos, egui::vec2(Self::CANVAS_W * h / Self::CANVAS_H, h))
    }

    pub fn paint(&self, painter: &egui::Painter) {
        let h = self.height;
        let scale = h / Self::CANVAS_H;
        let w = Self::CANVAS_W * scale;
        let top_left = self.rect().min;
        let track = egui::Rect::from_min_size(
            top_left + egui::vec2((w - Self::TRACK_W * scale) * 0.5, Self::PAD_Y * scale),
            egui::vec2(Self::TRACK_W * scale, Self::TRACK_H * scale),
        );
        let radius =
            egui::CornerRadius::same((Self::RADIUS * scale).round().clamp(0.0, 255.0) as u8);
        let y_for = |hz: f32| track.top() + (1.0 - Self::log_pos(hz)) * track.height();
        painter.rect_filled(
            track,
            radius,
            egui::Color32::from_rgba_unmultiplied(16, 22, 30, 209),
        );
        let (low, high) = Self::pass_band(self.low, self.high);
        // The lit segment is the speaker's role at a glance: a sub fills the
        // bottom, a tweeter the top, a mid a floating middle.
        let lit = egui::Rect::from_min_max(
            egui::pos2(track.left() + 2.0 * scale, y_for(high)),
            egui::pos2(
                track.right() - 2.0 * scale,
                (y_for(low)).max(y_for(high) + 2.0 * scale),
            ),
        );
        painter.rect_filled(lit.intersect(track), radius, self.color);
        painter.rect_stroke(
            track,
            radius,
            egui::Stroke::new(2.0 * scale, egui::Color32::from_white_alpha(71)),
            egui::StrokeKind::Inside,
        );
        // Decade ticks, so a segment can be read against the axis rather than
        // only compared with its neighbours.
        for hz in [100.0, 1000.0, 10_000.0] {
            let y = y_for(hz);
            painter.hline(
                (track.left() + 3.0 * scale)..=(track.right() - 3.0 * scale),
                y,
                egui::Stroke::new(scale.max(0.5), egui::Color32::from_white_alpha(56)),
            );
        }
    }
}

pub struct FrameOutput {
    pub frame: FrameData,
    pub labels: Vec<Label>,
    pub band_bars: Vec<BandBar>,
    /// What the edit gizmo is on, and where it is, for the drag handlers.
    pub gizmo_target: Option<(gizmos::GizmoTarget, Vec3)>,
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
    // Eased head-pose rotation and whether the glTF head is available.
    head_rotation: Quat,
    head_loaded: bool,
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
    let mut band_bars: Vec<BandBar> = Vec::new();
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
    if settings.vbap_grid {
        room::emit_vbap_grids(
            &bounds,
            &room,
            cam_pos,
            &live.app.vbap_cartesian,
            &mut frame,
        );
    }
    room::emit_axes(&mut frame, &project, &points_per_unit, &mut labels);
    // The hybrid backend's iso-distance surface, for the selected curve point.
    if live.app.render_backend_state.selection.as_deref() == Some("hybrid")
        && let Some(index) = settings.hybrid_point
        && let Some(stop) = live.app.render_backend_state.hybrid.curve.get(index)
    {
        let spherical = live.app.render_backend_state.hybrid.metric.as_deref() == Some("spherical");
        // A spherical metric measures the radius; a cubic one measures the
        // half-side, and the corner of that cube is √3 further out.
        let radius = stop[0] as f32 * if spherical { 3.0f32.sqrt() } else { 1.0 };
        room::emit_hybrid_distance(radius, spherical, &room, &mut frame);
    }
    if settings.room_guides_visible {
        room::emit_dimension_guides(
            &bounds,
            &room,
            &mut frame,
            &project,
            &points_per_unit,
            &mut labels,
        );
    }

    // Listener head (Dame de Brassempouy, roughness 0.92, metalness 0) under
    // the head-pose rotation; a sphere of the same size when the asset is
    // missing.
    if head_loaded {
        frame.meshes.push(MeshItem {
            kind: MeshKind::Head,
            instance: MeshInstance::new(
                Mat4::from_rotation_translation(head_rotation, Vec3::ZERO),
                [1.0, 1.0, 1.0, 1.0],
                [0.0, 0.0, 0.0, 0.08],
            ),
            blend: false,
            depth_test: true,
            order: 0,
        });
    } else {
        frame.meshes.push(MeshItem {
            kind: MeshKind::Sphere,
            instance: MeshInstance::new(
                Mat4::from_scale_rotation_translation(Vec3::splat(0.17), head_rotation, Vec3::ZERO),
                with_alpha(hex_linear(0xb9a58f), 1.0),
                [0.0, 0.0, 0.0, 0.1],
            ),
            blend: false,
            depth_test: true,
            order: 0,
        });
    }

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
            speakers::emit(sp, settings, &mut frame);
            pick_speakers.push((
                sp.index,
                sp.scene_pos,
                speakers::SPEAKER_BASE_SIZE * sp.scale * 0.87,
            ));
            // `SPEAKER_BAND_BAR_OFFSET`: beside the speaker along the depth
            // axis, so the gauge never sits on the cube it belongs to.
            if settings.speaker_band_bars_enabled
                && let Some((p, depth)) = project(sp.scene_pos + Vec3::new(0.11, 0.0, 0.0))
            {
                let rgb =
                    crate::render::linear_to_srgb_u8(speakers::band_color(sp.band.0, sp.band.1));
                band_bars.push(BandBar {
                    speaker: sp.index,
                    pos: p,
                    // The sprite is 0.22 world units tall.
                    height: 0.22 * points_per_unit(depth),
                    low: sp.pass_band.0,
                    high: sp.pass_band.1,
                    color: egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]),
                    depth,
                });
            }
            if settings.speaker_labels_enabled
                && let Some((p, depth)) = project(sp.scene_pos + Vec3::new(0.0, 0.12, 0.0))
            {
                labels.push(Label {
                    pos: p + egui::vec2(0.0, 0.03 * points_per_unit(depth)),
                    text: sp.name.clone(),
                    color: if sp.ghosted {
                        egui::Color32::from_white_alpha(77)
                    } else {
                        egui::Color32::WHITE
                    },
                    size: (0.06 * points_per_unit(depth)).clamp(6.0, 48.0).round(),
                    depth,
                });
            }
        }
    }

    // The edit gizmos: the selected speaker, or a selected object that is a
    // virtual bed channel — the two things whose position this editor moves.
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
    // The edit gizmos follow the thing this editor moves: the selected
    // speaker, or a selected object that is a virtual bed channel — a real
    // object's position belongs to whatever is playing it, not to the editor.
    let gizmo_target: Option<(gizmos::GizmoTarget, Vec3)> = match selection.speaker {
        Some(index) => speaker_visuals
            .get(index)
            .map(|sp| (gizmos::GizmoTarget::Speaker(index), sp.scene_pos)),
        None => selection.object.as_deref().and_then(|id| {
            gizmos::is_virtual_channel(&live.app, id)
                .then(|| {
                    objects
                        .iter()
                        .find(|o| o.id == id)
                        .map(|o| (gizmos::GizmoTarget::Channel(id.to_owned()), o.scene_pos))
                })
                .flatten()
        }),
    };
    if let Some((_, target)) = gizmo_target.clone() {
        let g = settings.gizmo;
        match g.mode {
            gizmos::EditMode::Polar if g.polar_armed => {
                gizmos::emit_polar(target, &mut frame, &project, &points_per_unit, &mut labels)
            }
            gizmos::EditMode::Cartesian if g.cartesian_armed => {
                gizmos::emit_cartesian(target, cam_pos, &mut frame)
            }
            _ => {}
        }
    }
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
                    size: (0.06 * points_per_unit(depth)).clamp(6.0, 48.0).round(),
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
        band_bars,
        gizmo_target,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The axis is logarithmic between 20 Hz and 20 kHz, and a decade is a
    /// third of it: that is what makes the three tick lines evenly spaced.
    #[test]
    fn the_track_is_three_decades_of_log_frequency() {
        assert!((BandBar::log_pos(20.0) - 0.0).abs() < 1e-6);
        assert!((BandBar::log_pos(20_000.0) - 1.0).abs() < 1e-6);
        let (a, b, c) = (
            BandBar::log_pos(100.0),
            BandBar::log_pos(1000.0),
            BandBar::log_pos(10_000.0),
        );
        assert!(((b - a) - (c - b)).abs() < 1e-4, "decades are not even");
        // Anything off the axis is clamped onto it rather than drawn outside.
        assert_eq!(BandBar::log_pos(1.0), BandBar::log_pos(20.0));
        assert_eq!(BandBar::log_pos(96_000.0), BandBar::log_pos(20_000.0));
    }

    /// A speaker the layout does not cut is full-band, and its bar is lit end
    /// to end rather than empty.
    #[test]
    fn an_uncut_end_is_open() {
        assert_eq!(BandBar::pass_band(0.0, 0.0), (20.0, 20_000.0));
        assert_eq!(BandBar::pass_band(0.0, 120.0), (20.0, 120.0));
        assert_eq!(BandBar::pass_band(2000.0, 0.0), (2000.0, 20_000.0));
    }
}
