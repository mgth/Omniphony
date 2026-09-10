//! The spike application: viewport in the central panel, fixed-extent panels
//! floated over it, stats, picking, labels.

use std::net::ToSocketAddrs;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Align2, Color32, Pos2, Rect};
use glam::{Mat4, Vec3, Vec4};

use crate::Args;
use crate::osc::{self, OscStats};
use crate::render::camera::OrbitCamera;
use crate::render::{
    FrameData, LineVertex, SceneRenderer, SphereInstance, ViewportCallback, hsv_linear, linear_rgba,
};
use crate::scene::{Scene, SharedScene};
use crate::stats::{FrameStats, ProcStats};
use crate::widgets::{self, OPTION_SCHEMA, OptionValue};

const STALE_AFTER: Duration = Duration::from_secs(10);
const PANEL_WIDTH: f32 = 300.0;
const PANEL_MARGIN: f32 = 12.0;
const SPEAKER_RADIUS: f32 = 0.055;
const HEAD_RADIUS: f32 = 0.09;

struct Display {
    show_labels: bool,
    show_speakers: bool,
    show_grid: bool,
    sphere_radius: f32,
    label_size: f32,
}

struct Label {
    pos: Pos2,
    text: String,
    color: Color32,
    /// View depth, for drawing far labels first.
    depth: f32,
}

pub struct StudioSpike {
    args: Args,
    scene: SharedScene,
    osc_stats: Arc<OscStats>,
    camera: OrbitCamera,
    selected: Option<u32>,
    display: Display,
    options: Vec<OptionValue>,
    ime_text: String,
    frame_stats: FrameStats,
    proc_stats: ProcStats,
    last_print: Instant,
    /// Cached static geometry, rebuilt when the grid toggle changes.
    lines: Vec<LineVertex>,
    lines_with_grid: bool,
    /// Input events seen since the last stats line, to attribute repaints.
    input_events: u32,
    pointer_moves: u32,
    pointer_over: bool,
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
        rs.renderer
            .write()
            .callback_resources
            .insert(SceneRenderer::new(&rs.device, rs.target_format));

        let scene: SharedScene = Arc::new(Mutex::new(Scene::default()));
        match crate::layout::load(&args.layout) {
            Ok((name, speakers)) => {
                log::info!("[layout] {name}: {} speakers", speakers.len());
                let mut s = scene.lock().unwrap();
                s.layout_name = name;
                s.speakers = speakers;
            }
            Err(e) => log::warn!("[layout] {e}"),
        }

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
        let port = osc::spawn_listener(
            scene.clone(),
            cc.egui_ctx.clone(),
            osc_stats.clone(),
            osc::ListenerConfig {
                listen_port: args.listen_port,
                register,
            },
        )?;
        log::info!("[osc] listening on udp/{port}");
        if args.synthetic > 0 {
            let stop = (args.synthetic_stop_after > 0.0)
                .then(|| Duration::from_secs_f32(args.synthetic_stop_after));
            osc::spawn_synthetic(args.synthetic, args.rate, port, stop)?;
        }

        let display = Display {
            show_labels: true,
            show_speakers: true,
            show_grid: true,
            sphere_radius: 0.07,
            label_size: 13.0,
        };
        let lines_with_grid = display.show_grid;
        let lines = scene_lines(lines_with_grid);
        Ok(Self {
            args,
            scene,
            osc_stats,
            camera: OrbitCamera::new(),
            selected: None,
            display,
            options: OPTION_SCHEMA.iter().map(|s| s.default.to_value()).collect(),
            ime_text: String::new(),
            frame_stats: FrameStats::new(),
            proc_stats: ProcStats::new(),
            last_print: Instant::now(),
            lines_with_grid,
            lines,
            input_events: 0,
            pointer_moves: 0,
            pointer_over: false,
        })
    }

    // -----------------------------------------------------------------------
    // Viewport
    // -----------------------------------------------------------------------

    fn viewport(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        let response = ui.allocate_rect(rect, egui::Sense::click_and_drag());

        if response.dragged_by(egui::PointerButton::Primary) {
            let d = response.drag_delta();
            self.camera.orbit(d.x, d.y);
        }
        if response.dragged_by(egui::PointerButton::Secondary)
            || response.dragged_by(egui::PointerButton::Middle)
        {
            let d = response.drag_delta();
            self.camera.pan(d.x, d.y);
        }
        if response.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                self.camera.zoom(scroll);
            }
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.selected = None;
        }

        let aspect = rect.width() / rect.height().max(1.0);
        let view_proj = self.camera.view_proj(aspect);

        if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            self.pick(p, rect, aspect);
        }

        if self.lines_with_grid != self.display.show_grid {
            self.lines = scene_lines(self.display.show_grid);
            self.lines_with_grid = self.display.show_grid;
        }

        let ppp = ui.ctx().pixels_per_point();
        let size_px = [
            (rect.width() * ppp).round().max(1.0) as u32,
            (rect.height() * ppp).round().max(1.0) as u32,
        ];

        let (instances, labels) = self.build_frame(view_proj, rect);
        let frame = Arc::new(FrameData {
            instances,
            lines: self.lines.clone(),
            view_proj,
            cam_pos: self.camera.eye(),
            light_dir: Vec3::new(0.35, -0.5, 0.8).normalize(),
            size_px,
            clear: [0.031, 0.037, 0.047, 1.0],
        });
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            ViewportCallback(frame),
        ));

        if self.display.show_labels {
            let painter = ui.painter().with_clip_rect(rect);
            let font = egui::FontId::proportional(self.display.label_size);
            let mut labels = labels;
            labels.sort_by(|a, b| b.depth.total_cmp(&a.depth));
            for l in labels {
                painter.text(l.pos, Align2::CENTER_BOTTOM, l.text, font.clone(), l.color);
            }
        }
    }

    /// Instances for objects, speakers and the listener head, plus the screen
    /// positions of the labels to draw over the callback.
    fn build_frame(&self, view_proj: Mat4, rect: Rect) -> (Vec<SphereInstance>, Vec<Label>) {
        let scene = self.scene.lock().unwrap();
        let mut instances = Vec::with_capacity(scene.objects.len() + scene.speakers.len() + 1);
        let mut labels = Vec::with_capacity(instances.capacity());

        let project = |p: [f32; 3]| -> Option<(Pos2, f32)> {
            let clip = view_proj * Vec4::new(p[0], p[1], p[2], 1.0);
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

        instances.push(SphereInstance {
            center: [0.0, 0.0, 0.0],
            radius: HEAD_RADIUS,
            color: linear_rgba(200, 205, 212, 1.0),
        });

        if self.display.show_speakers {
            for sp in &scene.speakers {
                let color = if sp.spatialize {
                    linear_rgba(120, 130, 145, 1.0)
                } else {
                    linear_rgba(90, 70, 70, 1.0)
                };
                instances.push(SphereInstance {
                    center: sp.pos,
                    radius: SPEAKER_RADIUS,
                    color,
                });
                if let Some((pos, depth)) = project(sp.pos) {
                    labels.push(Label {
                        pos: pos - egui::vec2(0.0, 8.0),
                        text: sp.name.clone(),
                        color: Color32::from_gray(150),
                        depth,
                    });
                }
            }
        }

        for obj in scene.objects.values() {
            let selected = self.selected == Some(obj.id);
            let color = if selected {
                linear_rgba(255, 210, 60, 1.0)
            } else if obj.fixed {
                linear_rgba(90, 150, 230, 1.0)
            } else {
                hsv_linear(obj.id as f32 * 47.0, 0.62, 0.95, 1.0)
            };
            let radius = if selected {
                self.display.sphere_radius * 1.3
            } else {
                self.display.sphere_radius
            };
            instances.push(SphereInstance {
                center: obj.pos,
                radius,
                color,
            });
            if let Some((pos, depth)) = project(obj.pos) {
                labels.push(Label {
                    pos: pos - egui::vec2(0.0, 10.0 + 60.0 * radius / depth.max(0.5)),
                    text: obj.label.clone(),
                    color: if selected {
                        Color32::from_rgb(255, 220, 90)
                    } else {
                        Color32::from_gray(225)
                    },
                    depth,
                });
            }
        }
        (instances, labels)
    }

    fn pick(&mut self, pointer: Pos2, rect: Rect, aspect: f32) {
        let ndc_x = (pointer.x - rect.min.x) / rect.width() * 2.0 - 1.0;
        let ndc_y = 1.0 - (pointer.y - rect.min.y) / rect.height() * 2.0;
        let (origin, dir) = self.camera.ray(ndc_x, ndc_y, aspect);
        let radius = self.display.sphere_radius * 1.15;
        let scene = self.scene.lock().unwrap();
        let mut best: Option<(f32, u32)> = None;
        for obj in scene.objects.values() {
            let c = Vec3::from_array(obj.pos);
            let t = (c - origin).dot(dir);
            if t <= 0.0 {
                continue;
            }
            let closest = origin + dir * t;
            if (closest - c).length() <= radius && best.is_none_or(|(bt, _)| t < bt) {
                best = Some((t, obj.id));
            }
        }
        self.selected = best.map(|(_, id)| id);
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
                    ui.small("egui / wgpu spike — phase 0");
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
        egui::CollapsingHeader::new("Live options (registry-driven)")
            .default_open(true)
            .show(ui, |ui| {
                for (spec, value) in OPTION_SCHEMA.iter().zip(self.options.iter_mut()) {
                    if widgets::option_row(ui, spec, value) {
                        log::info!(
                            "[options] {} = {:?} (spike: logged, not sent to the renderer)",
                            spec.key,
                            value
                        );
                    }
                }
                ui.small(
                    "Widgets are generated from the option schema. Changes are logged, never sent.",
                );
            });

        egui::CollapsingHeader::new("Display")
            .default_open(true)
            .show(ui, |ui| {
                widgets::switch_row(ui, "Labels", &mut self.display.show_labels);
                widgets::switch_row(ui, "Speakers", &mut self.display.show_speakers);
                widgets::switch_row(ui, "Floor grid", &mut self.display.show_grid);
                ui.add(
                    egui::Slider::new(&mut self.display.sphere_radius, 0.02..=0.16)
                        .text("object radius"),
                );
                ui.add(
                    egui::Slider::new(&mut self.display.label_size, 9.0..=22.0).text("label size"),
                );
            });

        egui::CollapsingHeader::new("Text: CJK and IME")
            .default_open(true)
            .show(ui, |ui| {
                ui.label("日本語: 音楽・効果音・対話");
                ui.label("中文: 环境声 · 对白 · 音效");
                ui.label("한국어: 음악 · 효과음");
                ui.add(
                    egui::TextEdit::singleline(&mut self.ime_text)
                        .hint_text("type here with an IME…")
                        .desired_width(f32::INFINITY),
                );
                if !self.ime_text.is_empty() {
                    ui.small(format!(
                        "{} chars, {} bytes",
                        self.ime_text.chars().count(),
                        self.ime_text.len()
                    ));
                }
            });

        egui::CollapsingHeader::new("Stats")
            .default_open(true)
            .show(ui, |ui| {
                let objects = self.scene.lock().unwrap().objects.len();
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
                ui.small("Repaints are driven by OSC packets; with no traffic the window idles.");
            });

        egui::CollapsingHeader::new("Camera")
            .default_open(false)
            .show(ui, |ui| {
                ui.small("Drag: orbit · right/middle drag: pan · wheel: zoom · click: select · Esc: clear");
                if ui.button("Reset view").clicked() {
                    self.camera = OrbitCamera::new();
                }
            });
    }

    fn right_panel(&mut self, ctx: &egui::Context) {
        let max_h = ctx.content_rect().height() - 2.0 * PANEL_MARGIN;
        egui::Area::new(egui::Id::new("right-panel"))
            .anchor(Align2::RIGHT_TOP, [-PANEL_MARGIN, PANEL_MARGIN])
            .show(ctx, |ui| {
                Self::panel_frame(ui).show(ui, |ui| {
                    ui.set_width(220.0);
                    ui.set_max_height(max_h - 2.0 * PANEL_MARGIN);
                    let (layout_name, speakers, objects): (
                        String,
                        Vec<String>,
                        Vec<(u32, String, bool)>,
                    ) = {
                        let s = self.scene.lock().unwrap();
                        (
                            s.layout_name.clone(),
                            s.speakers.iter().map(|sp| sp.name.clone()).collect(),
                            s.objects
                                .values()
                                .map(|o| (o.id, o.label.clone(), o.fixed))
                                .collect(),
                        )
                    };
                    ui.heading(format!("Objects ({})", objects.len()));
                    egui::ScrollArea::vertical()
                        .max_height(max_h * 0.55)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            for (id, label, fixed) in &objects {
                                let selected = self.selected == Some(*id);
                                let text = if *fixed {
                                    format!("{id:>3}  {label}  (bed)")
                                } else {
                                    format!("{id:>3}  {label}")
                                };
                                if ui.selectable_label(selected, text).clicked() {
                                    self.selected = if selected { None } else { Some(*id) };
                                }
                            }
                            if objects.is_empty() {
                                ui.small("No objects yet. Feed OSC or run with --synthetic 64.");
                            }
                        });
                    ui.separator();
                    ui.heading(format!("Layout: {layout_name}"));
                    ui.small(speakers.join(" · "));
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
        let objects = self.scene.lock().unwrap().objects.len();
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
        self.scene.lock().unwrap().prune(STALE_AFTER);

        egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| self.viewport(ui));
        let ctx = ui.ctx().clone();
        self.left_panel(&ctx);
        self.right_panel(&ctx);
        self.maybe_print_stats();
    }
}

/// Room box, optional floor grid, and the axis triad at the listener.
fn scene_lines(with_grid: bool) -> Vec<LineVertex> {
    let mut v: Vec<LineVertex> = Vec::with_capacity(320);
    let mut seg = |a: [f32; 3], b: [f32; 3], c: [f32; 4]| {
        v.push(LineVertex { pos: a, color: c });
        v.push(LineVertex { pos: b, color: c });
    };
    let edge = linear_rgba(95, 105, 120, 1.0);
    for &x in &[-1.0f32, 1.0] {
        for &y in &[-1.0f32, 1.0] {
            seg([x, y, -1.0], [x, y, 1.0], edge);
        }
    }
    for &z in &[-1.0f32, 1.0] {
        for &x in &[-1.0f32, 1.0] {
            seg([x, -1.0, z], [x, 1.0, z], edge);
        }
        for &y in &[-1.0f32, 1.0] {
            seg([-1.0, y, z], [1.0, y, z], edge);
        }
    }
    if with_grid {
        let grid = linear_rgba(60, 68, 80, 0.8);
        let n = 8;
        for i in 0..=n {
            let t = -1.0 + 2.0 * i as f32 / n as f32;
            seg([t, -1.0, -1.0], [t, 1.0, -1.0], grid);
            seg([-1.0, t, -1.0], [1.0, t, -1.0], grid);
        }
    }
    let len = 0.35;
    seg([0.0; 3], [len, 0.0, 0.0], linear_rgba(230, 80, 80, 1.0));
    seg([0.0; 3], [0.0, len, 0.0], linear_rgba(80, 210, 100, 1.0));
    seg([0.0; 3], [0.0, 0.0, len], linear_rgba(90, 140, 240, 1.0));
    v
}
