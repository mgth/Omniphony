//! Derives what the viewport draws from the live model, following the
//! Studio's `sources.js`, `speakers.js`, `scene/*.js`, `trails.js` and
//! `coordinates.js` rule for rule (see the phase 1 specs). Everything here is
//! CPU-side: it produces a `FrameData` for the renderer plus the screen-space
//! labels the app draws on top, and the pick lists it uses for selection.

pub mod gizmos;
pub mod objects;
pub mod room;
pub mod screen;
pub mod speakers;
pub mod trails;
pub mod volumes;

use std::time::Instant;

use glam::{Mat4, Quat, Vec3, Vec4};
use screen::{Color, ScreenPos, ScreenRect, Shape};

use crate::model::app_state::RoomRatio;
use crate::model::binaural::RenderPath;
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
    /// `app.showObjectDetails`: the coordinate line above each object row's
    /// meter (`body.hide-object-details` when off).
    pub show_object_details: bool,
    pub effective_render_enabled: bool,
    /// Band used for the effective-render centroid (`heatmapBandIndex`).
    pub heatmap_band_index: usize,
    /// `VolumeSettings::all_bands`, copied in each frame: with every band
    /// composited, the one-band readouts use the full-band gains instead.
    pub heatmap_all_bands: bool,
    pub speakers_visible: bool,
    /// Keep the speaker layout in view on the direct headphone path, where
    /// nothing feeds it: hidden by default there, ghosted when shown, a
    /// reference for where the objects are.
    pub speakers_on_headphones: bool,
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
    /// `channelEditPinId` / `channelEditPinPos`: an object whose position the
    /// editor owns for the moment, so the live stream cannot fight a drag.
    pub channel_edit_pin: Option<(String, Vec3)>,
    /// The speaker whose position the editor owns for the moment, and where.
    /// Speakers are drawn from the renderer's state, which only learns of a
    /// move on release: the pin is what makes the cube and its gizmo follow
    /// the pointer, and it outlives the release long enough for the renderer
    /// to echo the new position back.
    pub speaker_edit_pin: Option<(usize, Vec3)>,
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
            show_object_details: true,
            effective_render_enabled: false,
            heatmap_band_index: 0,
            heatmap_all_bands: true,
            speakers_visible: true,
            speakers_on_headphones: false,
            speaker_labels_enabled: false,
            speaker_band_bars_enabled: false,
            speaker_face_listener_enabled: false,
            room_guides_visible: false,
            gizmo: gizmos::GizmoState::default(),
            hybrid_point: None,
            channel_edit_pin: None,
            speaker_edit_pin: None,
            speaker_size: 0.08,
            vbap_grid: false,
            trails: TrailSettings::default(),
        }
    }
}

/// Current selection: at most one of the two is set.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Selection {
    pub object: Option<String>,
    pub speaker: Option<usize>,
}

/// A label to draw over the viewport, in screen points.
pub struct Label {
    pub pos: ScreenPos,
    pub text: String,
    pub color: Color,
    /// Font size in points, from the sprite's world height and depth.
    pub size: f32,
    /// View depth, so far labels draw first.
    pub depth: f32,
}

/// A speaker's frequency-extent gauge, drawn over the viewport.
///
/// The web makes it a billboard sprite with the depth test off, which is a
/// screen-space overlay by another name; it is drawn on top rather than
/// carrying a texture through the renderer for four rectangles and three
/// ticks. This says what those are; the app puts them on screen.
pub struct BandBar {
    /// The speaker it belongs to: the bar is a pick target too, as in the web.
    pub speaker: usize,
    /// Centre, in screen points.
    pub pos: ScreenPos,
    /// Height in points, from the sprite's world height at this depth.
    pub height: f32,
    /// The pass-band in hertz; zero means "open at this end".
    pub low: f32,
    pub high: f32,
    /// The lit segment's colour, the band's own.
    pub color: Color,
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
    pub fn rect(&self) -> ScreenRect {
        let h = self.height;
        ScreenRect::from_center_size(
            self.pos,
            glam::Vec2::new(Self::CANVAS_W * h / Self::CANVAS_H, h),
        )
    }

    /// The bar as flat shapes. Composing it here rather than in a panel keeps
    /// the gauge with the projection that placed it; painting it here would
    /// need a toolkit.
    pub fn shapes(&self) -> Vec<Shape> {
        let h = self.height;
        let scale = h / Self::CANVAS_H;
        let w = Self::CANVAS_W * scale;
        let top_left = self.rect().min;
        let track = ScreenRect::from_min_size(
            top_left + glam::Vec2::new((w - Self::TRACK_W * scale) * 0.5, Self::PAD_Y * scale),
            glam::Vec2::new(Self::TRACK_W * scale, Self::TRACK_H * scale),
        );
        let radius = Self::RADIUS * scale;
        let y_for = |hz: f32| track.top() + (1.0 - Self::log_pos(hz)) * track.height();
        let (low, high) = Self::pass_band(self.low, self.high);
        // The lit segment is the speaker's role at a glance: a sub fills the
        // bottom, a tweeter the top, a mid a floating middle.
        let lit = ScreenRect::from_min_max(
            glam::Vec2::new(track.left() + 2.0 * scale, y_for(high)),
            glam::Vec2::new(
                track.right() - 2.0 * scale,
                y_for(low).max(y_for(high) + 2.0 * scale),
            ),
        );
        let mut shapes = vec![
            Shape::Rect {
                rect: track,
                radius,
                fill: Some([16, 22, 30, 209]),
                stroke: None,
            },
            Shape::Rect {
                rect: lit.intersect(track),
                radius,
                fill: Some(self.color),
                stroke: None,
            },
            Shape::Rect {
                rect: track,
                radius,
                fill: None,
                stroke: Some((2.0 * scale, screen::white_alpha(71))),
            },
        ];
        // Decade ticks, so a segment can be read against the axis rather than
        // only compared with its neighbours.
        shapes.extend([100.0, 1000.0, 10_000.0].map(|hz| Shape::HLine {
            x: (track.left() + 3.0 * scale, track.right() - 3.0 * scale),
            y: y_for(hz),
            width: scale.max(0.5),
            color: screen::white_alpha(56),
        }));
        shapes
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

/// Where a normalised ADM position is shown while the renderer reads
/// positions on the listener's sphere (`RoomRatio::sphere`), in the ADM
/// frame: in the direction it is heard at
/// (`omniphony_geometry::sphere_reading`), at its distance to the room's
/// surface. The room's surface is drawn as the unit sphere it is heard as,
/// so a source sits where it sounds.
pub fn sphere_heard_point(adm: [f64; 3]) -> [f64; 3] {
    use omniphony_geometry::f64 as geometry;
    let read = geometry::sphere_reading(adm);
    let length = geometry::vec3::length(read);
    if length <= 0.0 {
        return adm;
    }
    let reach = adm[0].abs().max(adm[1].abs()).max(adm[2].abs());
    let scale = reach / length;
    [read[0] * scale, read[1] * scale, read[2] * scale]
}

/// Normalised ADM position → three.js scene position in the frame `room`
/// describes (`AppState::display_room`): warped by the room
/// (`coordinates.js normalizedOmniphonyToScenePosition`), or, on the direct
/// headphone path reading the sphere, where it is heard
/// ([`sphere_heard_point`]).
pub fn scene_position(adm: [f64; 3], room: &RoomRatio) -> Vec3 {
    use omniphony_geometry::f64 as geometry;
    let clamped = [
        adm[0].clamp(-1.0, 1.0),
        adm[1].clamp(-1.0, 1.0),
        adm[2].clamp(-1.0, 1.0),
    ];
    if room.sphere {
        return scene_point(sphere_heard_point(clamped));
    }
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

/// A point of the renderer's frame (x right, y front, z up) in scene units,
/// as it is: no clamp, no warp — a measured position.
pub fn scene_point(adm: [f64; 3]) -> Vec3 {
    let s = omniphony_geometry::f64::adm_to_scene(adm);
    Vec3::new(s[0] as f32, s[1] as f32, s[2] as f32)
}

/// Build the frame. `rect` is the viewport in points, `ppp` pixels per point.
pub fn build_frame(
    live: &Live,
    settings: &ViewSettings,
    camera: &OrbitCamera,
    rect: ScreenRect,
    ppp: f32,
    selection: &Selection,
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

    let project = |p: Vec3| -> Option<(ScreenPos, f32)> {
        let clip = view_proj * Vec4::new(p.x, p.y, p.z, 1.0);
        if clip.w <= 1e-4 {
            return None;
        }
        let ndc = clip.truncate() / clip.w;
        Some((
            ScreenPos::new(
                rect.min.x + (ndc.x + 1.0) * 0.5 * rect.width(),
                rect.min.y + (1.0 - ndc.y) * 0.5 * rect.height(),
            ),
            clip.w,
        ))
    };
    // Points per scene unit at depth `d` (perspective attenuation of sprites).
    let half_fov_tan = (camera.fov_y * 0.5).tan();
    let points_per_unit = |d: f32| rect.height() / (2.0 * d.max(0.05) * half_fov_tan);

    // The frame positions are placed in, as the engine places them: the live
    // room on every path through the speaker stage, the unit cube on the
    // direct binaural path, which reads a direction straight off a position
    // (`AppState::display_room`, which the editors and the volumes share).
    // The room's grid, the hybrid surface and the dimension guides describe
    // the speaker stage, so they go with the room.
    let path = live.app.render_path();
    let room = live.app.display_room();
    let bounds = RoomBounds::from_ratio(&room);
    // A measured room (BRIR): the set's own box replaces the user's room,
    // which does not render there; the sources keep the stage's frame.
    let measured = (path == RenderPath::MeasuredRoom)
        .then(|| live.app.brir_geometry())
        .flatten();
    if let Some(geometry) = &measured {
        room::emit_measured_room(
            geometry.room_box_m(),
            geometry.metres_per_unit(),
            cam_pos,
            &mut frame,
            &project,
            &points_per_unit,
            &mut labels,
        );
    } else if path.warps_with_room() {
        room::emit_room(&bounds, cam_pos, &room::RoomStyle::SPEAKER_ROOM, &mut frame);
        if settings.vbap_grid {
            room::emit_vbap_grids(
                &bounds,
                &room,
                cam_pos,
                &live.app.vbap_cartesian,
                &mut frame,
            );
        }
    } else {
        if room.sphere {
            // Positions are read on the sphere: the room's surface is the
            // unit sphere round the listener, and that is what is drawn.
            room::emit_listener_sphere(&mut frame);
        } else {
            room::emit_room(
                &bounds,
                cam_pos,
                &room::RoomStyle::LISTENER_CUBE,
                &mut frame,
            );
        }
        room::emit_unit_guide(
            &bounds,
            room.scale_m as f32,
            &mut frame,
            &project,
            &points_per_unit,
            &mut labels,
        );
    }
    // The listening room of the early reflections, on the two HRTF paths
    // (a measured room carries its own), while they are on: at the distance
    // scale the binaural stage reads the frame at.
    if matches!(path, RenderPath::Direct | RenderPath::VirtualRoom)
        && let Some(room_m) = live.app.binaural_reflection_room_m()
    {
        room::emit_listening_room(
            [room_m[0] as f32, room_m[1] as f32, room_m[2] as f32],
            live.app.binaural_unit_scale_m() as f32,
            &mut frame,
            &project,
            &points_per_unit,
            &mut labels,
        );
    }
    room::emit_axes(&mut frame, &project, &points_per_unit, &mut labels);
    // The hybrid backend's iso-distance surface, for the selected curve point.
    if path.warps_with_room()
        && measured.is_none()
        && live.app.render_backend_state.selection.as_deref() == Some("hybrid")
        && let Some(index) = settings.hybrid_point
        && let Some(stop) = live.app.render_backend_state.hybrid.curve.get(index)
    {
        let spherical = live.app.render_backend_state.hybrid.metric.as_deref() == Some("spherical");
        // The curve's x runs over the whole range of the metric it is written
        // in: for a cubic metric that is the half-side, and for a spherical
        // one the corner distance, which is √3 further out.
        let radius = stop[0] as f32 * if spherical { 3.0f32.sqrt() } else { 1.0 };
        room::emit_hybrid_distance(radius, spherical, &room, &mut frame);
    }
    if path.warps_with_room() && measured.is_none() && settings.room_guides_visible {
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

    // Speakers. A speaker's position is where it stands, not a position
    // the binaural stage reads: the sphere reading is the sources' alone
    // (`AppState::speaker_frame`, which the speaker gizmo inverts through).
    let speaker_room = live.app.speaker_frame();
    let speaker_visuals = speakers::collect(
        live,
        settings,
        &speaker_room,
        selection.object.as_deref(),
        selection.speaker,
    );
    let speaker_refs: Vec<SpeakerRef> = speaker_visuals
        .iter()
        .map(|s| SpeakerRef {
            scene_pos: s.scene_pos,
        })
        .collect();
    // On the direct path nothing feeds the speakers: the layout is hidden
    // there unless asked for as a reference.
    let speakers_shown =
        settings.speakers_visible && (path.speakers_render() || settings.speakers_on_headphones);
    if speakers_shown {
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
                    color: screen::rgb(rgb[0], rgb[1], rgb[2]),
                    depth,
                });
            }
            if settings.speaker_labels_enabled
                && let Some((p, depth)) = project(sp.scene_pos + Vec3::new(0.0, 0.12, 0.0))
            {
                labels.push(Label {
                    pos: p + glam::Vec2::new(0.0, 0.03 * points_per_unit(depth)),
                    text: sp.name.clone(),
                    color: if sp.ghosted() {
                        screen::white_alpha(77)
                    } else {
                        screen::WHITE
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
    );
    // The edit gizmos follow the thing this editor moves: the selected
    // speaker, or a selected object that is a virtual bed channel — a real
    // object's position belongs to whatever is playing it, not to the editor.
    // Read-only speakers (frozen by the backend, or a BRIR set's own) have
    // nothing to move.
    let gizmo_target: Option<(gizmos::GizmoTarget, Vec3)> = match selection.speaker {
        Some(_) if live.app.speakers_read_only() => None,
        Some(index) => speaker_visuals
            .get(index)
            .map(|sp| (gizmos::GizmoTarget::Speaker(index), sp.scene_pos)),
        None => selection.object.as_deref().and_then(|id| {
            let name =
                gizmos::virtual_channel_of(&live.channels, &live.app, live.editing_family, id)?;
            objects.iter().find(|o| o.id == id).map(|o| {
                let target = gizmos::GizmoTarget::Channel {
                    id: id.to_owned(),
                    name,
                };
                (target, o.scene_pos)
            })
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
                    pos: p + glam::Vec2::new(0.0, 0.03 * points_per_unit(depth)),
                    text: obj.label.clone(),
                    color: screen::WHITE,
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
    // The shadows fall on the walls of a box: the sphere has none.
    if let Some(p) = shadow_pos.filter(|_| !room.sphere) {
        room::emit_face_shadows(p, &bounds, &mut frame);
    }

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

    /// The unit room is the engine's direct path: a normalized position
    /// lands where `adm_to_scene` puts it, with no warp, where the live room
    /// would have moved it.
    #[test]
    fn the_unit_room_does_not_warp() {
        let unit = RoomRatio::unit(3.0);
        assert_eq!(unit.scale_m, 3.0);
        for adm in [[0.5, 0.866, 0.0], [-1.0, -1.0, -1.0], [0.3, 0.2, 0.9]] {
            let s = omniphony_geometry::f64::adm_to_scene(adm);
            let expected = Vec3::new(s[0] as f32, s[1] as f32, s[2] as f32);
            assert!(
                (scene_position(adm, &unit) - expected).length() < 1e-6,
                "{adm:?} is warped by the unit room"
            );
        }
        let left = [0.5, 0.866, 0.0];
        let warped = scene_position(left, &RoomRatio::default());
        assert!((warped - scene_position(left, &unit)).length() > 0.1);
        // And the gizmos' inverse brings a point back through the same
        // room, so a drag lands where the pointer is.
        for adm in [[0.2, 0.8, 0.0], [-0.7, -0.3, 0.5]] {
            let back = gizmos::scene_to_normalized(scene_position(adm, &unit), &unit);
            for axis in 0..3 {
                assert!((back[axis] - adm[axis]).abs() < 1e-5, "{adm:?} → {back:?}");
            }
        }
    }

    /// While the renderer reads positions on the sphere, a source is drawn
    /// where it is heard: the room's front-left corner at −30° on the unit
    /// sphere, a top corner 45° up, a point halfway in at half the radius.
    /// The gizmos' inverse comes back to the room position, and a pointer
    /// beyond the sphere is held on it.
    #[test]
    fn the_sphere_reading_draws_a_source_where_it_is_heard() {
        let sphere = RoomRatio {
            sphere: true,
            ..RoomRatio::unit(1.0)
        };
        let drawn = |adm: [f64; 3]| {
            let s = scene_position(adm, &sphere);
            let adm = omniphony_geometry::f64::scene_to_adm([s.x as f64, s.y as f64, s.z as f64]);
            omniphony_geometry::f64::to_spherical(adm[0], adm[1], adm[2])
        };
        let near = |(az, el, r): (f64, f64, f64), want: (f64, f64, f64)| {
            assert!(
                (az - want.0).abs() < 1e-4
                    && (el - want.1).abs() < 1e-4
                    && (r - want.2).abs() < 1e-5,
                "drawn at {az:.3}/{el:.3}, radius {r:.4}; expected {want:?}"
            );
        };
        near(drawn([-1.0, 1.0, 0.0]), (-30.0, 0.0, 1.0));
        near(drawn([1.0, 1.0, 1.0]), (45.0, 45.0, 1.0));
        near(drawn([1.0, -1.0, 0.0]), (135.0, 0.0, 1.0));
        near(drawn([-0.5, 0.5, 0.0]), (-30.0, 0.0, 0.5));

        for adm in [
            [-1.0, 1.0, 0.0],
            [0.2, 0.8, 0.0],
            [-0.7, -0.3, 0.5],
            [0.4, 0.1, -1.0],
        ] {
            let back = gizmos::scene_to_normalized(scene_position(adm, &sphere), &sphere);
            for axis in 0..3 {
                assert!((back[axis] - adm[axis]).abs() < 1e-5, "{adm:?} → {back:?}");
            }
        }
        // Twice the radius, towards the front left at ear level.
        let beyond = Vec3::new(3.0f32.sqrt(), 0.0, -1.0);
        let held = scene_position(gizmos::scene_to_normalized(beyond, &sphere), &sphere);
        assert!((held - beyond * 0.5).length() < 1e-5, "held at {held:?}");
    }

    /// The listening room is placed around the listener in the renderer's
    /// frame: its width across the scene's z, its depth along x, its height
    /// along y, at the distance scale; and it is drawn as dashes, twelve
    /// edges of six.
    #[test]
    fn the_listening_room_sits_around_the_listener_at_the_distance_scale() {
        let half = room::listening_room_half_extents([4.0, 5.0, 2.7], 2.0);
        assert!(
            (half - Vec3::new(1.25, 0.675, 1.0)).length() < 1e-6,
            "{half:?}"
        );
        let mut frame = FrameData::new(Mat4::IDENTITY, Vec3::ZERO, Vec3::X, Vec3::Y, [1, 1]);
        let mut labels = Vec::new();
        room::emit_listening_room(
            [4.0, 5.0, 2.7],
            2.0,
            &mut frame,
            &|_| None,
            &|_| 1.0,
            &mut labels,
        );
        assert_eq!(frame.overlay_lines.len(), 12 * 6 * 2);
        for v in &frame.overlay_lines {
            let p = Vec3::from_array(v.pos).abs();
            assert!(p.x <= 1.25 + 1e-6 && p.y <= 0.675 + 1e-6 && p.z <= 1.0 + 1e-6);
        }
    }

    /// A measured room's box lands around the listener in scene units: the
    /// renderer's front along x, up along y, right along z, at the metres
    /// per unit it is given.
    #[test]
    fn a_measured_room_box_is_placed_in_the_renderers_frame() {
        let b = room::metres_box_bounds([[-2.0, -3.0, -1.2], [2.0, 3.0, 1.3]], 3.0);
        let near = |a: f32, w: f32| (a - w).abs() < 1e-6;
        assert!(
            near(b.x_min, -1.0) && near(b.x_max, 1.0),
            "front along x: {b:?}"
        );
        assert!(
            near(b.y_min, -0.4) && near(b.y_max, 1.3 / 3.0),
            "up along y: {b:?}"
        );
        assert!(
            near(b.z_min, -2.0 / 3.0) && near(b.z_max, 2.0 / 3.0),
            "right along z: {b:?}"
        );
        let p = scene_point([2.0, 3.0, 1.3]);
        assert!((p - Vec3::new(3.0, 1.3, 2.0)).length() < 1e-6, "{p:?}");
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
