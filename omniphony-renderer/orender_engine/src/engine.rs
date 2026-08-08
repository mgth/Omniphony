//! The headless decode→render session.
//!
//! [`Engine`] owns a loaded decoder bridge plugin and a [`SpatialRenderer`], and
//! turns raw compressed packets into VBAP-rendered interleaved multichannel PCM.
//! It performs no audio I/O: the host (the `orender` CLI, or `liborender.so`
//! inside mpv) feeds packets in and consumes rendered samples.

use crate::bridge_loader::{LoadedBridge, resolve_bridge};
use crate::events::Configuration;
use crate::osc::{ObjectMeta, OscSender};
use crate::overlay;
use crate::renderer_build::{SpatialRendererParams, build_spatial_renderer};
use crate::{object_gen, phantom_extract, render, spatial, virtual_bed};
use anyhow::{Result, anyhow, bail};
use bridge_api::{RChannelLabel, RCoordinateFormat, RDecodedFrame, RInputTransport};
use renderer::config::Config;
use renderer::metering::AudioMeter;
use renderer::spatial_renderer::{SpatialChannelEvent, SpatialRenderer};
use renderer::speaker_layout::SpeakerLayout;
use std::collections::HashMap;
use std::path::Path;

/// Options for the engine's OSC live-control server.
pub struct OscOptions {
    /// Monitoring target host (where outgoing VU/state bundles are sent).
    pub host: String,
    /// Monitoring target port.
    pub port_out: u16,
    /// Registration/listener port for incoming control; 0 = OS-assigned (logged).
    pub port_in: u16,
}

/// One block of rendered, interleaved multichannel `f32` PCM.
pub struct RenderedAudio {
    /// Interleaved samples: `[s0c0, s0c1, …, s0c(N-1), s1c0, …]`, length
    /// `n_frames * n_channels`.
    pub samples: Vec<f32>,
    /// Number of output channels (speakers).
    pub n_channels: u32,
    /// Number of sample frames in this block.
    pub n_frames: usize,
    /// Absolute decoded sample position at the start of this block, in input
    /// samples (monotonic across the stream; reset by [`Engine::reset`]).
    pub sample_pos: u64,
}

/// A decode→render session: bridge plugin + spatial renderer + per-stream state.
pub struct Engine {
    bridge: LoadedBridge,
    renderer: SpatialRenderer,
    sample_rate: u32,
    coordinate_format: RCoordinateFormat,

    // ── per-stream spatial state ──
    /// Shared fixed-channel planner (routing + plan cache, both stream kinds).
    fixed_planner: virtual_bed::FixedChannelPlanner,
    /// Cached object↔channel declaration from the bridge (sparse emission),
    /// sorted by channel. See `docs/channel-object-contract.md`.
    object_channels: Vec<(u32, usize)>,
    has_objects: bool,
    loudness_applied: bool,
    decoded_samples: u64,
    /// Dynamic object count of the last rendered frame (`channel_count − beds`),
    /// `0` for plain multichannel content. Surfaced over FFI for the host's track
    /// info display. Reset per segment.
    last_object_count: u32,
    /// Dialogue normalisation level (dBFS, ≤ 0) once a major-sync frame has
    /// declared it. Surfaced over FFI for the host's track info display. Reset
    /// per segment.
    last_dialnorm: Option<i8>,
    /// Channel labels of the bed of the last object-based frame (empty for plain
    /// multichannel / no bed). Surfaced over FFI so the host can show the bed
    /// composition (e.g. "LFE+11 objects"). Reused buffer; reset per segment.
    last_bed_labels: Vec<RChannelLabel>,

    // ── DRC gain ramp state (continues across frames) ──
    drc_gain: f32,
    drc_target_gain: f32,
    drc_ramp_samples_remaining: u32,
    /// DRC mode last pushed to the bridge (selects which DRC words the decoder
    /// extracts → drives `frame.drc_gain`). Synced from the live param each
    /// `process` so config + OSC changes reach the decoder, mirroring the CLI's
    /// decoder thread. Empty until the first sync.
    applied_drc_mode: String,

    // ── reusable scratch ──
    frame_events: Vec<SpatialChannelEvent>,
    pcm_f32_buf: Vec<f32>,

    /// Optional OSC live-control server (kept alive here; its Drop stops the
    /// listener thread when the engine is dropped).
    osc: Option<OscSender>,
    /// VU meter, created with the OSC server; feeds outgoing meter bundles.
    audio_meter: Option<AudioMeter>,
    /// Accumulated object names (id → name) for OSC object broadcast.
    object_names: HashMap<u32, String>,
    /// Opt-in per-frame perf accumulator (env `ORENDER_PERF_LOG`). `None` = off
    /// (zero overhead). Diagnostic only; safe to remove once perf work lands.
    perf: Option<PerfLog>,

    /// Pluggable bed→height object generator stage (2D upmix). Off by default;
    /// active only for channel content on a height-capable layout. Selected by
    /// the live param `object_generator_id`. See [`crate::object_gen`].
    object_gen: object_gen::ObjectGenStage,

    /// Phantom-source extraction pre-stage: runs before the height lift, pulling
    /// correlated content out of channel pairs as planar objects and reducing the
    /// bed. Off by default (live param `phantom_extract_mode`). See
    /// [`crate::phantom_extract`].
    phantom: phantom_extract::PhantomExtractStage,

    /// Signature of the last published fixed-channel applicability state. The
    /// JSON snapshot is rebuilt only when this changes, never every frame.
    fixed_processing_sig: Option<FixedProcessingSig>,
}

#[derive(Clone, PartialEq, Eq)]
struct FixedProcessingSig {
    stream_has_objects: bool,
    labels: Vec<RChannelLabel>,
    options_epoch: u64,
    output_has_height: bool,
    phantom_count: usize,
    height_count: usize,
}

/// Throttled (~1 Hz) aggregate of per-frame render/decode cost, correlated with
/// the block size and channel/metadata mix that drive it. Confirms on real
/// content whether render spikes track active-object count vs metadata frames,
/// and reveals the actual `sample_count` per access unit (to calibrate the
/// offline benches). Enabled by setting `ORENDER_PERF_LOG=1`.
struct PerfLog {
    last_flush: std::time::Instant,
    frames: u64,
    obj_sum: u64,
    meta_frames: u64,
    sc_min: u32,
    sc_max: u32,
    render_sum_ms: f64,
    render_max_ms: f32,
    render_meta_max_ms: f32,
    decode_sum_ms: f64,
    decode_max_ms: f32,
}

impl PerfLog {
    fn new() -> Self {
        Self {
            last_flush: std::time::Instant::now(),
            frames: 0,
            obj_sum: 0,
            meta_frames: 0,
            sc_min: u32::MAX,
            sc_max: 0,
            render_sum_ms: 0.0,
            render_max_ms: 0.0,
            render_meta_max_ms: 0.0,
            decode_sum_ms: 0.0,
            decode_max_ms: 0.0,
        }
    }

    fn record(
        &mut self,
        sample_count: u32,
        n_objects: u32,
        has_metadata: bool,
        render_ms: f32,
        decode_ms: f32,
    ) {
        self.frames += 1;
        self.obj_sum += n_objects as u64;
        self.sc_min = self.sc_min.min(sample_count);
        self.sc_max = self.sc_max.max(sample_count);
        self.render_sum_ms += render_ms as f64;
        self.render_max_ms = self.render_max_ms.max(render_ms);
        self.decode_sum_ms += decode_ms as f64;
        self.decode_max_ms = self.decode_max_ms.max(decode_ms);
        if has_metadata {
            self.meta_frames += 1;
            self.render_meta_max_ms = self.render_meta_max_ms.max(render_ms);
        }

        // Flush on a ~1 Hz wall clock (realtime mpv) OR every 8192 render calls,
        // so faster-than-realtime offline runs still emit periodically. A final
        // flush on drop catches short streams that hit neither threshold.
        if self.last_flush.elapsed().as_secs_f32() >= 1.0 || self.frames >= 8192 {
            self.flush();
        }
    }

    fn flush(&mut self) {
        if self.frames == 0 {
            return;
        }
        // eprintln (not log::) so it is visible without a logger init — opt-in
        // via ORENDER_PERF_LOG, shows up in both `--nocapture` test runs and
        // mpv's stderr.
        eprintln!(
            "perf: {} render calls | block {}…{} smp | obj~{:.1} | meta {:.0}% \
             | render avg {:.3} max {:.3} (meta-max {:.3}) ms \
             | decode avg {:.3} max {:.3} ms",
            self.frames,
            self.sc_min,
            self.sc_max,
            self.obj_sum as f64 / self.frames as f64,
            self.meta_frames as f64 / self.frames as f64 * 100.0,
            self.render_sum_ms / self.frames as f64,
            self.render_max_ms,
            self.render_meta_max_ms,
            self.decode_sum_ms / self.frames as f64,
            self.decode_max_ms,
        );
        *self = PerfLog::new();
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // Tear down the OSC server (stop + join its listener/standby threads)
        // *before* the implicit field drops below free the renderer and bridge
        // those threads read from. `osc` is declared after `bridge`/`renderer`,
        // so plain field-order drop would stop the threads last, leaving a
        // teardown window where a worker could read state mid-free. Explicit so
        // it can't regress if the field order changes.
        drop(self.osc.take());
        if let Some(perf) = self.perf.as_mut() {
            perf.flush();
        }
    }
}

impl Engine {
    /// Build a session around an already-loaded bridge and a constructed
    /// renderer. The bridge must already be configured (presentation, DRC mode)
    /// before the first [`process`](Self::process) call.
    pub fn new(bridge: LoadedBridge, renderer: SpatialRenderer, sample_rate: u32) -> Self {
        let coordinate_format = bridge.bridge.coordinate_format();
        let engine = Self {
            bridge,
            renderer,
            sample_rate,
            coordinate_format,
            fixed_planner: virtual_bed::FixedChannelPlanner::new(),
            object_channels: Vec::new(),
            has_objects: false,
            loudness_applied: false,
            decoded_samples: 0,
            last_object_count: 0,
            last_dialnorm: None,
            last_bed_labels: Vec::new(),
            drc_gain: 1.0,
            drc_target_gain: 1.0,
            drc_ramp_samples_remaining: 0,
            applied_drc_mode: String::new(),
            frame_events: Vec::new(),
            pcm_f32_buf: Vec::new(),
            osc: None,
            audio_meter: None,
            object_names: HashMap::new(),
            perf: std::env::var_os("ORENDER_PERF_LOG")
                .is_some()
                .then(PerfLog::new),
            object_gen: object_gen::ObjectGenStage::new(),
            phantom: phantom_extract::PhantomExtractStage::new(),
            fixed_processing_sig: None,
        };
        engine
            .renderer
            .renderer_control()
            .set_fixed_channel_catalog(virtual_bed::fixed_channel_catalog_json());
        engine
    }

    /// Register a host-supplied (out-of-tree) bed→height object generator so it
    /// can be selected by id and appears in Studio's selector + parameter sliders.
    /// Call at startup (before `enable_osc`); a later call re-publishes the schema.
    pub fn register_object_generator(
        &mut self,
        factory: Box<dyn object_gen::ObjectGeneratorFactory>,
    ) {
        self.object_gen.register(factory);
        self.renderer
            .renderer_control()
            .set_object_generators_schema(self.object_gen.registry().listings_json());
    }

    /// Record the hosting FFI shim's C-ABI version so the live-state snapshot
    /// broadcasts it (`/omniphony/state/render/abi`, shown in Studio's About).
    /// Called by liborender right after creation; hosts linking the engine as a
    /// Rust crate never call it.
    pub fn set_host_abi(&self, major: u32, minor: u32) {
        self.renderer.renderer_control().set_host_abi(major, minor);
    }

    /// Start the OSC live-control server, attaching the renderer control so
    /// incoming `/omniphony/control/*` messages adjust live params (gains, room,
    /// spread, …) — picked up by the next `render_frame` — and registered
    /// clients receive the live-state bundle.
    ///
    /// The embedded host has no audio/input controls, so those OSC domains stay
    /// inactive (studio hides the matching panels via the capabilities
    /// handshake). The server is owned by the engine and shut down on drop.
    pub fn enable_osc(&mut self, opts: OscOptions) -> Result<()> {
        use std::net::SocketAddrV4;
        use std::str::FromStr;

        // Publish the object-generator schema (built-ins + any host-registered
        // out-of-tree generators) into RendererControl so the live-state bundle
        // carries it to Studio.
        self.renderer
            .renderer_control()
            .set_object_generators_schema(self.object_gen.registry().listings_json());
        // Publish the phantom-extraction param schema for Studio's sliders.
        self.renderer
            .renderer_control()
            .set_phantom_schema(phantom_extract::phantom_schema_json());

        let target = SocketAddrV4::from_str(&format!("{}:{}", opts.host, opts.port_out))
            .map_err(|e| anyhow!("invalid OSC target {}:{}: {e}", opts.host, opts.port_out))?;
        let mut sender = OscSender::new(target)?;
        sender.attach_renderer_control(self.renderer.renderer_control());
        sender.start_listener(opts.port_in, true)?;
        // Meter cadence reads the RendererControl atomic each poll (source of
        // truth, OSC-adjustable, persisted).
        self.audio_meter = Some(AudioMeter::new_with_rate_atomic(
            self.renderer.num_speakers(),
            self.renderer.renderer_control().meter_rate_atomic(),
        ));
        self.osc = Some(sender);
        Ok(())
    }

    /// Build a session from file paths: load the omniphony YAML config (if any),
    /// resolve the speaker layout (explicit path → config layout → 7.1.4 preset),
    /// load + configure the decoder bridge, and build the renderer. This is the
    /// path both the FFI and the test harness use.
    /// `bridge_path`: explicit decoder-bridge path, or `None` to take it from
    /// the config YAML's `render.bridge_path`. As a last-resort fallback, when
    /// neither is set (or the config path no longer exists), we look for a
    /// `*_bridge.{so,dll,dylib}` next to the current executable — covers the
    /// Windows bundle case where the user extracted a zip with mpv.exe,
    /// orender.dll and the bridge .dll all in the same folder.
    /// `input_codec`: codec identifier of the raw access units the host will
    /// feed (matching the bridge's supported codec IDs). Declared to the
    /// bridge so its `Raw` transport routes to the right decoder; `None`
    /// lets the bridge sniff the sync word.
    pub fn from_paths(
        config_yaml_path: Option<&Path>,
        speaker_layout_path: Option<&Path>,
        bridge_path: Option<&Path>,
        input_codec: Option<&str>,
        sample_rate: u32,
    ) -> Result<Self> {
        let t_total = std::time::Instant::now();
        let (loaded_cfg, live_restored) = match config_yaml_path {
            Some(path) => {
                let (cfg, restored) = Config::load_or_default_with_live(path);
                (Some(cfg), restored)
            }
            None => (None, false),
        };
        let render_cfg = loaded_cfg.and_then(|c| c.render);

        let layout = if let Some(p) = speaker_layout_path {
            SpeakerLayout::from_file(p)?
        } else if let Some(l) = render_cfg.as_ref().and_then(|c| c.current_layout.clone()) {
            l
        } else {
            SpeakerLayout::preset("7.1.4")?
        };

        // Resolve the bridge path with the shared strict policy (identical for
        // the CLI and this FFI/mpv host): an explicitly requested path (FFI
        // param or config render.bridge_path) must exist or it's an error; only
        // when nothing is requested do we auto-discover *_bridge.* next to the
        // host binary. See `bridge_loader::resolve_bridge`.
        let config_bridge = render_cfg.as_ref().and_then(|c| c.bridge_path.clone());
        let resolved_bridge = resolve_bridge(bridge_path, config_bridge.as_deref())?;

        // The renderer's table mode/defaults come from the bridge, so load and
        // configure it before building the renderer.
        let t_bridge = std::time::Instant::now();
        let mut bridge = LoadedBridge::load_with_params(&resolved_bridge)?;
        bridge.configure("presentation", "best");
        if let Some(codec) = input_codec {
            // Disambiguates the bridge's `Raw` transport (no data_type byte).
            // Unknown to older bridges → harmless `false`, which falls back to
            // sniffing the sync word.
            bridge.configure("input_codec", codec);
        }
        let vbap_defaults = bridge.vbap_cartesian_defaults();
        let preferred = bridge.preferred_vbap_table_mode();
        log::info!(
            "bridge loaded + configured in {:.2}s",
            t_bridge.elapsed().as_secs_f64()
        );

        let params = SpatialRendererParams::from_render_config(render_cfg.as_ref());
        let renderer = build_spatial_renderer(
            &params,
            layout,
            sample_rate,
            vbap_defaults,
            preferred,
            render_cfg.as_ref(),
        )?;

        // Seed monitoring cadences from config (renderer is the source of
        // truth); embedded default is 10 Hz.
        let control = renderer.renderer_control();
        // Propagate config_path + bridge_path so that the OSC SaveConfig
        // command can persist the live state (CLI bootstrap does this in
        // cli/decode/bootstrap.rs; any embedder of this engine — FFI,
        // mpv-omniphony, future hosts — needs it too). Without these,
        // `persist::save_live_config` either aborts with "no config path
        // available", or — worse — succeeds while erasing
        // `render.bridge_path` from the YAML because `control.bridge_path()`
        // returns None and gets serialised verbatim.
        if let Some(path) = config_yaml_path {
            control.set_config_path(path.to_path_buf());
            // Diagnose whether that path actually loaded or silently fell back
            // to defaults — `render_cfg` above can't tell us, since
            // `load_or_default` collapses missing/parse-error into defaults.
            // Surfaced in Studio's About to catch host config mismatches.
            let status = renderer::config::Config::load_status(path);
            if status != renderer::config::ConfigLoadStatus::Loaded {
                log::warn!(
                    "config '{}' not loaded ({}); running on built-in defaults",
                    path.display(),
                    status.as_str()
                );
            }
            control.set_config_status(Some(status.as_str().to_string()));
        }
        // State restored from a live-handoff sidecar is by definition unsaved.
        if live_restored {
            control.mark_dirty();
        }

        // Overlay display prefs (enable / labels / trails) are owned and
        // persisted by orender now, in a small dedicated file next to the
        // config — loaded here at startup and auto-saved on each live change.
        // Deliberately NOT part of the savable config (no mark_dirty / save).
        let overlay_prefs = config_yaml_path
            .map(Path::to_path_buf)
            .or_else(crate::default_config_path)
            .and_then(|p| p.parent().map(|d| d.join("overlay-prefs.conf")));
        if let Some(p) = overlay_prefs {
            overlay::load_prefs(&p);
        }

        control.set_bridge_path(Some(resolved_bridge.clone()));
        control.set_meter_rate_hz(
            render_cfg
                .as_ref()
                .and_then(|c| c.meter_rate)
                .unwrap_or(10.0),
        );
        control.set_diag_rate_hz(
            render_cfg
                .as_ref()
                .and_then(|c| c.diag_rate)
                .unwrap_or(10.0),
        );

        // Object-transition ramp mode. `SpatialRenderer::new` seeds the live
        // params with `RampMode::Sample` (per-sample `compute_gains` — the most
        // expensive path); the CLI overrides this from `render.ramp_mode` in
        // config_resolution + bootstrap, but this embedded/mpv host never did,
        // so it silently ran every render in per-sample mode. Mirror the CLI:
        // honour `render.ramp_mode` (default "frame"). Both the requested-mode
        // mutex and the live snapshot field must be set — the render loop reads
        // the latter.
        let ramp_mode = render_cfg
            .as_ref()
            .and_then(renderer::config_fields::ramp_mode::get)
            .as_deref()
            .and_then(renderer::live_params::RampMode::from_str)
            .unwrap_or(renderer::live_params::RampMode::Frame);
        control.set_requested_ramp_mode(ramp_mode);
        control.live.write().ramp_mode = ramp_mode;

        // Declared live options (surround_placement, output_channel_mapping,
        // synthesized-object master/generators) plus
        // their param bags and the virtual bed: seeded from config through the
        // shared registry seed — the same call as the CLI bootstrap, so the
        // embedded host cannot drift from it (FFI/CLI parity by construction).
        if let Some(render) = render_cfg.as_ref() {
            renderer::options::seed_live_from_config(&mut control.live.write(), render);
        }

        // DRC: seed the live params from config and publish the bridge's
        // supported modes (so studio shows the DRC control). The decode-side
        // mode itself is pushed to the bridge lazily in `process` (see
        // `sync_drc_mode`), mirroring the CLI's decoder thread.
        let supported_drc: Vec<String> = bridge
            .bridge
            .supported_drc_modes()
            .iter()
            .map(|m| m.as_str().to_string())
            .collect();
        control.set_bridge_supported_drc_modes(supported_drc);
        {
            let mut live = control.live.write();
            live.drc_mode = render_cfg
                .as_ref()
                .and_then(|c| c.drc_mode.clone())
                .unwrap_or_else(|| "Off".to_string());
            live.drc_weight = render_cfg
                .as_ref()
                .and_then(|c| c.drc_weight)
                .unwrap_or(1.0)
                .clamp(0.0, 1.0);
        }

        let engine = Self::new(bridge, renderer, sample_rate);
        log::info!(
            "engine ready in {:.2}s (bridge load + VBAP table + renderer build)",
            t_total.elapsed().as_secs_f64()
        );
        Ok(engine)
    }

    /// Number of output channels the renderer produces (speaker count, or 2 in
    /// binaural mode). Hosts size their output sink from this.
    pub fn channel_count(&self) -> u32 {
        self.renderer.output_channel_count() as u32
    }

    /// Per-channel labels of the active output layout, one entry per speaker in
    /// render (output channel) order. The host (mpv) turns this into a channel
    /// map. Speakers whose layout name is unrecognised map to
    /// [`RChannelLabel::Unknown`].
    pub fn channel_layout(&self) -> Vec<RChannelLabel> {
        // Binaural output is a plain stereo pair; the layout MUST match
        // `channel_count()` (2) or the host builds an inconsistent chmap and the
        // frame is malformed (silence).
        if self.renderer.output_channel_count() == 2 {
            return vec![
                crate::channel_layout::label_for_speaker_name("FL"),
                crate::channel_layout::label_for_speaker_name("FR"),
            ];
        }
        self.renderer
            .speaker_layout()
            .speakers
            .iter()
            .map(|s| crate::channel_layout::label_for_speaker_name(&s.name))
            .collect()
    }

    /// Whether the current presentation carries dynamic objects. A live fact
    /// about the stream (it may flip mid-stream); hosts must not latch it.
    pub fn has_objects(&self) -> bool {
        self.bridge.bridge.has_objects()
    }

    /// Dynamic object count of the last rendered frame (decoded `channel_count`
    /// minus the bed channels), or `0` for plain multichannel content. For the
    /// host's track info display.
    pub fn object_count(&self) -> u32 {
        self.last_object_count
    }

    /// Dialogue normalisation level in dBFS (≤ 0) once the stream has declared
    /// it, else `None`. For the host's track info display.
    pub fn dialnorm_db(&self) -> Option<i8> {
        self.last_dialnorm
    }

    /// Channel labels of the bed of the last object-based frame (empty for plain
    /// multichannel / no bed). For the host's track info display.
    pub fn bed_labels(&self) -> &[RChannelLabel] {
        &self.last_bed_labels
    }

    /// Configured render mode for channel-based (non-object) content, as a small
    /// code for the C FFI: 0 = host (the host should fall back to its native
    /// decoder for plain multichannel streams), non-zero = spatial (render
    /// through the virtual bed). Per-channel direct/virtual placement lives in
    /// the virtual bed, not in this code.
    pub fn channel_render_mode_code(&self) -> i32 {
        use renderer::live_params::ChannelRenderMode;
        match self
            .renderer
            .renderer_control()
            .live
            .read()
            .channel_render_mode
        {
            ChannelRenderMode::Host => 0,
            ChannelRenderMode::Spatial => 1,
        }
    }

    /// Override the channel render mode at runtime (per-host override of the
    /// config value). Codes: 0 = host, anything else = spatial. The legacy
    /// `direct`(1)/`virtual`(2) codes both map to spatial.
    pub fn set_channel_render_mode_code(&self, code: i32) {
        use renderer::live_params::ChannelRenderMode;
        let mode = match code {
            0 => ChannelRenderMode::Host,
            _ => ChannelRenderMode::Spatial,
        };
        self.renderer
            .renderer_control()
            .live
            .write()
            .channel_render_mode = mode;
    }

    /// Output channel mapping as a small code for the C FFI: 0 = by_index
    /// (positionless, port N = layout speaker N), 1 = by_name (positional). The
    /// host (mpv) uses this to decide between a positionless and a positional
    /// `mp_chmap`.
    pub fn output_channel_mapping_code(&self) -> i32 {
        self.renderer
            .renderer_control()
            .live
            .read()
            .output_channel_mapping
            .code()
    }

    /// Override the output channel mapping at runtime. Codes: 0 = by_index,
    /// 1 = by_name. Unknown codes are ignored.
    pub fn set_output_channel_mapping_code(&self, code: i32) {
        if let Some(mapping) = renderer::live_params::OutputChannelMapping::from_code(code) {
            self.renderer
                .renderer_control()
                .live
                .write()
                .output_channel_mapping = mapping;
        }
    }

    /// Input sample rate the session was created for.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Reset the session after a seek or stream discontinuity. Flushes the
    /// bridge pipeline and the renderer's per-object/ramp state, and clears the
    /// per-stream spatial state. Live parameters (gains, layout, OSC-applied
    /// settings) are preserved — a seek must not lose live adjustments.
    pub fn reset(&mut self) {
        self.bridge.bridge.reset();
        self.renderer.reset_runtime_state();
        self.reset_segment_state();
        // Object frames are delta-encoded; after a seek the (static) virtual-bed
        // poses would never be re-sent, so force a full re-emit of object
        // positions + names on the next frame.
        if let Some(osc) = self.osc.as_mut() {
            osc.request_full_object_resend();
        }
        self.decoded_samples = 0;
        self.drc_gain = 1.0;
        self.drc_target_gain = 1.0;
        self.drc_ramp_samples_remaining = 0;
        // Drop overlay scene + motion trails so they don't bridge the seek.
        overlay::clear();
    }

    fn reset_segment_state(&mut self) {
        self.has_objects = false;
        self.fixed_planner.reset();
        self.object_channels.clear();
        self.frame_events.clear();
        self.loudness_applied = false;
        self.object_names.clear();
        // A new segment may re-declare a different object count / dialnorm / bed.
        self.last_object_count = 0;
        self.last_dialnorm = None;
        self.last_bed_labels.clear();
        self.fixed_processing_sig = None;
        self.renderer
            .renderer_control()
            .set_fixed_channel_processing(
                r#"{"stream":"idle","labels":[],"phantom":"no_stream","height":"no_stream"}"#
                    .to_string(),
            );
    }

    #[allow(clippy::too_many_arguments)]
    fn publish_fixed_processing_state(
        &mut self,
        stream_has_objects: bool,
        labels: &[RChannelLabel],
        options_epoch: u64,
        output_has_height: bool,
        phantom_count: usize,
        height_count: usize,
        synthetic_objects_enabled: bool,
        phantom_mode: renderer::live_params::PhantomExtractMode,
        height_generator_enabled: bool,
    ) {
        let unchanged = self.fixed_processing_sig.as_ref().is_some_and(|sig| {
            sig.stream_has_objects == stream_has_objects
                && sig.labels.as_slice() == labels
                && sig.options_epoch == options_epoch
                && sig.output_has_height == output_has_height
                && sig.phantom_count == phantom_count
                && sig.height_count == height_count
        });
        if unchanged {
            return;
        }

        self.fixed_processing_sig = Some(FixedProcessingSig {
            stream_has_objects,
            labels: labels.to_vec(),
            options_epoch,
            output_has_height,
            phantom_count,
            height_count,
        });

        let phantom = if phantom_mode == renderer::live_params::PhantomExtractMode::Off {
            "off"
        } else if !synthetic_objects_enabled {
            "master_off"
        } else if stream_has_objects {
            "object_stream"
        } else if phantom_count > 0 {
            "active"
        } else {
            "insufficient_channels"
        };
        let input_has_height = object_gen::input_has_height(labels);
        let height = if !height_generator_enabled {
            "off"
        } else if !synthetic_objects_enabled {
            "master_off"
        } else if stream_has_objects {
            "object_stream"
        } else if input_has_height {
            "input_has_height"
        } else if !output_has_height {
            "output_has_no_height"
        } else if height_count > 0 {
            "active"
        } else {
            "insufficient_channels"
        };
        let names: Vec<&str> = labels
            .iter()
            .map(|&label| bridge_api::labels::canonical_name(label))
            .collect();
        let state = serde_json::json!({
            "stream": if stream_has_objects { "objects" } else { "fixed" },
            "labels": names,
            "inputHasHeight": input_has_height,
            "outputHasHeight": output_has_height,
            "phantom": phantom,
            "height": height,
        });
        self.renderer
            .renderer_control()
            .set_fixed_channel_processing(state.to_string());
    }

    /// Push the live DRC mode to the bridge when it changes (selects which DRC
    /// words the decoder extracts). Cheap no-op when unchanged. Mirrors the
    /// CLI's `DecoderCommand::SetDrcMode` handling. The bridge preserves the
    /// mode across `reset`, so a seek keeps the current DRC setting.
    fn sync_drc_mode(&mut self) {
        let live_mode = {
            let control = self.renderer.renderer_control();
            let live = control.live.read();
            if live.drc_mode == self.applied_drc_mode {
                return;
            }
            live.drc_mode.clone()
        };
        self.bridge.bridge.set_drc_mode(live_mode.as_str().into());
        self.applied_drc_mode = live_mode;
    }

    /// Push one raw compressed packet and render any frames it produces.
    ///
    /// `transport`/`data_type` follow the bridge ABI: hosts that demux raw
    /// access units (e.g. mpv) pass [`RInputTransport::Raw`] with `data_type` 0.
    pub fn process(
        &mut self,
        data: &[u8],
        transport: RInputTransport,
        data_type: u8,
    ) -> Result<Vec<RenderedAudio>> {
        // Push any DRC-mode change (config-seeded or OSC-driven) to the decoder
        // before it decodes this packet.
        self.sync_drc_mode();

        let decode_started = std::time::Instant::now();
        let result = self
            .bridge
            .bridge
            .push_packet(data.into(), transport, data_type);
        let decode_time_ms = decode_started.elapsed().as_secs_f32() * 1000.0;

        if !result.error_message.is_empty() {
            bail!("bridge decode error: {}", result.error_message);
        }
        if result.did_reset {
            // Sync-loss recovery / seek inside the bridge: drop stale spatial
            // state but keep live params and the absolute sample clock. Also bump
            // the content generation, force a full object re-emit and clear the
            // overlay so OSC clients and the overlay purge the pre-seek objects
            // instead of leaving them behind as stale duplicates.
            self.renderer.reset_runtime_state();
            self.reset_segment_state();
            if let Some(osc) = self.osc.as_mut() {
                osc.bump_content_generation();
                osc.request_full_object_resend();
            }
            overlay::clear();
        }

        // The bridge decodes one packet into N frames synchronously; attribute the
        // packet's decode cost evenly across its frames so the per-frame meter
        // bundle reports a comparable figure to the CLI decoder thread.
        let per_frame_decode_time_ms = if result.frames.is_empty() {
            decode_time_ms
        } else {
            decode_time_ms / result.frames.len() as f32
        };

        let mut out = Vec::with_capacity(result.frames.len());
        for frame in result.frames.iter() {
            if let Some(chunk) = self.render_frame(frame, per_frame_decode_time_ms)? {
                out.push(chunk);
            }
        }
        Ok(out)
    }

    /// Convenience wrapper for hosts that always feed raw access units.
    pub fn process_raw(&mut self, data: &[u8]) -> Result<Vec<RenderedAudio>> {
        self.process(data, RInputTransport::Raw, 0)
    }

    fn render_frame(
        &mut self,
        frame: &RDecodedFrame,
        decode_time_ms: f32,
    ) -> Result<Option<RenderedAudio>> {
        let channel_count = frame.channel_count as usize;
        let sample_count = frame.sample_count as usize;
        let sample_rate = frame.sampling_frequency.max(1);
        let sample_pos_at_start = self.decoded_samples;

        let want_osc = self.osc.as_ref().is_some_and(|o| o.has_osc_clients());
        // The mpv overlay is produced in-process by the `overlay` module and
        // pulled over FFI; it needs the same object positions + meter levels as
        // OSC, but independently of whether any OSC client is connected. It
        // self-gates (only active once the host has pulled), so the CLI host
        // pays nothing here.
        let overlay_active = overlay::is_active();
        let want_objects = want_osc || overlay_active;

        // A mid-stream format change (the initial TrueHD layout settling, or a
        // 7.1<->5.1 boundary) invalidates the per-segment spatial state. Mirror
        // the CLI's `is_new_segment` handling: drop has_objects / bed / object
        // state so a channel-based segment is never stuck on a previous
        // object-based (or differently-sized) layout — the cause of a 5.1 track
        // not spatializing until a track swap. Bump the content generation and
        // force a full re-emit so OSC clients and the overlay purge the previous
        // layout's objects (otherwise a smaller new layout leaves stale,
        // inactive objects behind after the boundary).
        if frame.is_new_segment {
            self.renderer.reset_runtime_state();
            self.reset_segment_state();
            if let Some(osc) = self.osc.as_mut() {
                osc.bump_content_generation();
                osc.request_full_object_resend();
            }
            overlay::clear();
        }

        // Dialogue normalisation (from major-sync frames), applied once.
        if !self.loudness_applied {
            if let Some(dialogue_level) = frame.dialogue_level.into_option() {
                self.renderer.set_loudness(dialogue_level);
                self.last_dialnorm = Some(dialogue_level);
                self.loudness_applied = true;
                if want_osc {
                    if let Some(osc) = self.osc.as_ref() {
                        osc.send_loudness_state();
                    }
                }
            }
        }

        // Spatial metadata → bed config + per-channel events (+ OSC objects).
        for meta in frame.metadata.iter() {
            self.has_objects = true;
            // Cache the sparse object↔channel declaration.
            if !meta.object_channels.is_empty() {
                let mut decl: Vec<(u32, usize)> = meta
                    .object_channels
                    .iter()
                    .map(|oc| (oc.id, oc.channel as usize))
                    .collect();
                decl.sort_unstable_by_key(|&(_, channel)| channel);
                if self.object_channels != decl {
                    self.object_channels = decl;
                }
            }
            // Plan the fixed prefix through the shared channel planner:
            // virtualized by default, per-entry direct opt-in via the
            // placement layout — identically to fixed-only streams. Mode is
            // always Spatial here (an object stream cannot pass through the
            // host), and the plan is cached on (labels, options epoch).
            self.fixed_planner.plan_object_stream_fixed(
                &frame.channel_labels,
                &self.renderer,
                &mut self.frame_events,
            );
            let conf = Configuration::from(meta);
            spatial::build_spatial_channel_events(
                &conf,
                self.coordinate_format,
                &self.object_channels,
                &meta.channel_gains,
                meta.sample_pos,
                meta.ramp_duration,
                &mut self.frame_events,
            );

            // Outgoing: broadcast object positions/names to OSC clients and/or
            // feed the in-process mpv overlay.
            // Declarations are cached even before an OSC/overlay consumer
            // connects, so a later Studio attachment sees stable names.
            for upd in meta.name_updates.iter() {
                if self.object_names.get(&upd.id).map(String::as_str) != Some(upd.name.as_str()) {
                    self.object_names.insert(upd.id, upd.name.to_string());
                }
            }
            if want_objects {
                let mut objects = virtual_bed::build_fixed_channel_objects(
                    &self.renderer,
                    self.fixed_planner.fixed_labels(),
                )
                .unwrap_or_default();
                objects.extend(spatial::build_object_metas(
                    &conf,
                    self.coordinate_format,
                    &self.object_names,
                ));
                if want_osc {
                    let coord_fmt = match self.coordinate_format {
                        RCoordinateFormat::Cartesian => 0,
                        RCoordinateFormat::Polar => 1,
                    };
                    if let Some(osc) = self.osc.as_mut() {
                        let _ = osc.send_object_frame(
                            meta.sample_pos,
                            meta.ramp_duration,
                            coord_fmt,
                            &objects,
                        );
                        let seconds = meta.sample_pos as f64 / sample_rate as f64;
                        let _ = osc.send_timestamp(meta.sample_pos, seconds);
                    }
                }
                if overlay_active {
                    overlay::update_positions(overlay_positions(&objects));
                }
            }
        }

        if self.has_objects {
            let (master, phantom_mode, height_generator_enabled, options_epoch, output_has_height) = {
                let control = self.renderer.renderer_control();
                let live = control.live.read();
                (
                    live.synthetic_objects_enabled,
                    live.phantom_extract_mode,
                    !live.object_generator_id.trim().is_empty()
                        && !live.object_generator_id.eq_ignore_ascii_case("none"),
                    control.options_epoch(),
                    object_gen::layout_has_height(&control.active_topology().speaker_layout),
                )
            };
            let fixed_end = frame
                .channel_labels
                .iter()
                .position(|label| *label == RChannelLabel::Object)
                .unwrap_or(frame.channel_labels.len());
            self.publish_fixed_processing_state(
                true,
                &frame.channel_labels[..fixed_end],
                options_epoch,
                output_has_height,
                0,
                0,
                master,
                phantom_mode,
                height_generator_enabled,
            );
        }

        self.decoded_samples += sample_count as u64;

        // Number of synthesized height objects planned this frame by the
        // object-generator stage (stays 0 unless activated in the bed-only
        // branch below).
        let mut synth_count = 0usize;
        // Phantom-extraction pre-stage object count (planar primaries pulled out of
        // the bed). Stays 0 unless enabled in the bed-only branch below.
        let mut phantom_count = 0usize;

        // Bed-only / pre-metadata frames carry no OAMD objects: render them
        // according to the configured channel mode (host / direct / virtual),
        // identically to the CLI's file-decode path. The embedded host has no
        // live input device, so there is no input layout to bias the virtual
        // poses (`None`), exactly as in the CLI's file-decode path.
        if !self.has_objects {
            let labels: Vec<RChannelLabel> = frame.channel_labels.iter().copied().collect();
            let (
                mode,
                virtual_bed_layout,
                surround_placement,
                room_ratio,
                room_ratio_rear,
                room_ratio_lower,
                room_ratio_center_blend,
                object_generator_id,
                synthetic_objects_enabled,
                phantom_extract_mode,
                options_epoch,
            ) = {
                let control = self.renderer.renderer_control();
                let live = control.live.read();
                (
                    live.channel_render_mode,
                    live.virtual_bed.clone(),
                    live.surround_placement,
                    live.room_ratio,
                    live.room_ratio_rear,
                    live.room_ratio_lower,
                    live.room_ratio_center_blend,
                    live.object_generator_id.clone(),
                    live.synthetic_objects_enabled,
                    live.phantom_extract_mode,
                    control.options_epoch(),
                )
            };
            let output_layout = self.renderer.speaker_layout();

            match virtual_bed::plan_channel_render(
                mode,
                &labels,
                virtual_bed_layout.as_ref(),
                Some(&output_layout),
                room_ratio,
                room_ratio_rear,
                room_ratio_lower,
                room_ratio_center_blend,
                surround_placement,
            ) {
                virtual_bed::ChannelRenderPlan::Events { events, routes } => {
                    // Spatial mode mixes per channel: direct channels route
                    // one-hot by label, virtual channels render as VBAP
                    // objects. Reconfigure only on change to avoid per-frame
                    // churn.
                    self.fixed_planner.apply_routes(&self.renderer, routes);
                    self.frame_events = events;
                }
                // Host: the mpv decoder declines at the spatial probe and falls
                // back to ad_lavc, so this only runs for the discarded probe
                // frame. Silence: no mapping for these labels. Either way emit
                // silence so the host still advances by the frame's sample count.
                virtual_bed::ChannelRenderPlan::HostPassthrough
                | virtual_bed::ChannelRenderPlan::Silence => {
                    self.frame_events.clear();
                    let n_channels = self.renderer.output_channel_count() as u32;
                    return Ok(Some(RenderedAudio {
                        samples: vec![0.0; sample_count * n_channels as usize],
                        n_channels,
                        n_frames: sample_count,
                        sample_pos: sample_pos_at_start,
                    }));
                }
            }

            // Bed→height object generator (2D upmix): plan synthesized height
            // objects for channel content on a height-capable layout. No-op
            // unless a generator is selected and the gating passes. The static
            // positions/names are planned here (before the OSC object emit); the
            // audio for the new object channels is filled after the bed PCM is
            // built, below — they then ride the existing object/VBAP path.
            let ctx = object_gen::PrepareCtx {
                input_labels: &labels,
                output_layout: &output_layout,
                sample_rate,
                surround_placement,
            };
            // Phantom-extraction pre-stage runs first: its planar objects occupy the
            // channel slots right after the bed; the height-lift objects follow. The
            // audio (and the bed reduction) is applied after the bed PCM is built.
            self.phantom.set_mode(phantom_extract_mode);
            let phantom_enabled = synthetic_objects_enabled
                && phantom_extract_mode != renderer::live_params::PhantomExtractMode::Off;
            phantom_count = self.phantom.sync(phantom_enabled, &ctx, options_epoch);
            let effective_generator_id = if synthetic_objects_enabled {
                object_generator_id.as_str()
            } else {
                "none"
            };
            synth_count = self
                .object_gen
                .sync(effective_generator_id, &ctx, options_epoch);
            self.publish_fixed_processing_state(
                false,
                &labels,
                options_epoch,
                object_gen::layout_has_height(&output_layout),
                phantom_count,
                synth_count,
                synthetic_objects_enabled,
                phantom_extract_mode,
                !object_generator_id.trim().is_empty()
                    && !object_generator_id.eq_ignore_ascii_case("none"),
            );
            if phantom_count > 0 || synth_count > 0 {
                // Push each stage's live param overrides (declared schema; sparse —
                // absent keys keep the stage's default). Cheap + idempotent, so a
                // freshly (re)built stage re-receives them next frame.
                let control = self.renderer.renderer_control();
                let live = control.live.read();
                for (key, &value) in live.phantom_params.iter() {
                    self.phantom.set_param(key, value, sample_rate);
                }
                for (key, &value) in live.object_generator_params.iter() {
                    self.object_gen.set_param(key, value, sample_rate);
                }
            }
            // Phantom objects first (channels [channel_count ..]), then the height
            // objects offset past them.
            for (p, spec) in self.phantom.specs().iter().enumerate() {
                self.frame_events.push(SpatialChannelEvent {
                    channel_idx: channel_count + p,
                    is_bed: false,
                    gain_db: Some(spec.gain_db),
                    ramp_length: Some(0),
                    size: Some(spec.size),
                    position: Some(spec.position),
                    sample_pos: Some(0),
                });
            }
            for (k, spec) in self.object_gen.specs().iter().enumerate() {
                self.frame_events.push(SpatialChannelEvent {
                    channel_idx: channel_count + phantom_count + k,
                    is_bed: false,
                    gain_db: Some(spec.gain_db),
                    ramp_length: Some(0),
                    size: Some(spec.size),
                    position: Some(spec.position),
                    sample_pos: Some(0),
                });
            }

            // Outgoing: broadcast the virtual-bed channel poses + any synthesized
            // height objects as OSC objects and/or feed the in-process mpv
            // overlay, so they appear in Studio's 3D view and object list.
            if want_objects {
                let mut objects = virtual_bed::build_virtual_bed_objects(
                    &labels,
                    virtual_bed_layout.as_ref(),
                    Some(&output_layout),
                    room_ratio,
                    room_ratio_rear,
                    room_ratio_lower,
                    room_ratio_center_blend,
                    surround_placement,
                )
                .unwrap_or_default();
                for spec in self
                    .phantom
                    .specs()
                    .iter()
                    .chain(self.object_gen.specs().iter())
                {
                    objects.push(ObjectMeta {
                        name: spec.name.clone(),
                        x: spec.position[0] as f32,
                        y: spec.position[1] as f32,
                        z: spec.position[2] as f32,
                        coord_mode: "cartesian".to_string(),
                        direct_speaker_index: None,
                        gain: spec.gain_db as i32,
                        priority: 0.0,
                        size: spec.size,
                        fixed: false,
                        label: String::new(),
                    });
                }
                if !objects.is_empty() {
                    if want_osc {
                        if let Some(osc) = self.osc.as_mut() {
                            let _ = osc.send_object_frame(sample_pos_at_start, 0, 0, &objects);
                        }
                    }
                    if overlay_active {
                        overlay::update_positions(overlay_positions(&objects));
                    }
                }
            }
        }

        // DRC target from the stream gain, weighted by the live DRC weight.
        let drc_weight = self
            .renderer
            .renderer_control()
            .live
            .read()
            .drc_weight
            .clamp(0.0, 1.0);
        self.drc_target_gain = if drc_weight >= 1.0 {
            frame.drc_gain
        } else if drc_weight <= 0.0 {
            1.0
        } else {
            frame.drc_gain.powf(drc_weight)
        };
        self.drc_ramp_samples_remaining = frame.drc_ramp_duration;

        let mut pcm_f32 = std::mem::take(&mut self.pcm_f32_buf);
        render::fill_pcm_f32_drc(
            &mut pcm_f32,
            &frame.pcm,
            channel_count,
            &mut self.drc_gain,
            self.drc_target_gain,
            &mut self.drc_ramp_samples_remaining,
        );

        // Two upmix stages now that the bed PCM exists. First the phantom pre-stage
        // subtracts correlated content from the bed *in place* and appends its
        // planar objects; then the height lift runs on the reduced bed and appends
        // its objects. The renderer sees one extended interleaved buffer
        // (bed | phantom objects | height objects). Both zero-cost when inactive.
        let (mid_pcm, mid_channels): (&[f32], usize) = if phantom_count > 0 {
            self.phantom
                .process_and_extend(&mut pcm_f32, channel_count, sample_count, sample_rate)
        } else {
            (pcm_f32.as_slice(), channel_count)
        };
        let (render_pcm, render_channels): (&[f32], usize) = if synth_count > 0 {
            self.object_gen
                .fill_and_extend(mid_pcm, mid_channels, sample_count, sample_rate)
        } else {
            (mid_pcm, mid_channels)
        };

        // VU metering (outgoing): feed object PCM pre-render; speakers post-render.
        // Needed for OSC metering clients and/or the in-process overlay (object
        // circle radius tracks RMS).
        let want_meter_osc = self.osc.as_ref().is_some_and(|o| o.has_metering_clients());
        let want_metering = want_meter_osc || overlay_active;
        // The overlay needs object levels even with no OSC client connected, so
        // create the meter lazily if `enable_osc` never did (studio not running).
        if want_metering && self.audio_meter.is_none() {
            self.audio_meter = Some(AudioMeter::new_with_rate_atomic(
                self.renderer.num_speakers(),
                self.renderer.renderer_control().meter_rate_atomic(),
            ));
        }
        if want_metering {
            if let Some(meter) = self.audio_meter.as_mut() {
                meter.update_channel_count(render_channels);
                for chunk in render_pcm.chunks_exact(render_channels) {
                    meter.process_objects(chunk, render_channels);
                }
            }
        }

        let render_started = std::time::Instant::now();
        let rendered = self.renderer.render_frame(
            render_pcm,
            render_channels,
            &self.frame_events,
            Vec::new(),
            want_metering,
        )?;
        let render_time_ms = render_started.elapsed().as_secs_f32() * 1000.0;

        // Dynamic object count = decoded channels minus the fixed channels.
        // Only meaningful for object-based content; plain multichannel
        // reports 0.
        let num_beds = self.fixed_planner.fixed_labels().len();
        let n_objects = (channel_count as u32).saturating_sub(num_beds as u32);
        self.last_object_count = if self.has_objects { n_objects } else { 0 };

        // Bed composition for the host's track-info display, e.g. "LFE+11
        // objects". The renderer lays bed channels out first in PCM order (see
        // `build_spatial_channel_events`: beds at channels 0..num_beds, objects
        // after), so the bed labels are the first `num_beds` channel labels —
        // NOT `channel_labels[bed_id]` (`bed_indices` are OAMD bed ids, a
        // different space). Reuse the buffer.
        self.last_bed_labels.clear();
        if self.has_objects {
            for ch in 0..num_beds {
                if let Some(&lbl) = frame.channel_labels.get(ch) {
                    self.last_bed_labels.push(lbl);
                }
            }
        }

        if let Some(perf) = self.perf.as_mut() {
            perf.record(
                frame.sample_count,
                n_objects,
                !frame.metadata.is_empty(),
                render_time_ms,
                decode_time_ms,
            );
        }

        // Return scratch buffers for reuse next frame.
        self.pcm_f32_buf = pcm_f32;
        self.frame_events.clear();

        let n_channels = self.renderer.output_channel_count() as u32;

        if want_metering {
            let frame_duration_ms = sample_count as f32 / sample_rate as f32 * 1000.0;
            let drc_gain = self.drc_gain;
            if let Some(meter) = self.audio_meter.as_mut() {
                meter.process_speakers(&rendered.samples, n_channels as usize);
                meter.process_object_bands(&rendered.object_band_sq);
                if let Some(snapshot) = meter.poll() {
                    if overlay_active {
                        let levels: Vec<(u32, f64)> = snapshot
                            .object_levels
                            .iter()
                            .map(|&(id, _peak, rms)| (id, rms as f64))
                            .collect();
                        overlay::update_levels(&levels);
                    }
                    if let Some(osc) = self.osc.as_ref().filter(|_| want_meter_osc) {
                        // Latency/resample/adaptive args are output-stage specific
                        // and absent in the embedded host → None.
                        let _ = osc.send_meter_bundle(
                            &snapshot,
                            &rendered.object_gains,
                            &rendered.object_band_gains,
                            Some(decode_time_ms),
                            Some(rendered.crossover_time_ms),
                            Some(render_time_ms),
                            None,
                            Some(frame_duration_ms),
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            None,
                            Some(drc_gain),
                        );
                    }
                }
            }
        }

        Ok(Some(RenderedAudio {
            samples: rendered.samples,
            n_channels,
            n_frames: sample_count,
            sample_pos: sample_pos_at_start,
        }))
    }
}

/// Map the per-frame object metas to overlay positions `(id, x, y, z)`. The id
/// is the object's frame index, matching the `/omniphony/object/{id}` OSC id, so
/// the overlay keys colours and motion trails exactly as Studio did. Polar
/// objects carry no front-view cartesian position, so they sit at the origin —
/// identical to the previous Studio→Lua path, which zeroed non-cartesian
/// positions before sending them to the overlay.
fn overlay_positions(objects: &[ObjectMeta]) -> Vec<(u32, f64, f64, f64, String)> {
    objects
        .iter()
        .enumerate()
        .map(|(idx, o)| {
            let (x, y, z) = if o.coord_mode.eq_ignore_ascii_case("cartesian") {
                (o.x as f64, o.y as f64, o.z as f64)
            } else {
                (0.0, 0.0, 0.0)
            };
            (idx as u32, x, y, z, o.name.clone())
        })
        .collect()
}
