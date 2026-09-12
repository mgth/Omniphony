//! The application: viewport in the central panel, fixed-extent panels
//! floated over it, camera input, picking, labels, stats.

use std::net::ToSocketAddrs;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use egui::{Align2, Pos2, Rect};
use glam::{Quat, Vec3};

use crate::Args;
use crate::model::app_state::AppState;
use crate::model::layouts::load_layouts;
use crate::osc::dispatch::Live;
use crate::osc::{self, OscStats, SharedLive};
use crate::prefs::Prefs;
use crate::render::camera::OrbitCamera;
use crate::render::{SceneRenderer, ViewportCallback};
use crate::stats::{FrameStats, ProcStats};
use crate::ui::layout::{OverlayLayout, Side};
use crate::view::{self, Selection, ViewSettings, VolumeSettings, VolumeState};

/// How long after a selection change the lists keep the selected row in view,
/// long enough for the pinned editor to settle on its height.
const REVEAL_WINDOW: Duration = Duration::from_millis(400);

/// How long the preferences wait for changes to settle before being written.
const PREFS_DEBOUNCE: Duration = Duration::from_millis(600);

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
    /// OSC form fields (`osc_config.json`, shared with the Tauri Studio).
    pub(crate) osc_host: String,
    pub(crate) osc_port: u16,
    /// `auto_start_renderer` / `keep_renderer_alive_on_quit`, as last saved:
    /// the switches write the file straight away, the rest of the form on
    /// Connect.
    pub(crate) osc_auto_start: bool,
    pub(crate) osc_keep_alive: bool,
    /// What the user's mpv.conf says about `ad=orender`, read when the OSC
    /// section opens (the file can change behind Studio's back) and dropped
    /// when it closes. `Err` is the read failure, shown in place of the path.
    pub(crate) mpv_orender:
        Option<Result<crate::host::commands::mpv_config::MpvOrenderStatus, String>>,
    /// Which half of the renderer panel is showing.
    pub(crate) renderer_tab: crate::panels::renderer::RendererTab,
    /// Named-pipe path remembered while the file output is switched off.
    pub(crate) audio_pipe_path: String,
    /// Speaker editor: tab, and the test pane's own settings (the web keeps
    /// these in `localStorage`, keyed `speakerTest.*`).
    pub(crate) speaker_tab: crate::panels::speaker_editor::SpeakerTab,
    pub(crate) speaker_test_mode: String,
    pub(crate) speaker_test_isolation: String,
    pub(crate) speaker_test_level_db: f32,
    /// When the speaker-test idle feed was last armed (None = not armed).
    pub(crate) idle_feed_armed_at: Option<Instant>,
    /// Speaker selection of the previous frame, to notice a change.
    pub(crate) last_speaker_selection: Option<usize>,
    /// The selection the lists last scrolled to, and until when they keep
    /// making sure its row is in view (see `reveal_selected_row`).
    pub(crate) revealed_selection: Selection,
    pub(crate) reveal_until: Option<Instant>,
    /// Room dimensions as the form holds them, and whether this panel is the
    /// one that last changed them.
    pub(crate) room_edit: Option<crate::panels::room::RoomDimensions>,
    pub(crate) room_editing: bool,
    /// The parametric HRTF settings. They travel inside the source string, so
    /// the renderer never echoes them: this side owns them, as the web does.
    pub(crate) pinna_preset: String,
    pub(crate) pinna_d_scale: f32,
    pub(crate) pinna_depth: f32,
    pub(crate) prtf_depth: f32,
    pub(crate) prtf_freq_scale: f32,
    /// Target latency being typed, until Apply.
    pub(crate) latency_target_edit: Option<f64>,
    /// Adaptive-controller fields edited but not yet applied.
    pub(crate) adaptive_edits: std::collections::BTreeMap<&'static str, f64>,
    /// Config profiles: the inline name editor, its text, and the pending
    /// delete confirmation.
    pub(crate) profile_editor: Option<crate::panels::profiles::NameEditor>,
    pub(crate) profile_name_edit: String,
    pub(crate) profile_name_focus: bool,
    pub(crate) profile_delete_confirm: Option<String>,
    /// A bulk delay tool waiting for its confirmation: both rewrite every
    /// speaker, so neither runs on a single click.
    pub(crate) delay_tool_confirm: Option<crate::panels::speaker_editor::DelayTool>,
    /// "Reset channel layout" waiting for its confirmation.
    pub(crate) virtual_bed_reset_confirm: bool,
    /// Object injection: whether the test signal is playing, whether the list's
    /// M button silenced it, and what a sheet gesture is locked to.
    pub(crate) object_test_playing: bool,
    pub(crate) object_test_muted: bool,
    pub(crate) object_test_drag: Option<crate::panels::object_test_sheet::Target>,
    pub(crate) object_test_focus: Option<usize>,
    /// The orbit path, rebuilt in place each frame so drawing it allocates
    /// nothing after the first.
    pub(crate) object_test_orbit: Vec<[f64; 3]>,
    /// Snap grid, keyed on the published interval counts it was built from.
    pub(crate) object_test_grid_cache: Option<([u32; 4], [Vec<f64>; 3])>,
    /// The renderer's fixed-channel catalogue, digested once per publication.
    pub(crate) channel_catalog: crate::panels::channel_editor::ChannelCatalog,
    /// Which coordinate table the channel editor is showing.
    pub(crate) channel_coord_mode: crate::panels::channel_editor::CoordMode,
    /// The sample rate being typed, until it is applied or abandoned.
    pub(crate) sample_rate_edit: Option<String>,
    /// An update check in flight, answering on its own thread.
    pub(crate) update_check: Option<crate::panels::updates::CheckHandle>,
    /// Which gradient stop each custom-colormap editor has selected.
    pub(crate) object_stop_selected: Option<usize>,
    pub(crate) speaker_stop_selected: Option<usize>,
    /// What was last mirrored onto the mpv overlay, and for which snapshot.
    pub(crate) overlay_pushed: Option<crate::panels::mpv_overlay::OverlayPrefs>,
    pub(crate) overlay_pushed_epoch: Option<u64>,
    /// Which long-form info modal is open, by its i18n key prefix.
    pub(crate) info_modal_open: Option<crate::ui::help::Overlay>,
    /// Which scene-effects flyout is open, if any.
    pub(crate) scene_fx_flyout_open: Option<crate::panels::scene_fx::Flyout>,
    /// The display settings panel over the scene-effects bar.
    pub(crate) display_panel_open: bool,
    /// Resample sparkline: whether it is showing, and what it has sampled.
    pub(crate) resample_plot_open: bool,
    pub(crate) resample_series: crate::panels::resample_plot::ResampleSeries,
    /// Diagnostics plot: the sampled series, when the plot started, whether it
    /// is frozen, and when publication was last re-asserted.
    pub(crate) diag_series: crate::panels::diag_plot::DiagSeries,
    pub(crate) diag_started: Instant,
    pub(crate) diag_paused: bool,
    pub(crate) diag_keepalive_at: Option<Instant>,
    /// What the edit gizmo is on, and where: the drag handlers' anchor, kept
    /// from the last frame and moved locally while a drag is in flight.
    pub(crate) gizmo_target: Option<(crate::view::gizmos::GizmoTarget, glam::Vec3)>,
    /// A gizmo drag in progress.
    pub(crate) gizmo_drag: Option<crate::panels::gizmo_drag::GizmoDrag>,
    /// The object the editor is holding in place, and until when: `None` is a
    /// pin that lasts as long as the drag does.
    pub(crate) channel_edit_pin: Option<(String, glam::Vec3, Option<Instant>)>,
    /// Where the frequency gauges were drawn last frame, so a click on one
    /// selects its speaker the way a click on the cube does.
    pub(crate) band_bar_hits: Vec<(usize, Rect)>,
    /// The measuring rectangle on the diagnostics plot, while one is drawn.
    pub(crate) diag_selection: Option<crate::panels::diag_plot::DiagSelection>,
    /// The orender binary this Studio would launch, resolved once at start-up.
    /// A renderer answering from anywhere else is not one we started.
    pub(crate) expected_orender_path: Option<String>,
    /// Hybrid backend: which tab of its panel is showing, and which curve
    /// point is selected.
    pub(crate) hybrid_tab: crate::panels::hybrid::HybridTab,
    pub(crate) hybrid_point: Option<usize>,
    /// The speaker a drag picked up, until it is dropped on another row.
    pub(crate) speaker_drag: Option<usize>,
    /// Where each speaker row was laid out last frame, in the order shown:
    /// what a drag measures its pointer against, and the height its empty
    /// slot keeps.
    pub(crate) speaker_row_rects: Vec<(usize, egui::Rect)>,
    /// How far below the dragged row's top the pointer took hold of it.
    pub(crate) speaker_drag_grab: f32,
    /// A drop sent to the renderer and not yet echoed back: the list keeps
    /// showing the new order until the layout does, rather than flicking
    /// back to the old one for the round trip.
    pub(crate) speaker_move_pending: Option<crate::panels::lists::PendingMove>,
    /// Whether the About box is showing.
    pub(crate) about_open: bool,
    /// The at-rest bed markers this host owns, and what they were built from.
    pub(crate) synthetic_bed_ids: Vec<String>,
    pub(crate) synthetic_bed_signature: Option<u64>,
    /// Config directory this environment is assigned (`OMNIPHONY_CONFIG_DIR`).
    pub(crate) config_dir: std::path::PathBuf,
    /// A handle on the context, so a worker thread can ask for the frame that
    /// shows what it found.
    pub(crate) ctx: egui::Context,
    /// The SOFA browser, while it is open.
    pub(crate) sofa_browser: Option<crate::panels::sofa_browser::SofaBrowser>,
    /// The backend file editor, while it is open.
    pub(crate) script_editor: Option<crate::panels::script_editor::ScriptEditor>,
    /// The auto-tune wizard, while it is open.
    pub(crate) auto_tune: Option<crate::panels::auto_tune::Wizard>,
    /// The host's own `SharedState`, kept for the whole session because the
    /// watchdog and the tracked child live in it: a fresh one per call would
    /// forget the renderer it just started.
    pub(crate) host: std::sync::Arc<crate::host::commands::SharedState>,
    /// When the watchdog last ticked, and since when the link has been down.
    pub(crate) watchdog_tick: Option<Instant>,
    pub(crate) disconnected_since: Option<Instant>,
    /// The OS service's state, and when it was last asked for. Asking means
    /// spawning a process, so it is not a per-frame question.
    pub(crate) service_status: Option<(Instant, bool, String)>,
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
        // The config is read before the listener starts, because the register
        // carries the metering choice: a client that registers with metering
        // off gets no meters until someone touches the switch, and each client
        // is subscribed on its own.
        let config_dir = crate::host::runtime_env::config_dir()
            .map(|dir| dir.join("studio"))
            .unwrap_or_else(|| args.layouts_dir.join(".studio-egui"));
        let osc_config = crate::host::config::load_config(&config_dir);
        app.osc_metering_enabled = Some(u8::from(osc_config.osc_metering_enabled));
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
        // Two wakers over one context. The listener's also nudges the core's
        // clock, since a packet may have given it something to do; the clock's
        // only repaints — nudging itself from its own waker would spin.
        let (clock, nudges) = crate::host::services::ServiceClock::new();
        let repaint: osc::Waker = {
            let ctx = cc.egui_ctx.clone();
            Arc::new(move || ctx.request_repaint())
        };
        let waker: osc::Waker = {
            let repaint = repaint.clone();
            let clock = clock.clone();
            Arc::new(move || {
                repaint();
                clock.nudge();
            })
        };
        let (port, control) = osc::spawn_listener(
            live.clone(),
            waker,
            osc_stats.clone(),
            osc::ListenerConfig {
                listen_port: args.listen_port,
                register,
                metering: osc_config.osc_metering_enabled,
            },
        )?;
        log::info!("[osc] listening on udp/{port}");
        if args.synthetic > 0 {
            let stop = (args.synthetic_stop_after > 0.0)
                .then(|| Duration::from_secs_f32(args.synthetic_stop_after));
            osc::spawn_synthetic(args.synthetic, args.rate, port, stop)?;
        }

        let mut prefs = crate::prefs::load(&config_dir);
        // The language is applied before the first frame, so nothing is drawn
        // in English and then redrawn.
        crate::i18n::set_locale(prefs.locale.as_deref().unwrap_or("auto"));
        let mut layout = prefs.side_panels;
        layout.clamp_all(cc.egui_ctx.content_rect().width().max(800.0));
        prefs.side_panels = layout;
        // The OSC form starts from the same file the Tauri Studio writes, so
        // both hosts point at the same renderer by default.
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

        let live_for_host = live.clone();
        let control_for_host = control.clone();
        let object_field = args.object_field;
        // The Display panel as the user left it, then the command line, which
        // wins where it forces something: `--no-trails` and `--object-field`
        // are asked for on this launch, the prefs only say what was last set.
        let mut settings = ViewSettings::default();
        let mut volume_settings = VolumeSettings::default();
        prefs.display.apply(&mut settings, &mut volume_settings);
        if args.no_trails {
            settings.trails.enabled = false;
        }
        if object_field {
            volume_settings.object_field_enabled = true;
        }
        let host = std::sync::Arc::new(crate::host::commands::SharedState {
            inner: live_for_host,
            osc_tx: control_for_host,
            config_dir: config_dir.clone(),
            listen_port: Arc::new(Mutex::new(port)),
            realtime_seq: std::sync::atomic::AtomicI32::new(0),
            renderer_child: Default::default(),
            watchdog: Default::default(),
            auto_tune_snapshot: Default::default(),
            paths: crate::host::commands::HostPaths::default(),
        });
        // The core's own clock: it sleeps until a service is due or the waker
        // nudges it, so an idle Studio wakes for nothing.
        crate::host::services::spawn(host.clone(), repaint, nudges)?;

        Ok(Self {
            args,
            live,
            osc_stats,
            camera: OrbitCamera::new(),
            selection: Selection::default(),
            settings,
            frame_stats: FrameStats::new(),
            proc_stats: ProcStats::new(),
            last_print: Instant::now(),
            input_events: 0,
            pointer_moves: 0,
            pointer_over: false,
            pick_objects: Vec::new(),
            pick_speakers: Vec::new(),
            volume_settings,
            volume_state: VolumeState::default(),
            head_loaded,
            head_rotation: Quat::IDENTITY,
            subscribed_tables: Vec::new(),
            layout,
            prefs,
            prefs_dirty: false,
            prefs_dirty_since: None,
            log_expanded: false,
            log_filter: String::new(),
            osc_host,
            osc_port,
            osc_auto_start: osc_config.auto_start_renderer,
            osc_keep_alive: osc_config.keep_renderer_alive_on_quit,
            mpv_orender: None,
            renderer_tab: Default::default(),
            audio_pipe_path: String::new(),
            speaker_tab: Default::default(),
            speaker_test_mode: "toggle".to_owned(),
            speaker_test_isolation: "test_only".to_owned(),
            speaker_test_level_db: -8.0,
            idle_feed_armed_at: None,
            last_speaker_selection: None,
            revealed_selection: Selection::default(),
            reveal_until: None,
            room_edit: None,
            room_editing: false,
            pinna_preset: "pbnh".to_owned(),
            pinna_d_scale: 100.0,
            pinna_depth: 100.0,
            prtf_depth: 100.0,
            prtf_freq_scale: 100.0,
            latency_target_edit: None,
            adaptive_edits: Default::default(),
            profile_editor: None,
            profile_name_edit: String::new(),
            profile_name_focus: false,
            profile_delete_confirm: None,
            delay_tool_confirm: None,
            virtual_bed_reset_confirm: false,
            object_test_playing: false,
            object_test_muted: false,
            object_test_drag: None,
            object_test_focus: None,
            object_test_orbit: Vec::new(),
            object_test_grid_cache: None,
            channel_catalog: Default::default(),
            channel_coord_mode: crate::panels::channel_editor::CoordMode::Cartesian,
            // No bundle here, so the resolver falls through to the paths a
            // native build actually has: the repo's own build, then the
            // executable's own directory.
            sample_rate_edit: None,
            update_check: None,
            object_stop_selected: None,
            speaker_stop_selected: None,
            overlay_pushed: None,
            overlay_pushed_epoch: None,
            info_modal_open: None,
            scene_fx_flyout_open: None,
            display_panel_open: false,
            resample_plot_open: false,
            resample_series: Default::default(),
            diag_series: Default::default(),
            diag_started: Instant::now(),
            diag_paused: false,
            diag_keepalive_at: None,
            gizmo_target: None,
            gizmo_drag: None,
            channel_edit_pin: None,
            band_bar_hits: Vec::new(),
            diag_selection: None,
            expected_orender_path: crate::host::commands::orender::expected_orender_path(
                &crate::host::commands::HostPaths::default(),
                None,
            ),
            hybrid_tab: "hybrid".to_owned(),
            hybrid_point: None,
            speaker_drag: None,
            speaker_row_rects: Vec::new(),
            speaker_drag_grab: 0.0,
            speaker_move_pending: None,
            about_open: false,
            synthetic_bed_ids: Vec::new(),
            synthetic_bed_signature: None,
            host,
            watchdog_tick: None,
            service_status: None,
            disconnected_since: Some(Instant::now()),
            config_dir,
            ctx: cc.egui_ctx.clone(),
            sofa_browser: None,
            script_editor: None,
            auto_tune: None,
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
                crate::host::commands::diag::unsubscribe_speaker_gaintable(&self.host);
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
                crate::host::commands::diag::subscribe_speaker_gaintable(
                    &self.host,
                    have_version,
                    target as i32,
                );
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

        let aspect = rect.width() / rect.height().max(1.0);
        // A gizmo drag takes the primary button before the camera does: an
        // orbit under a drag would move the thing being aimed with.
        if response.drag_started_by(egui::PointerButton::Primary)
            && let Some(p) = response.interact_pointer_pos()
        {
            self.begin_gizmo_drag(p, rect, aspect);
        }
        if self.gizmo_drag.is_some() {
            if let Some(p) = response.interact_pointer_pos() {
                self.update_gizmo_drag(p, rect, aspect);
            }
            // The drag ends when the button does, not when the pointer stops
            // moving: a pause mid-drag is not a release.
            if response.drag_stopped_by(egui::PointerButton::Primary)
                || ui.input(|i| !i.pointer.primary_down())
            {
                self.end_gizmo_drag();
            }
        }
        let dragging_gizmo = self.gizmo_drag.is_some();

        // OrbitControls: left rotate, middle dolly, right lens-shift pan, wheel dolly.
        if response.dragged_by(egui::PointerButton::Primary) && !dragging_gizmo {
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
            let (scroll, ctrl, shift) = ui.input(|i| {
                (
                    i.smooth_scroll_delta.y,
                    i.modifiers.command,
                    i.modifiers.shift,
                )
            });
            if scroll != 0.0 {
                // Held with a modifier the wheel moves the target, not the
                // camera: fine with shift, coarse with ctrl.
                if (ctrl || shift) && self.gizmo_wheel(scroll, shift) {
                } else {
                    // egui: positive = wheel up; three.js deltaY < 0 for wheel up.
                    self.camera.dolly_wheel(-scroll);
                }
            }
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.selection = Selection::default();
        }
        if self.camera.update() {
            ui.ctx().request_repaint();
        }
        self.ease_head_pose(ui.ctx());

        if response.clicked()
            && let Some(p) = response.interact_pointer_pos()
        {
            self.pick(p, rect, aspect);
        }

        // The curve editor's selection is the scene's too: the shape it draws
        // is what that point means.
        self.settings.hybrid_point = self.hybrid_point;
        // An expired pin is dropped here rather than in the drawing, so the
        // object goes back to the stream on the frame the pin runs out.
        if self
            .channel_edit_pin
            .as_ref()
            .and_then(|(_, _, until)| *until)
            .is_some_and(|until| Instant::now() >= until)
        {
            self.channel_edit_pin = None;
        }
        self.settings.channel_edit_pin = self
            .channel_edit_pin
            .as_ref()
            .map(|(id, at, _)| (id.clone(), *at));
        let ppp = ui.ctx().pixels_per_point();
        // One band for everything: the band cursor and the Heatmaps select
        // write `heatmap_band_index`, and the volumes read it from here.
        self.volume_settings.band_index = self.settings.heatmap_band_index;
        self.settings.heatmap_all_bands = self.volume_settings.all_bands;
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
        // A drag owns the anchor while it lasts: the frame's copy is the
        // position before the pointer moved.
        if self.gizmo_drag.is_none() {
            self.gizmo_target = out.gizmo_target;
        }

        let mut frame = out.frame;
        frame.backdrop = self.panel_backdrop(ui.ctx());
        ui.painter().add(egui_wgpu::Callback::new_paint_callback(
            rect,
            ViewportCallback(Arc::new(frame)),
        ));

        let painter = ui.painter().with_clip_rect(rect);
        // The gauges go under the labels, and far ones first, so a near
        // speaker's bar is not hidden behind a distant one's.
        let mut bars = out.band_bars;
        bars.sort_by(|a, b| b.depth.total_cmp(&a.depth));
        self.band_bar_hits.clear();
        for bar in &bars {
            bar.paint(&painter);
            self.band_bar_hits.push((bar.speaker, bar.rect()));
        }
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
        // A gauge is drawn over everything and picked before everything: it
        // sits beside its speaker precisely so it can be hit.
        if let Some((index, _)) = self
            .band_bar_hits
            .iter()
            .rev()
            .find(|(_, bar)| bar.contains(pointer))
        {
            self.selection.speaker = Some(*index);
            self.selection.object = None;
            return;
        }
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
        // A new selection opens its editor in the pinned slot at the foot of
        // the overlay, which shortens the list above and can hide the very
        // row that was picked. The web scrolls it back into view after layout
        // (`scrollIntoView({ block: 'nearest' })`); here the slot only settles
        // on its height a frame later, so the lists keep checking for a moment
        // rather than once.
        if self.selection != self.revealed_selection {
            self.revealed_selection = self.selection.clone();
            let picked = self.selection.object.is_some() || self.selection.speaker.is_some();
            self.reveal_until = picked.then(|| Instant::now() + REVEAL_WINDOW);
        }
        if self.reveal_until.is_some_and(|t| Instant::now() < t) {
            ctx.request_repaint();
        } else {
            self.reveal_until = None;
        }
        // `OverlayLayout` is `Copy`: take it out so the panel bodies can
        // borrow `self`, then write back what the chrome changed.
        let mut layout = self.layout;
        layout.clamp_all(ctx.content_rect().width());
        crate::ui::overlay::show(ctx, Side::Left, &mut layout, |ui| {
            self.brand_row(ui);
            self.connection_line(ui);
            self.profiles_row(ui);
            self.updates_panel(ui);
            self.language_row(ui);
            // What comes *in* sits on the left and what goes *out* on the
            // right, as in the web: the objects are the program arriving, so
            // they follow the input sections here, and their two editors take
            // the left overlay's pinned slot. The right panel keeps the output
            // chain, from the device to the speakers.
            if self.object_test_editor_open() || self.selected_channel().is_some() {
                crate::ui::overlay::pinned_slot(ui, "overlay-left-pinned", |ui| {
                    self.object_test_editor(ui);
                    self.channel_editor(ui);
                });
            }
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show_inside(ui, |ui| {
                    let height = ui.available_height();
                    crate::ui::overlay::panel_scroll(
                        ui,
                        "overlay-left-scroll",
                        Some(height),
                        |ui| {
                            self.osc_section(ui);
                            self.audio_input_section(ui);
                            self.sources_2d_section(ui);
                            self.room_geometry_section(ui);
                            self.drc_section(ui);
                            self.objects_section(ui);
                            self.tool_sections(ui);
                        },
                    );
                });
        });
        crate::ui::overlay::show(ctx, Side::Right, &mut layout, |ui| {
            if self.selection.speaker.is_some() {
                crate::ui::overlay::pinned_slot(ui, "overlay-right-pinned", |ui| {
                    self.speaker_editor(ui);
                });
            }
            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show_inside(ui, |ui| {
                    let height = ui.available_height();
                    crate::ui::overlay::panel_scroll(
                        ui,
                        "overlay-right-scroll",
                        Some(height),
                        |ui| {
                            self.audio_output_section(ui);
                            self.latency_section(ui);
                            self.diagnostics_section(ui);
                            self.master_section(ui);
                            self.renderer_section(ui);
                            self.headphones_section(ui);
                            self.speakers_section(ui);
                        },
                    );
                });
        });
        self.log_overlay(ctx, &layout);
        self.scene_fx_bar(ctx);
        self.save_footer(ctx);
        self.band_cursor(ctx, &layout);
        self.about_modal(ctx);
        self.info_modal(ctx);
        self.delay_tool_modal(ctx);
        self.virtual_bed_reset_modal(ctx);
        crate::ui::help::end_frame(ctx);
        self.sofa_browser_modal(ctx);
        self.script_editor_modal(ctx);
        self.auto_tune_modal(ctx);
        self.auto_tune_quit_guard(ctx);
        if !layout_eq(&layout, &self.layout) {
            self.layout = layout;
            self.prefs.side_panels = layout;
            self.prefs_dirty = true;
            self.prefs_dirty_since = None;
        }
        // The web writes its display prefs to `localStorage` on every change;
        // here a change marks the file for the same debounced write as the
        // panel widths. The comparison allocates nothing, so it can run every
        // frame; the snapshot is only rebuilt when a setting actually moved.
        if !self
            .prefs
            .display
            .matches(&self.settings, &self.volume_settings)
        {
            let next =
                crate::prefs::display::DisplayPrefs::capture(&self.settings, &self.volume_settings);
            self.prefs.display = next;
            self.mark_prefs_dirty();
        }
    }

    /// Where the two side panels sit, in framebuffer pixels, so the renderer
    /// can blur what lies behind them (`backdrop-filter: blur(8px)` in the
    /// web). A folded panel is a button, not glass, and gets none. The rects
    /// are last frame's — the panels are laid out after the viewport — so a
    /// panel edge being dragged is one frame behind, which does not show.
    fn panel_backdrop(&self, ctx: &egui::Context) -> crate::render::Backdrop {
        let ppp = ctx.pixels_per_point();
        let mut backdrop = crate::render::Backdrop {
            radius_px: f32::from(crate::ui::theme::PANEL_RADIUS) * ppp,
            ..Default::default()
        };
        for (id, folded) in [
            ("overlay-left", self.layout.left.collapsed),
            ("overlay-right", self.layout.right.collapsed),
            (
                crate::panels::scene_fx::DISPLAY_PANEL_ID,
                !self.display_panel_open,
            ),
        ] {
            if folded {
                continue;
            }
            if let Some(r) = ctx.memory(|m| m.area_rect(egui::Id::new(id))) {
                backdrop.rects[backdrop.count as usize] =
                    [r.min.x * ppp, r.min.y * ppp, r.max.x * ppp, r.max.y * ppp];
                backdrop.count += 1;
            }
        }
        backdrop
    }

    /// Write the preferences out once the user has stopped dragging a panel
    /// edge (the web writes to `localStorage` on every change; a file wants a
    /// debounce).
    ///
    /// The window only repaints when something happens, so waiting for "the
    /// next frame after the delay" could wait for ever: with the renderer idle
    /// the frames stop, and a toggled setting or a dragged panel edge was
    /// never written. The debounce asks for the frame it is waiting on.
    fn persist_prefs(&mut self, ctx: &egui::Context) {
        if !self.prefs_dirty {
            return;
        }
        match self.prefs_dirty_since {
            Some(since) if since.elapsed() < PREFS_DEBOUNCE => {
                ctx.request_repaint_after(PREFS_DEBOUNCE.saturating_sub(since.elapsed()));
            }
            Some(_) => {
                crate::prefs::save(&self.config_dir, &self.prefs);
                self.prefs_dirty = false;
                self.prefs_dirty_since = None;
            }
            None => {
                self.prefs_dirty_since = Some(Instant::now());
                ctx.request_repaint_after(PREFS_DEBOUNCE);
            }
        }
    }

    /// Mark the preferences file for a (debounced) rewrite, restarting the
    /// debounce so a burst of changes writes once.
    pub(crate) fn mark_prefs_dirty(&mut self) {
        self.prefs_dirty = true;
        self.prefs_dirty_since = None;
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
        if self.last_speaker_selection != self.selection.speaker {
            self.last_speaker_selection = self.selection.speaker;
            self.follow_speaker_selection();
        }
        self.refresh_channel_catalog();
        self.sync_virtual_bed_objects(false);
        self.maintain_mpv_overlay();
        self.maintain_renderer_watchdog();
        self.maintain_object_test_source();
        self.maintain_test_idle_feed();
        self.check_recompute_ack(&ctx);
        self.maintain_gaintable_subscriptions();
        self.persist_prefs(&ctx);
        self.maybe_print_stats();
    }

    /// `beforeunload`: a test left playing would outlive the window, so the
    /// renderer is told to stop before this host goes away.
    fn on_exit(&mut self) {
        self.stop_speaker_test();
        self.stop_object_test();
        self.stop_launched_renderer();
    }
}
