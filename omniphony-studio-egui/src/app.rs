//! The application: viewport in the central panel, fixed-extent panels
//! floated over it, camera input, picking, labels, stats.

use std::net::ToSocketAddrs;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Align2, Color32, Pos2, Rect};
use glam::{Quat, Vec3};

use crate::Args;
use crate::host::control::Ctl;
use crate::host::prefs::Prefs;
use crate::model::app_state::AppState;
use crate::model::layouts::load_layouts;
use crate::osc::dispatch::Live;
use crate::osc::{self, Control, ControlTx, OscStats, SharedLive};
use crate::render::camera::OrbitCamera;
use crate::render::{SceneRenderer, ViewportCallback};
use crate::stats::{FrameStats, ProcStats};
use crate::ui::layout::{OverlayLayout, Side};
use crate::view::volumes::{Colormap, DiscontinuityMode};
use crate::view::{
    self, ObjectDisplayMode, Selection, TrailMode, ViewSettings, VolumeSettings, VolumeState,
};
use crate::widgets::{self, OPTION_SCHEMA, OptionValue};

/// True when two layouts describe the same panels (they hold only floats and
/// flags, so a field-wise comparison is enough to know whether to persist).
fn layout_eq(a: &OverlayLayout, b: &OverlayLayout) -> bool {
    a.left.width == b.left.width
        && a.right.width == b.right.width
        && a.left.collapsed == b.left.collapsed
        && a.right.collapsed == b.right.collapsed
}

pub struct StudioSpike {
    pub(crate) args: Args,
    pub(crate) live: SharedLive,
    pub(crate) osc_stats: Arc<OscStats>,
    pub(crate) camera: OrbitCamera,
    pub(crate) selection: Selection,
    pub(crate) settings: ViewSettings,
    pub(crate) options: Vec<OptionValue>,
    pub(crate) ime_text: String,
    pub(crate) frame_stats: FrameStats,
    pub(crate) proc_stats: ProcStats,
    pub(crate) last_print: Instant,
    /// Input events seen since the last stats line, to attribute repaints.
    pub(crate) input_events: u32,
    pub(crate) pointer_moves: u32,
    pub(crate) pointer_over: bool,
    /// Pick lists of the last built frame.
    pub(crate) pick_objects: Vec<(String, Vec3, f32)>,
    pub(crate) pick_speakers: Vec<(usize, Vec3, f32)>,
    pub(crate) volume_settings: VolumeSettings,
    pub(crate) volume_state: VolumeState,
    pub(crate) head_loaded: bool,
    /// Eased head-pose rotation (`scene/head-pose.js`, slerp 0.4 per frame).
    pub(crate) head_rotation: Quat,
    pub(crate) control: ControlTx,
    /// Gain-table targets currently subscribed, and the last (re)subscribe.
    pub(crate) subscribed_tables: Vec<i64>,
    /// Side-panel widths and collapsed flags, persisted with the prefs.
    pub(crate) layout: OverlayLayout,
    pub(crate) prefs: Prefs,
    pub(crate) prefs_dirty: bool,
    pub(crate) prefs_dirty_since: Option<Instant>,
    /// Log overlay: expanded state and the filter box's text.
    pub(crate) log_expanded: bool,
    pub(crate) log_filter: String,
    /// `SharedState::realtime_seq`: monotonic stamp on realtime controls, so
    /// the renderer can drop updates that arrive out of order.
    pub(crate) realtime_seq: i32,
    /// OSC form fields (`osc_config.json`, shared with the Tauri Studio).
    pub(crate) osc_host: String,
    pub(crate) osc_port: u16,
    /// Config directory this environment is assigned (`OMNIPHONY_CONFIG_DIR`).
    pub(crate) config_dir: std::path::PathBuf,
    /// Handle on the renderer: every control the panels expose goes through it.
    pub(crate) ctl: Ctl,
    pub(crate) last_subscribe: Option<Instant>,
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

        let config_dir = crate::host::runtime_env::config_dir()
            .map(|dir| dir.join("studio"))
            .unwrap_or_else(|| args.layouts_dir.join(".studio-egui"));
        let mut prefs = crate::host::prefs::load(&config_dir);
        let mut layout = prefs.side_panels;
        layout.clamp_all(cc.egui_ctx.content_rect().width().max(800.0));
        prefs.side_panels = layout;
        let ctl = Ctl::new(control.clone());
        // The OSC form starts from the same file the Tauri Studio writes, so
        // both hosts point at the same renderer by default.
        let osc_config = crate::host::config::load_config(&config_dir);
        let (osc_host, osc_port) = match &args.register {
            Some(spec) => match spec.rsplit_once(':') {
                Some((host, port)) => (
                    host.to_owned(),
                    port.parse().unwrap_or(osc_config.osc_rx_port),
                ),
                None => (spec.clone(), osc_config.osc_rx_port),
            },
            None => (osc_config.host.clone(), osc_config.osc_rx_port),
        };

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
            layout,
            prefs,
            prefs_dirty: false,
            prefs_dirty_since: None,
            log_expanded: false,
            log_filter: String::new(),
            realtime_seq: 0,
            osc_host,
            osc_port,
            config_dir,
            ctl,
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

    /// Both side overlays. They float above the viewport, so resizing or
    /// collapsing one never changes the scene's size.
    fn overlays(&mut self, ctx: &egui::Context) {
        // `OverlayLayout` is `Copy`: take it out so the panel bodies can
        // borrow `self`, then write back what the chrome changed.
        let mut layout = self.layout;
        layout.clamp_all(ctx.content_rect().width());
        crate::ui::overlay::show(ctx, Side::Left, &mut layout, |ui| {
            ui.heading(crate::i18n::t("app.title"));
            ui.label(
                egui::RichText::new(crate::i18n::t("app.subtitle"))
                    .size(crate::ui::theme::FONT_SIZE_SMALL)
                    .color(crate::ui::theme::TEXT_MUTED),
            );
            self.connection_line(ui);
            let height = ui.available_height();
            egui::ScrollArea::vertical()
                .id_salt("overlay-left-scroll")
                .max_height(height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    self.osc_section(ui);
                    self.display_sections(ui);
                    self.tool_sections(ui);
                });
        });
        crate::ui::overlay::show(ctx, Side::Right, &mut layout, |ui| {
            let height = ui.available_height();
            egui::ScrollArea::vertical()
                .id_salt("overlay-right-scroll")
                .max_height(height)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    self.master_section(ui);
                    self.objects_section(ui);
                    self.speakers_section(ui);
                });
        });
        self.log_overlay(ctx, &layout);
        if !layout_eq(&layout, &self.layout) {
            self.layout = layout;
            self.prefs.side_panels = layout;
            self.prefs_dirty = true;
            self.prefs_dirty_since = None;
        }
    }

    /// Write the preferences out once the user has stopped dragging a panel
    /// edge (the web writes to `localStorage` on every change; a file wants a
    /// debounce).
    fn persist_prefs(&mut self) {
        if !self.prefs_dirty {
            return;
        }
        match self.prefs_dirty_since {
            Some(since) if since.elapsed() < Duration::from_millis(600) => {}
            Some(_) => {
                crate::host::prefs::save(&self.config_dir, &self.prefs);
                self.prefs_dirty = false;
                self.prefs_dirty_since = None;
            }
            None => self.prefs_dirty_since = Some(Instant::now()),
        }
    }

    /// Next value of the realtime sequence counter.
    pub(crate) fn next_realtime_seq(&mut self) -> i32 {
        self.realtime_seq = self.realtime_seq.wrapping_add(1);
        self.realtime_seq
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
        self.overlays(&ctx);
        self.maintain_gaintable_subscriptions();
        self.persist_prefs();
        self.maybe_print_stats();
    }
}
