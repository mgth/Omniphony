// Prevents an additional console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app_state;
mod commands;
mod config;
mod engine_deploy;
mod layouts;
mod osc_listener;
mod osc_parser;

use std::path::PathBuf;
use std::sync::atomic::AtomicI32;
use std::sync::{Arc, Mutex};

use app_state::AppState;
use config::load_config;
use osc_listener::{spawn_osc_task, OscControlMsg};
use tauri::{Manager, State};
use tokio::sync::mpsc::UnboundedSender;

// Tauri command handlers grouped into themed modules under `commands/`. Glob-
// imported so `generate_handler!` can keep referring to them by bare name.
use commands::app::*;
use commands::audio::*;
use commands::binaural::*;
use commands::diag::*;
use commands::engine::*;
use commands::gain::*;
use commands::input::*;
use commands::layout_io::*;
use commands::mpv_overlay::*;
use commands::orender::*;
use commands::render::*;
use commands::resampling::*;
use commands::sofa_browser::*;
use commands::speakers::*;

// ── shared state wrapper ──────────────────────────────────────────────────

/// State of the local-renderer auto-start watchdog (see `osc_listener`).
#[derive(Default)]
pub(crate) struct WatchdogControl {
    /// Consecutive fast-fail spawn attempts since the last re-arm.
    pub(crate) attempts: u8,
    /// No spawn before this instant (backoff after a fast-failing child).
    pub(crate) cooldown_until: Option<std::time::Instant>,
    /// When the current child was spawned (fast-fail detection window).
    pub(crate) last_spawn_at: Option<std::time::Instant>,
    /// Set on a renderer goodbye broadcast: run the watchdog checks as soon
    /// as this is ~500 ms old instead of waiting out the heartbeat timeout.
    pub(crate) check_requested_at: Option<std::time::Instant>,
    /// Set by a manual "Stop orender": keep the auto-start watchdog from
    /// immediately respawning the renderer the user just stopped. Cleared by
    /// any re-arm (a manual launch or a settings save).
    pub(crate) suppressed: bool,
}

impl WatchdogControl {
    pub(crate) fn rearm(&mut self) {
        self.attempts = 0;
        self.cooldown_until = None;
        self.suppressed = false;
    }
}

pub(crate) struct SharedState {
    pub(crate) inner: Arc<Mutex<AppState>>,
    pub(crate) osc_tx: Arc<Mutex<Option<UnboundedSender<OscControlMsg>>>>,
    pub(crate) config_dir: PathBuf,
    pub(crate) listen_port: Arc<Mutex<u16>>,
    pub(crate) realtime_seq: AtomicI32,
    pub(crate) auto_tune_snapshot: Arc<Mutex<Option<serde_json::Value>>>,
    /// Local renderer process spawned by Studio (manual launch or watchdog).
    pub(crate) renderer_child: Arc<Mutex<Option<std::process::Child>>>,
    pub(crate) watchdog: Arc<Mutex<WatchdogControl>>,
}

// ── helper ────────────────────────────────────────────────────────────────

pub(crate) fn send_control(
    tx: &Arc<Mutex<Option<UnboundedSender<OscControlMsg>>>>,
    msg: OscControlMsg,
) {
    if let Some(tx) = tx.lock().unwrap().as_ref() {
        let _ = tx.send(msg);
    }
}

pub(crate) fn send_json_control(
    tx: &Arc<Mutex<Option<UnboundedSender<OscControlMsg>>>>,
    address: &str,
    payload: serde_json::Value,
) {
    let Ok(value) = serde_json::to_string(&payload) else {
        return;
    };
    send_control(
        tx,
        OscControlMsg::SendString {
            address: address.to_string(),
            value,
        },
    );
}

pub(crate) fn send_distance_metric(state: &State<SharedState>, address: &str, value: String) {
    let normalized = value.trim().to_ascii_lowercase();
    if !matches!(normalized.as_str(), "spherical" | "chebyshev") {
        return;
    }
    send_control(
        &state.osc_tx,
        OscControlMsg::SendString {
            address: address.to_string(),
            value: normalized,
        },
    );
}

// ── main ─────────────────────────────────────────────────────────────────

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                let decoded = image::load_from_memory(include_bytes!("../icons/icon.png"))
                    .expect("failed to decode window icon")
                    .into_rgba8();
                let (width, height) = decoded.dimensions();
                let window_icon = tauri::image::Image::new_owned(decoded.into_raw(), width, height);
                let _ = window.set_icon(window_icon);
            }

            // Publish the bundled engine library (liborender) to the per-user
            // path external consumers (the forked mpv) load it from.
            engine_deploy::deploy(app);

            let config_dir = app
                .path()
                .app_config_dir()
                .expect("could not resolve app config dir");

            let osc_cfg = load_config(&config_dir);

            // layouts dir: bundled as a resource
            let layouts_dir = app
                .path()
                .resource_dir()
                .map(|d| d.join("layouts"))
                .unwrap_or_else(|_| PathBuf::from("layouts"));

            let loaded_layouts = layouts::load_layouts(&layouts_dir);

            let mut initial_state = AppState::new(loaded_layouts);
            initial_state.osc_metering_enabled =
                Some(if osc_cfg.osc_metering_enabled { 1 } else { 0 });
            let app_state = Arc::new(Mutex::new(initial_state));
            let osc_tx: Arc<Mutex<Option<UnboundedSender<OscControlMsg>>>> =
                Arc::new(Mutex::new(None));
            let listen_port = Arc::new(Mutex::new(0u16));

            let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<OscControlMsg>();
            *osc_tx.lock().unwrap() = Some(tx);

            let shared = SharedState {
                inner: app_state.clone(),
                osc_tx: osc_tx.clone(),
                config_dir,
                listen_port: listen_port.clone(),
                realtime_seq: AtomicI32::new(0),
                auto_tune_snapshot: Arc::new(Mutex::new(None)),
                renderer_child: Arc::new(Mutex::new(None)),
                watchdog: Arc::new(Mutex::new(WatchdogControl::default())),
            };
            app.manage(shared);

            spawn_osc_task(
                app.handle().clone(),
                app_state,
                osc_cfg.host,
                osc_cfg.osc_port,
                osc_cfg.osc_rx_port,
                rx,
                listen_port.clone(),
            );

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            debug_memory_stats,
            debug_state_sizes,
            debug_write_memory_csv,
            get_state,
            get_osc_config,
            renderer_is_local,
            get_about_info,
            save_osc_config,
            launch_orender,
            stop_orender,
            get_orender_service_status,
            install_orender_service,
            uninstall_orender_service,
            start_orender_service,
            stop_orender_service,
            restart_orender_service,
            restart_pipewire_services,
            control_osc_metering,
            select_layout,
            import_layout_from_path,
            pick_import_layout_path,
            pick_preset_layout_path,
            pick_export_layout_path,
            pick_import_evaluation_artifact_path,
            pick_export_evaluation_artifact_path,
            pick_bridge_path,
            pick_orender_path,
            pick_backend_file_path,
            export_layout_to_path,
            control_speaker_gain,
            control_object_mute,
            control_speaker_mute,
            control_master_gain,
            control_loudness,
            control_auto_gain,
            control_auto_gain_ceiling,
            control_adaptive_resampling,
            control_adaptive_resampling_enable_far_mode,
            control_adaptive_resampling_force_silence_in_far_mode,
            control_adaptive_resampling_hard_recover_high_in_far_mode,
            control_adaptive_resampling_hard_recover_low_in_far_mode,
            control_adaptive_resampling_far_mode_return_fade_in_ms,
            control_latency_target,
            control_adaptive_resampling_kp_near,
            control_adaptive_resampling_ki,
            control_adaptive_resampling_integral_discharge_ratio,
            control_adaptive_resampling_max_adjust,
            control_adaptive_resampling_update_interval_callbacks,
            control_adaptive_resampling_high_recover_entry_margin_ms,
            control_adaptive_resampling_pause,
            control_adaptive_resampling_reset_ratio,
            control_metering_rate_hz,
            control_diag_rate_hz,
            control_diag_publication_enabled,
            control_spread_min,
            control_spread_max,
            control_spread_from_distance,
            control_spread_distance_range,
            control_spread_distance_curve,
            control_size_to_spread_mode,
            control_distance_model,
            control_distance_model_metric,
            control_distance_diffuse_metric,
            control_hybrid_external_backend,
            control_hybrid_internal_backend,
            control_hybrid_curve,
            control_hybrid_metric,
            control_hybrid_curve_smoothing,
            control_render_evaluation_object_size_intervals,
            control_render_evaluation_cartesian_x_size,
            control_render_evaluation_cartesian_y_size,
            control_render_evaluation_cartesian_z_size,
            control_render_evaluation_cartesian_z_neg_size,
            control_render_backend,
            control_backend_param,
            backend_file_get,
            backend_file_list,
            backend_file_put,
            control_restore_render_backend,
            control_render_evaluation_mode,
            control_render_evaluation_polar_azimuth_resolution,
            control_render_evaluation_polar_elevation_resolution,
            control_render_evaluation_polar_distance_res,
            control_render_evaluation_polar_distance_max,
            control_render_evaluation_position_interpolation,
            subscribe_speaker_gaintable,
            unsubscribe_speaker_gaintable,
            control_distance_diffuse_enabled,
            control_distance_diffuse_threshold,
            control_distance_diffuse_curve,
            control_room_ratio,
            control_room_ratio_rear,
            control_room_ratio_lower,
            control_room_ratio_center_blend,
            control_layout_radius_m,
            control_layout_config,
            control_layout_config_apply,
            control_speakers_config,
            control_speaker_az,
            control_speaker_el,
            control_speaker_distance,
            control_speaker_x,
            control_speaker_y,
            control_speaker_z,
            control_speaker_coord_mode,
            control_speaker_delay,
            control_speaker_spatialize,
            control_speaker_name,
            control_speaker_freq_low,
            control_speaker_freq_high,
            control_speakers_apply,
            control_speakers_add,
            control_speakers_remove,
            control_speakers_move,
            control_save_config,
            control_reload_config,
            control_log_level,
            control_ramp_mode,
            control_option,
            control_object_generator_param,
            control_phantom_extract_param,
            control_virtual_bed,
            control_output_mode,
            control_hrir_source,
            control_binaural_unit_scale,
            control_binaural_head_radius,
            control_binaural_reflections_enabled,
            control_binaural_reflections_level,
            control_binaural_reflections_room,
            control_binaural_reverb_enabled,
            control_binaural_reverb_level,
            control_binaural_reverb_rt60,
            control_binaural_air_absorption,
            sofa_browse,
            sofa_download,
            sofa_list_local,
            sofa_file_meta,
            sofa_delete_local,
            sofa_import_local,
            sofa_upload_to_renderer,
            sofa_download_cancel,
            control_head_recenter,
            control_head_tracking_address,
            control_head_tracking_format,
            control_head_tracking_smoothing,
            control_head_tracking_invert,
            control_audio_config,
            control_audio_config_apply,
            control_audio_output_device,
            control_audio_output_backend,
            control_audio_output_file,
            control_audio_output_file_format,
            refresh_output_devices,
            control_input_config,
            control_input_config_apply,
            control_input_mode,
            control_input_live_backend,
            control_input_live_node,
            control_input_live_description,
            control_input_live_layout,
            import_input_layout_from_path,
            control_input_live_channels,
            control_input_live_sample_rate,
            control_input_live_format,
            control_input_live_clock_mode,
            control_input_live_map,
            control_input_live_lfe_mode,
            control_input_apply,
            control_render_bridge_path,
            control_render_input_pipe,
            control_export_layout,
            control_audio_sample_rate,
            control_drc_mode,
            control_drc_weight,
            auto_tune_snapshot_save,
            auto_tune_snapshot_take,
            auto_tune_snapshot_peek,
            mpv_overlay_set_trail_prefs,
            mpv_overlay_set_active,
            mpv_overlay_set_labels,
            mpv_overlay_set_objects,
            mpv_overlay_set_heatmap_enabled,
            mpv_overlay_set_heatmap_custom_stops,
            mpv_overlay_set_heatmap_bands,
            mpv_overlay_set_heatmap_colormap,
        ])
        .build(tauri::generate_context!())
        .expect("error building Tauri application")
        .run(|app_handle, event| {
            // On exit, take the Studio-launched standby renderer down with us
            // (unless the user opted to keep it): ask for a graceful quit so
            // it writes its live-state handoff sidecar, then kill as a last
            // resort. mpv-embedded renderers are unaffected (separate process).
            if let tauri::RunEvent::ExitRequested { .. } = event {
                let state = app_handle.state::<SharedState>();
                let keep_alive = load_config(&state.config_dir).keep_renderer_alive_on_quit;
                let mut child_guard = state.renderer_child.lock().unwrap();
                if let Some(child) = child_guard.as_mut() {
                    if keep_alive {
                        log::info!("leaving the local renderer running (keep alive on quit)");
                    } else {
                        send_control(
                            &state.osc_tx,
                            OscControlMsg::SendNoArgs {
                                address: "/omniphony/control/quit".to_string(),
                            },
                        );
                        let deadline =
                            std::time::Instant::now() + std::time::Duration::from_secs(2);
                        loop {
                            match child.try_wait() {
                                Ok(Some(_)) => break,
                                Ok(None) if std::time::Instant::now() < deadline => {
                                    std::thread::sleep(std::time::Duration::from_millis(50));
                                }
                                _ => {
                                    log::warn!("local renderer did not quit in time; killing it");
                                    let _ = child.kill();
                                    let _ = child.wait();
                                    break;
                                }
                            }
                        }
                        *child_guard = None;
                    }
                }
            }
        });
}
