//! The application: viewport in the central panel, fixed-extent panels
//! floated over it, camera input, picking, labels, stats.

use std::net::ToSocketAddrs;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Align2, Color32, Pos2, Rect};
use glam::{Quat, Vec3};

use crate::Args;
use crate::model::app_state::AppState;
use crate::model::layouts::load_layouts;
use crate::osc::dispatch::Live;
use crate::osc::{self, Control, ControlTx, OscStats, SharedLive};
use crate::render::camera::OrbitCamera;
use crate::render::{SceneRenderer, ViewportCallback};
use crate::stats::{FrameStats, ProcStats};
use crate::view::volumes::{Colormap, DiscontinuityMode};
use crate::view::{
    self, ObjectDisplayMode, Selection, TrailMode, ViewSettings, VolumeSettings, VolumeState,
};
use crate::widgets::{self, OPTION_SCHEMA, OptionValue};

const PANEL_WIDTH: f32 = 300.0;
const PANEL_MARGIN: f32 = 12.0;

pub struct StudioSpike {
    args: Args,
    live: SharedLive,
    osc_stats: Arc<OscStats>,
    camera: OrbitCamera,
    selection: Selection,
    settings: ViewSettings,
    options: Vec<OptionValue>,
    ime_text: String,
    frame_stats: FrameStats,
    proc_stats: ProcStats,
    last_print: Instant,
    /// Input events seen since the last stats line, to attribute repaints.
    input_events: u32,
    pointer_moves: u32,
    pointer_over: bool,
    /// Pick lists of the last built frame.
    pick_objects: Vec<(String, Vec3, f32)>,
    pick_speakers: Vec<(usize, Vec3, f32)>,
    volume_settings: VolumeSettings,
    volume_state: VolumeState,
    head_loaded: bool,
    /// Eased head-pose rotation (`scene/head-pose.js`, slerp 0.4 per frame).
    head_rotation: Quat,
    control: ControlTx,
    /// Gain-table targets currently subscribed, and the last (re)subscribe.
    subscribed_tables: Vec<i64>,
    last_subscribe: Option<Instant>,
}

impl StudioSpike {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        args: Args,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let rs = cc
            .wgpu_render_state
            .as_ref()
            .ok_or("eframe did not initialise wgpu")?;
        let head = match crate::render::head::load(&args.head_model) {
            Ok(mesh) => Some(mesh),
            Err(e) => {
                log::warn!("[head] {e}; drawing a placeholder sphere");
                None
            }
        };
        let head_loaded = head.is_some();
        rs.renderer
            .write()
            .callback_resources
            .insert(SceneRenderer::new(&rs.device, rs.target_format, head));

        let layouts = load_layouts(&args.layouts_dir);
        log::info!(
            "[layouts] {} layouts from {}",
            layouts.len(),
            args.layouts_dir.display()
        );
        let mut app = AppState::new(layouts);
        let key = args.layout_key.clone().or_else(|| {
            app.layouts
                .iter()
                .find(|l| l.key == "7.1.4")
                .or_else(|| app.layouts.first())
                .map(|l| l.key.clone())
        });
        if let Some(k) = &key {
            if app.layouts.iter().any(|l| &l.key == k) {
                log::info!("[layouts] default layout: {k}");
            } else {
                log::warn!("[layouts] no layout with key {k:?}");
            }
        }
        app.selected_layout_key = key;
        let live: SharedLive = Arc::new(Mutex::new(Live::new(app)));

        let osc_stats = OscStats::new();
        let register = match &args.register {
            Some(hp) => Some(
                hp.as_str()
                    .to_socket_addrs()?
                    .next()
                    .ok_or("--register: no address resolved")?,
            ),
            None => None,
        };
        let (port, control) = osc::spawn_listener(
            live.clone(),
            cc.egui_ctx.clone(),
            osc_stats.clone(),
            osc::ListenerConfig {
                listen_port: args.listen_port,
                register,
                metering: false,
            },
        )?;
        log::info!("[osc] listening on udp/{port}");
        if args.synthetic > 0 {
            let stop = (args.synthetic_stop_after > 0.0)
                .then(|| Duration::from_secs_f32(args.synthetic_stop_after));
            osc::spawn_synthetic(args.synthetic, args.rate, port, stop)?;
        }

        let object_field = args.object_field;
        let mut settings = ViewSettings::default();
        settings.trails.enabled = !args.no_trails;
        Ok(Self {
            args,
            live,
            osc_stats,
            camera: OrbitCamera::new(),
            selection: Selection::default(),
            settings,
            options: OPTION_SCHEMA.iter().map(|s| s.default.to_value()).collect(),
            ime_text: String::new(),
            frame_stats: FrameStats::new(),
            proc_stats: ProcStats::new(),
            last_print: Instant::now(),
            input_events: 0,
            pointer_moves: 0,
            pointer_over: false,
            pick_objects: Vec::new(),
            pick_speakers: Vec::new(),
            volume_settings: VolumeSettings {
                object_field_enabled: object_field,
                ..VolumeSettings::default()
            },
            volume_state: VolumeState::default(),
            head_loaded,
            head_rotation: Quat::IDENTITY,
            control,
            subscribed_tables: Vec::new(),
            last_subscribe: None,
        })
    }

    /// Head pose: only while the renderer is in binaural output mode; the
    /// wire quaternion is conjugated and permuted into the scene frame.
    fn ease_head_pose(&mut self, ctx: &egui::Context) {
        let target = {
            let live = self.live.lock().unwrap();
            let binaural = live
                .app
                .binaural
                .as_ref()
                .and_then(|b| b.get("outputMode"))
                .and_then(|m| m.as_str())
                == Some("binaural");
            match (binaural, live.head_pose) {
                (true, Some([w, x, y, z])) => Quat::from_xyzw(-y, -z, -x, w).normalize(),
                _ => Quat::IDENTITY,
            }
        };
        if self.head_rotation.abs_diff_eq(target, 1e-4) {
            self.head_rotation = target;
            return;
        }
        self.head_rotation = self.head_rotation.slerp(target, 0.4);
        ctx.request_repaint();
    }

    /// `acquireGainTable` / `releaseGainTable`: keep the renderer's
    /// gain-table subscriptions aligned with the enabled volumes, with the
    /// Studio's 5 s repair heartbeat.
    fn maintain_gaintable_subscriptions(&mut self) {
        if self.args.register.is_none() {
            return;
        }
        let wanted = view::volumes::wanted_tables(&self.volume_settings, self.selection.speaker);
        let changed = wanted != self.subscribed_tables;
        let heartbeat_due = self
            .last_subscribe
            .is_none_or(|t| t.elapsed() >= Duration::from_secs(5));
        if wanted.is_empty() {
            if changed {
                let _ = self.control.send(Control::UnsubscribeGainTable);
                self.subscribed_tables.clear();
            }
            return;
        }
        if changed || heartbeat_due {
            let versions: Vec<(i64, i32)> = {
                let live = self.live.lock().unwrap();
                wanted
                    .iter()
                    .map(|t| {
                        (
                            *t,
                            live.gain_tables
                                .get(t)
                                .map(|g| g.version() as i32)
                                .unwrap_or(0)
                                .max(0),
                        )
                    })
                    .collect()
            };
            for (target, have_version) in versions {
                let _ = self.control.send(Control::SubscribeGainTable {
                    have_version,
                    speaker_index: target as i32,
                });
            }
            self.subscribed_tables = wanted;
            self.last_subscribe = Some(Instant::now());
        }
    }

    // -----------------------------------------------------------------------
    // Viewport
    // -----------------------------------------------------------------------

    fn viewport(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());

        // OrbitControls: left rotate, middle dolly, right lens-shift pan, wheel dolly.
        if response.dragged_by(egui::PointerButton::Primary) {
            let d = response.drag_delta();
            self.camera.rotate(d.x, d.y, rect.height());
        }
        if response.dragged_by(egui::PointerButton::Middle) {
            self.camera.dolly_drag(response.drag_delta().y);
        }
        if response.dragged_by(egui::PointerButton::Secondary) {
            let d = response.drag_delta();
            self.camera.pan(d.x, d.y);
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                // egui: positive = wheel up; three.js deltaY < 0 for wheel up.
                self.camera.dolly_wheel(-scroll);
            }
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.selection = Selection::default();
        }
        if self.camera.update() {
            ui.ctx().request_repaint();
        }
        self.ease_head_pose(ui.ctx());

        let aspect = rect.width() / rect.height().max(1.0);
        if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            self.pick(p, rect, aspect);
        }

        let ppp = ui.ctx().pixels_per_point();
        let out = {
            let live = self.live.lock().unwrap();
            view::build_frame(
                &live,
                &self.settings,
                &self.camera,
                rect,
                ppp,
                &self.selection,
                &self.volume_settings,
                &mut self.volume_state,
                self.head_rotation,
                self.head_loaded,
                Instant::now(),
            )
        };
        self.pick_objects = out.pick_objects;
        self.pick_speakers = out.pick_speakers;

        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            ViewportCallback(Arc::new(out.frame)),
        ));

        let painter = ui.painter().with_clip_rect(rect);
        let mut labels = out.labels;
        labels.sort_by(|a, b| b.depth.total_cmp(&a.depth));
        for l in labels {
            painter.text(
                l.pos,
                Align2::CENTER_CENTER,
                l.text,
                egui::FontId::proportional(l.size),
                l.color,
            );
        }
    }

    /// `picking.js`: speakers first, then objects; empty hit deselects.
    fn pick(&mut self, pointer: Pos2, rect: Rect, aspect: f32) {
        let ndc_x = (pointer.x - rect.min.x) / rect.width() * 2.0 - 1.0;
        let ndc_y = 1.0 - (pointer.y - rect.min.y) / rect.height() * 2.0;
        let (origin, dir) = self
            .camera
            .ray(ndc_x, ndc_y, aspect, [rect.width(), rect.height()]);
        let hit = |c: Vec3, r: f32| -> Option<f32> {
            let t = (c - origin).dot(dir);
            if t <= 0.0 {
                return None;
            }
            ((origin + dir * t - c).length() <= r).then_some(t)
        };
        let mut best_speaker: Option<(f32, usize)> = None;
        for (index, pos, radius) in &self.pick_speakers {
            if let Some(t) = hit(*pos, *radius)
                && best_speaker.is_none_or(|(bt, _)| t < bt)
            {
                best_speaker = Some((t, *index));
            }
        }
        if let Some((_, index)) = best_speaker {
            self.selection = Selection {
                object: None,
                speaker: Some(index),
            };
            return;
        }
        let mut best_object: Option<(f32, &str)> = None;
        for (id, pos, radius) in &self.pick_objects {
            if let Some(t) = hit(*pos, *radius)
                && best_object.is_none_or(|(bt, _)| t < bt)
            {
                best_object = Some((t, id.as_str()));
            }
        }
        self.selection = Selection {
            object: best_object.map(|(_, id)| id.to_owned()),
            speaker: None,
        };
    }

    // -----------------------------------------------------------------------
    // Overlay panels
    // -----------------------------------------------------------------------

    fn panel_frame(ui: &egui::Ui) -> egui::Frame {
        egui::Frame::new()
            .fill(Color32::from_rgba_unmultiplied(18, 22, 28, 232))
            .stroke(egui::Stroke::new(1.0, Color32::from_gray(52)))
            .corner_radius(8.0)
            .inner_margin(PANEL_MARGIN)
            .shadow(ui.style().visuals.window_shadow)
    }

    fn left_panel(&mut self, ctx: &egui::Context) {
        let max_h = ctx.content_rect().height() - 2.0 * PANEL_MARGIN;
        egui::Area::new(egui::Id::new("left-panel"))
            .anchor(Align2::LEFT_TOP, [PANEL_MARGIN, PANEL_MARGIN])
            .show(ctx, |ui| {
                Self::panel_frame(ui).show(ui, |ui| {
                    ui.set_width(PANEL_WIDTH);
                    ui.set_max_height(max_h - 2.0 * PANEL_MARGIN);
                    ui.heading("Omniphony Studio");
                    ui.small("egui / wgpu — phase 1");
                    self.connection_line(ui);
                    ui.separator();
                    egui::ScrollArea::vertical()
                        .max_height(max_h - 110.0)
                        .auto_shrink([false, true])
                        .show(ui, |ui| self.left_sections(ui));
                });
            });
    }

    fn connection_line(&self, ui: &mut egui::Ui) {
        let port = self.osc_stats.listen_port.load(Ordering::Relaxed);
        let (dot, text) = match (
            self.args.register.as_deref(),
            self.osc_stats.registered.load(Ordering::Relaxed),
            self.osc_stats.since_last_packet(),
        ) {
            (Some(host), true, Some(d)) if d < Duration::from_secs(7) => (
                Color32::from_rgb(70, 200, 120),
                format!("registered with {host} · udp/{port}"),
            ),
            (Some(host), _, _) => (
                Color32::from_rgb(230, 170, 60),
                format!("waiting for {host} · udp/{port}"),
            ),
            (None, _, Some(d)) if d < Duration::from_secs(2) => (
                Color32::from_rgb(70, 200, 120),
                format!("receiving on udp/{port}"),
            ),
            (None, _, _) => (Color32::from_gray(120), format!("idle · udp/{port}")),
        };
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
            ui.painter().circle_filled(r.center(), 4.0, dot);
            ui.small(text);
        });
    }

    fn left_sections(&mut self, ui: &mut egui::Ui) {
        let s = &mut self.settings;
        egui::CollapsingHeader::new("Display")
            .default_open(true)
            .show(ui, |ui| {
                widgets::switch_row(ui, "Objects", &mut s.objects_visible);
                ui.horizontal(|ui| {
                    ui.label("Object display");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::ComboBox::from_id_salt("object-display-mode")
                            .selected_text(s.object_display_mode.label())
                            .width(150.0)
                            .show_ui(ui, |ui| {
                                for mode in ObjectDisplayMode::ALL {
                                    ui.selectable_value(
                                        &mut s.object_display_mode,
                                        mode,
                                        mode.label(),
                                    );
                                }
                            });
                    });
                });
                ui.add(
                    egui::Slider::new(&mut s.object_sphere_size, 0.03..=0.2)
                        .step_by(0.002)
                        .text("object size"),
                );
                widgets::switch_row(ui, "Object colours", &mut s.object_colors_enabled);
                widgets::switch_row(ui, "Object labels", &mut s.object_labels_enabled);
                widgets::switch_row(ui, "Effective render", &mut s.effective_render_enabled);
                widgets::switch_row(ui, "Grid (VBAP nodes)", &mut s.vbap_grid);
                ui.separator();
                widgets::switch_row(ui, "Speakers", &mut s.speakers_visible);
                widgets::switch_row(ui, "Speaker labels", &mut s.speaker_labels_enabled);
                ui.add(
                    egui::Slider::new(&mut s.speaker_size, 0.04..=0.2)
                        .step_by(0.002)
                        .text("speaker size"),
                );
            });

        egui::CollapsingHeader::new("Trails")
            .default_open(true)
            .show(ui, |ui| {
                widgets::switch_row(ui, "Show trails", &mut s.trails.enabled);
                ui.horizontal(|ui| {
                    ui.label("Mode");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::ComboBox::from_id_salt("trail-mode")
                            .selected_text(s.trails.mode.label())
                            .width(120.0)
                            .show_ui(ui, |ui| {
                                for mode in [TrailMode::Diffuse, TrailMode::Line] {
                                    ui.selectable_value(&mut s.trails.mode, mode, mode.label());
                                }
                            });
                    });
                });
                let mut ttl_s = s.trails.ttl.as_secs_f32();
                if ui
                    .add(
                        egui::Slider::new(&mut ttl_s, 1.0..=20.0)
                            .step_by(0.5)
                            .text("duration (s)"),
                    )
                    .changed()
                {
                    s.trails.ttl = Duration::from_secs_f32(ttl_s.max(0.5));
                }
                ui.add(
                    egui::Slider::new(&mut s.trails.teleport_threshold, 0.05..=2.0)
                        .step_by(0.05)
                        .text("teleport threshold"),
                );
            });

        let v = &mut self.volume_settings;
        egui::CollapsingHeader::new("Heatmaps")
            .default_open(false)
            .show(ui, |ui| {
                let combo = |ui: &mut egui::Ui, id: &str, label: &str, cm: &mut Colormap| {
                    ui.horizontal(|ui| {
                        ui.label(label);
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            egui::ComboBox::from_id_salt(id)
                                .selected_text(cm.label())
                                .width(130.0)
                                .show_ui(ui, |ui| {
                                    for c in Colormap::ALL {
                                        ui.selectable_value(cm, c, c.label());
                                    }
                                });
                        });
                    });
                };
                widgets::switch_row(ui, "Object energy field", &mut v.object_field_enabled);
                combo(ui, "object-colormap", "Colormap", &mut v.object_colormap);
                ui.add(
                    egui::Slider::new(&mut v.object_radius, 0.02..=0.5)
                        .step_by(0.01)
                        .text("falloff radius"),
                );
                ui.separator();
                widgets::switch_row(ui, "Global energy deviation", &mut v.global_enabled);
                ui.add(
                    egui::Slider::new(&mut v.global_scale_db, 1.0..=40.0)
                        .step_by(1.0)
                        .text("scale (dB)"),
                );
                ui.separator();
                widgets::switch_row(ui, "Speaker heatmap volume", &mut v.speaker_enabled);
                combo(ui, "speaker-colormap", "Colormap", &mut v.speaker_colormap);
                ui.separator();
                widgets::switch_row(ui, "Discontinuity", &mut v.discontinuity_enabled);
                ui.horizontal(|ui| {
                    ui.label("Mode");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::ComboBox::from_id_salt("discontinuity-mode")
                            .selected_text(match v.discontinuity_mode {
                                DiscontinuityMode::Gain => "Gain",
                                DiscontinuityMode::Centroid => "Centroid",
                            })
                            .width(130.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut v.discontinuity_mode,
                                    DiscontinuityMode::Gain,
                                    "Gain",
                                );
                                ui.selectable_value(
                                    &mut v.discontinuity_mode,
                                    DiscontinuityMode::Centroid,
                                    "Centroid",
                                );
                            });
                    });
                });
                ui.add(
                    egui::Slider::new(&mut v.discontinuity_scale, 0.05..=2.0)
                        .step_by(0.05)
                        .text("scale"),
                );
                ui.separator();
                ui.small("Common parameters");
                let mut res = v.resolution as f32;
                if ui
                    .add(
                        egui::Slider::new(&mut res, 8.0..=64.0)
                            .step_by(2.0)
                            .text("resolution"),
                    )
                    .changed()
                {
                    v.resolution = res.round() as u32;
                }
                ui.add(
                    egui::Slider::new(&mut v.opacity, 0.05..=1.0)
                        .step_by(0.05)
                        .text("opacity"),
                );
                ui.add(
                    egui::Slider::new(&mut v.mix, 0.0..=1.0)
                        .step_by(0.01)
                        .text("mix"),
                );
                ui.add(
                    egui::Slider::new(&mut v.gamma_accumulate, 1.0..=10.0)
                        .step_by(0.1)
                        .text("gamma accumulate"),
                );
                ui.add(
                    egui::Slider::new(&mut v.gamma_mip, 0.2..=3.0)
                        .step_by(0.05)
                        .text("gamma mip"),
                );
                let mut refresh = v.refresh_ms as f32;
                if ui
                    .add(
                        egui::Slider::new(&mut refresh, 40.0..=500.0)
                            .step_by(10.0)
                            .text("refresh (ms)"),
                    )
                    .changed()
                {
                    v.refresh_ms = refresh.round() as u32;
                }
                widgets::switch_row(ui, "Smooth interpolation", &mut v.smooth);
                widgets::switch_row(ui, "All bands", &mut v.all_bands);
                if !v.all_bands {
                    let mut band = v.band_index as f32;
                    if ui
                        .add(
                            egui::Slider::new(&mut band, 0.0..=7.0)
                                .step_by(1.0)
                                .text("band"),
                        )
                        .changed()
                    {
                        v.band_index = band.round() as usize;
                    }
                }
            });

        egui::CollapsingHeader::new("Live options (registry-driven)")
            .default_open(false)
            .show(ui, |ui| {
                for (spec, value) in OPTION_SCHEMA.iter().zip(self.options.iter_mut()) {
                    if widgets::option_row(ui, spec, value) {
                        log::info!(
                            "[options] {} = {:?} (logged, not sent to the renderer)",
                            spec.key,
                            value
                        );
                    }
                }
                ui.small(
                    "Widgets are generated from the option schema. Changes are logged, never sent.",
                );
            });

        egui::CollapsingHeader::new("Text: CJK and IME")
            .default_open(false)
            .show(ui, |ui| {
                ui.label("日本語: 音楽・効果音・対話");
                ui.label("中文: 环境声 · 对白 · 音效");
                ui.label("한국어: 음악 · 효과음");
                ui.add(
                    egui::TextEdit::singleline(&mut self.ime_text)
                        .hint_text("type here with an IME…")
                        .desired_width(f32::INFINITY),
                );
            });

        egui::CollapsingHeader::new("Stats")
            .default_open(true)
            .show(ui, |ui| {
                let (objects, epoch) = {
                    let live = self.live.lock().unwrap();
                    (live.app.sources.len(), live.snapshot_epoch)
                };
                egui::Grid::new("stats-grid")
                    .num_columns(2)
                    .spacing([16.0, 2.0])
                    .show(ui, |ui| {
                        let row = |ui: &mut egui::Ui, k: &str, v: String| {
                            ui.label(k);
                            ui.monospace(v);
                            ui.end_row();
                        };
                        row(ui, "frames/s", format!("{:.1}", self.frame_stats.fps));
                        row(
                            ui,
                            "frame interval",
                            format!("{:.2} ms", self.frame_stats.frame_ms),
                        );
                        row(
                            ui,
                            "OSC packets/s",
                            format!("{:.0}", self.proc_stats.packets_per_s),
                        );
                        row(ui, "objects", objects.to_string());
                        row(ui, "snapshot epoch", epoch.to_string());
                        row(ui, "RSS", format!("{:.0} MB", self.proc_stats.rss_mb));
                        row(
                            ui,
                            "process CPU",
                            format!("{:.1} %", self.proc_stats.cpu_percent),
                        );
                        row(
                            ui,
                            "OSC ignored",
                            self.osc_stats.ignored.load(Ordering::Relaxed).to_string(),
                        );
                    });
            });

        egui::CollapsingHeader::new("Camera")
            .default_open(false)
            .show(ui, |ui| {
                ui.small("Drag: orbit · middle drag / wheel: dolly · right drag: pan · click: select · Esc: clear");
                if ui.button("Reset view").clicked() {
                    self.camera.reset();
                }
            });
    }

    fn right_panel(&mut self, ctx: &egui::Context) {
        let max_h = ctx.content_rect().height() - 2.0 * PANEL_MARGIN;
        let (layout_name, speakers, objects): (
            String,
            Vec<String>,
            Vec<(String, String, bool, bool)>,
        ) = {
            let live = self.live.lock().unwrap();
            let name = live
                .app
                .layouts
                .iter()
                .find(|l| Some(&l.key) == live.app.selected_layout_key.as_ref())
                .map(|l| l.name.clone())
                .unwrap_or_else(|| "(none)".to_owned());
            let mut objects: Vec<(String, String, bool, bool)> = live
                .app
                .sources
                .iter()
                .map(|(id, src)| {
                    (
                        id.clone(),
                        view::objects::badge_code(id, src.name.as_deref()),
                        src.fixed.unwrap_or(false),
                        live.app.object_mutes.get(id).is_some_and(|m| *m != 0),
                    )
                })
                .collect();
            objects.sort_by(|a, b| match (a.0.parse::<u32>(), b.0.parse::<u32>()) {
                (Ok(x), Ok(y)) => x.cmp(&y),
                _ => a.0.cmp(&b.0),
            });
            (
                name,
                live.selected_speakers()
                    .iter()
                    .map(|s| s.id.clone())
                    .collect(),
                objects,
            )
        };
        egui::Area::new(egui::Id::new("right-panel"))
            .anchor(Align2::RIGHT_TOP, [-PANEL_MARGIN, PANEL_MARGIN])
            .show(ctx, |ui| {
                Self::panel_frame(ui).show(ui, |ui| {
                    ui.set_width(220.0);
                    ui.set_max_height(max_h - 2.0 * PANEL_MARGIN);
                    ui.heading(format!("Objects ({})", objects.len()));
                    egui::ScrollArea::vertical()
                        .id_salt("objects-list")
                        .max_height(max_h * 0.45)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (id, code, fixed, muted) in &objects {
                                let selected =
                                    self.selection.object.as_deref() == Some(id.as_str());
                                let mut text = format!("{id:>3}  {code}");
                                if *fixed {
                                    text.push_str("  (bed)");
                                }
                                if *muted {
                                    text.push_str("  M");
                                }
                                if ui.selectable_label(selected, text).clicked() {
                                    self.selection = Selection {
                                        object: (!selected).then(|| id.clone()),
                                        speaker: None,
                                    };
                                }
                            }
                            if objects.is_empty() {
                                ui.small("No objects yet. Feed OSC or run with --synthetic 64.");
                            }
                        });
                    ui.separator();
                    ui.heading(format!("Speakers · {layout_name}"));
                    egui::ScrollArea::vertical()
                        .id_salt("speakers-list")
                        .max_height(max_h * 0.3)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (index, name) in speakers.iter().enumerate() {
                                let selected = self.selection.speaker == Some(index);
                                if ui
                                    .selectable_label(selected, format!("{index:>2}  {name}"))
                                    .clicked()
                                {
                                    self.selection = Selection {
                                        object: None,
                                        speaker: (!selected).then_some(index),
                                    };
                                }
                            }
                        });
                });
            });
    }

    fn maybe_print_stats(&mut self) {
        if self.args.stats_interval <= 0.0
            || self.last_print.elapsed() < Duration::from_secs_f32(self.args.stats_interval)
        {
            return;
        }
        self.last_print = Instant::now();
        let objects = self.live.lock().unwrap().app.sources.len();
        println!(
            "stats t={:.0}s fps={:.1} frame_ms={:.2} osc_pkt_s={:.0} objects={} rss_mb={:.0} cpu_pct={:.1} events={} pointer_moves={} pointer_over={}",
            self.osc_stats.start.elapsed().as_secs_f32(),
            self.frame_stats.fps,
            self.frame_stats.frame_ms,
            self.proc_stats.packets_per_s,
            objects,
            self.proc_stats.rss_mb,
            self.proc_stats.cpu_percent,
            self.input_events,
            self.pointer_moves,
            self.pointer_over,
        );
        self.input_events = 0;
        self.pointer_moves = 0;
    }
}

impl eframe::App for StudioSpike {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.frame_stats.tick();
        ui.input(|i| {
            self.input_events += i.events.len() as u32;
            self.pointer_moves += i
                .events
                .iter()
                .filter(|e| matches!(e, egui::Event::PointerMoved(_)))
                .count() as u32;
            self.pointer_over = i.pointer.has_pointer();
        });
        self.proc_stats
            .sample(self.osc_stats.packets.load(Ordering::Relaxed));

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| self.viewport(ui));
        let ctx = ui.ctx().clone();
        self.left_panel(&ctx);
        self.right_panel(&ctx);
        self.maintain_gaintable_subscriptions();
        self.maybe_print_stats();
    }
}
