//! The headless decode→render session.
//!
//! [`Engine`] owns a loaded decoder bridge plugin and a [`SpatialRenderer`], and
//! turns raw compressed packets into VBAP-rendered interleaved multichannel PCM.
//! It performs no audio I/O: the host (the `orender` CLI, or `liborender.so`
//! inside mpv) feeds packets in and consumes rendered samples.

use crate::bridge_loader::{LoadedBridge, configure_presentation, resolve_bridge};
use crate::decode_step::{
    Declaration, DeclarationTracker, DecodedPacket, DrcModeSync, LogLevelSync, decode_packet,
};
use crate::frame_pipeline::{FrameOutput, FramePipeline, OutputStageFigures};
use crate::object_gen;
use crate::osc::OscSender;
use crate::overlay;
use crate::renderer_build::{SpatialRendererParams, build_spatial_renderer};
use anyhow::{Result, anyhow, bail};
use bridge_api::{RChannelLabel, RDecodedFrame, RInputTransport};
use renderer::config::Config;
use renderer::metering::AudioMeter;
use renderer::spatial_renderer::SpatialRenderer;
use renderer::speaker_layout::SpeakerLayout;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};

/// Monitoring cadences this host falls back to when the config declares none.
///
/// Lower than the CLI's: embedded in a player, the only consumer is the
/// in-process overlay, and the host is the one that may be running on
/// constrained hardware.
pub(crate) const EMBEDDED_METER_RATE_HZ: f32 = 10.0;
pub(crate) const EMBEDDED_DIAG_RATE_HZ: f32 = 10.0;

/// Options for the engine's OSC live-control server.
pub struct OscOptions {
    /// Monitoring target host (where outgoing VU/state bundles are sent).
    pub host: String,
    /// Monitoring target port.
    pub port_out: u16,
    /// Registration/listener port for incoming control; 0 = OS-assigned (logged).
    pub port_in: u16,
    /// Pre-subscribe the monitoring target to meter bundles (`render.osc_metering`),
    /// so it receives them without registering first.
    pub metering: bool,
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
    /// The host's timestamp for the packet this block was decoded from, as
    /// given to [`Engine::set_input_pts`], on the first block of that packet
    /// only; `None` on the others and when the host gave none. With the
    /// decode thread on, a packet's audio comes out a few calls after the
    /// packet went in, so this is how a host that stamps its output with its
    /// input timestamps finds which packet it is looking at.
    pub input_pts_us: Option<i64>,
}

/// Which bounded-buffer call held output is kept for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeldFor {
    /// [`Engine::process_raw_within`] with the same packet; moving on to
    /// another packet drops it.
    Packet,
    /// [`Engine::drain_with_capacity`]; `process` refuses input until then.
    Drain,
}

/// Who decides whether the engine decodes on a thread of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeThreadMode {
    /// Decode inline, whatever the live option says (the default).
    Off,
    /// Decode on the thread, whatever the live option says.
    On,
    /// Follow the live `decode_thread` option (config.yaml, Studio, OSC),
    /// switching at packet boundaries. A host picks this when it copes with
    /// either: it stamps its output from [`RenderedAudio::input_pts_us`] and
    /// drains at end of stream.
    Live,
}

/// A decode→render session: bridge plugin + spatial renderer + per-stream state.
pub struct Engine {
    /// Shared with the decode thread when there is one. Only that thread locks
    /// it per packet; everything else here is rare (a label change, a DRC-mode
    /// change, reset, drain).
    bridge: Arc<Mutex<LoadedBridge>>,
    /// Decodes packet N+1 on its own thread while this one renders packet N.
    /// Off unless the host asks for it: see [`Engine::set_decode_thread`].
    decode_worker: Option<DecodeWorker>,
    /// The bridge's `has_objects`, stored by whoever last held the lock. Hosts
    /// poll it after every packet; with the decode thread on, reading it here
    /// keeps that poll from waiting on the thread's decode.
    bridge_has_objects: Arc<AtomicBool>,
    /// Which packets decoded inline carry the bridge's declaration. The decode
    /// thread keeps its own: each counts the packets it decodes.
    declarations: DeclarationTracker,
    /// A declaration that came with a packet the bridge reported an error for:
    /// its frames are dropped, but not what it declared, which the next
    /// frames do not bring again.
    carried_declaration: Option<Declaration>,
    renderer: SpatialRenderer,
    sample_rate: u32,

    /// The per-frame sequence, shared with the CLI host (see
    /// [`crate::frame_pipeline`]), and the per-stream state it keeps. The
    /// stream's declaration comes from the last [`Declaration`] a decoded
    /// packet carried: applied from the frame it belongs to, kept until the
    /// next one (segment starts included, since the tracker has one read for
    /// them), and dropped by a [`reset`](Engine::reset).
    pipeline: FramePipeline,
    decoded_samples: u64,
    /// Dynamic object count of the last rendered frame (`channel_count − beds`),
    /// `0` for plain multichannel content. Surfaced over FFI for the host's track
    /// info display. Reset per segment.
    last_object_count: u32,
    /// Channel labels of the bed of the last object-based frame (empty for plain
    /// multichannel / no bed). Surfaced over FFI so the host can show the bed
    /// composition (e.g. "LFE+11 objects"). Reused buffer; reset per segment.
    last_bed_labels: Vec<RChannelLabel>,

    /// DRC mode last pushed to the bridge (selects which DRC words the decoder
    /// extracts → drives `frame.drc_gain`). Synced from the live param each
    /// `process` so config + OSC changes reach the decoder, as in the CLI.
    drc_mode: DrcModeSync,
    /// Log level last pushed to the bridge (first when it was opened), so its
    /// diagnostics follow `log_level` changes made over OSC.
    log_level: LogLevelSync,

    // ── reusable scratch ──
    /// Spare sample buffers for [`RenderedAudio`], returned by
    /// [`Engine::recycle`] once the host has copied them out.
    ///
    /// The buffers cannot simply live here, because every block of one packet is
    /// alive at the same time — the host copies them in one pass and may stop
    /// early on a layout change. So they are lent out and handed back. A host
    /// that never recycles allocates exactly as before; it is an optimisation,
    /// not a contract.
    output_pool: Vec<Vec<f32>>,
    /// Output a bounded-buffer call had no room for, and which call it is for
    /// (see [`Engine::process_raw_within`], [`Engine::drain_with_capacity`]):
    /// the retry with a larger buffer gets it back, since decoding again
    /// cannot recreate it.
    held: Option<(HeldFor, Vec<RenderedAudio>)>,
    /// The packet [`HeldFor::Packet`] output was rendered from.
    held_packet: Vec<u8>,
    /// Whether the host or the live option decides on the decode thread.
    decode_thread_mode: DecodeThreadMode,
    /// The host's timestamp for the packet about to be pushed, from
    /// [`set_input_pts`](Self::set_input_pts); taken by that packet.
    input_pts_us: Option<i64>,
    /// [`RenderedAudio::input_pts_us`] of the first block the last
    /// bounded-buffer call handed back, for hosts reading it through the C ABI.
    last_output_input_pts: Option<i64>,

    /// Optional OSC live-control server (kept alive here; its Drop stops the
    /// listener thread when the engine is dropped).
    osc: Option<OscSender>,
    /// VU meter, created with the OSC server; feeds outgoing meter bundles.
    audio_meter: Option<AudioMeter>,
    /// Opt-in per-frame perf accumulator (env `ORENDER_PERF_LOG`). `None` = off
    /// (zero overhead). Diagnostic only; safe to remove once perf work lands.
    perf: Option<PerfLog>,
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
        // Join the decode thread the same way, so the bridge it shares is
        // released here, by the engine's own thread, and not by that one.
        drop(self.decode_worker.take());
        if let Some(perf) = self.perf.as_mut() {
            perf.flush();
        }
    }
}

impl Engine {
    /// Build a session around an already-loaded bridge and a constructed
    /// renderer. The bridge must already be configured (presentation, DRC mode)
    /// before the first [`process`](Self::process) call.
    pub fn new(mut bridge: LoadedBridge, renderer: SpatialRenderer, sample_rate: u32) -> Self {
        crate::bridge_loader::declare_source_families(&bridge.lib, &renderer.renderer_control());
        // Checked before each packet without locking the bridge.
        let log_level = std::mem::take(&mut bridge.log_level);
        let coordinate_format = bridge.bridge.coordinate_format();
        let bridge_has_objects = Arc::new(AtomicBool::new(bridge.bridge.has_objects()));
        let engine = Self {
            bridge: Arc::new(Mutex::new(bridge)),
            decode_worker: None,
            bridge_has_objects,
            declarations: DeclarationTracker::new(),
            carried_declaration: None,
            renderer,
            sample_rate,
            pipeline: FramePipeline::new(coordinate_format),
            decoded_samples: 0,
            last_object_count: 0,
            last_bed_labels: Vec::new(),
            drc_mode: DrcModeSync::new(),
            log_level,
            output_pool: Vec::new(),
            held: None,
            held_packet: Vec::new(),
            decode_thread_mode: DecodeThreadMode::Off,
            input_pts_us: None,
            last_output_input_pts: None,
            osc: None,
            audio_meter: None,
            perf: std::env::var_os("ORENDER_PERF_LOG")
                .is_some()
                .then(PerfLog::new),
        };
        engine
            .pipeline
            .stream
            .channel_objects
            .publish_static_state(&engine.renderer.renderer_control());
        engine
    }

    /// Register a host-supplied (out-of-tree) bed→height object generator so it
    /// can be selected by id and appears in Studio's selector + parameter sliders.
    /// Call at startup (before `enable_osc`); a later call re-publishes the schema.
    pub fn register_object_generator(
        &mut self,
        factory: Box<dyn object_gen::ObjectGeneratorFactory>,
    ) {
        self.pipeline
            .stream
            .channel_objects
            .register_generator(factory);
        self.pipeline
            .stream
            .channel_objects
            .publish_static_state(&self.renderer.renderer_control());
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

        // The generator catalogue (built-ins + any host-registered out-of-tree
        // generators), the phantom-extraction schema and the fixed-channel
        // catalogue, so the live-state bundle carries them to Studio.
        self.pipeline
            .stream
            .channel_objects
            .publish_static_state(&self.renderer.renderer_control());

        let target = SocketAddrV4::from_str(&format!("{}:{}", opts.host, opts.port_out))
            .map_err(|e| anyhow!("invalid OSC target {}:{}: {e}", opts.host, opts.port_out))?;
        let mut sender = OscSender::new(target)?;
        if opts.metering {
            sender.set_default_metering(true);
        }
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
        // Client-visible profiles view (active name + list), applied to the
        // control below once it exists; see docs/config-profiles.md.
        let profiles_info = loaded_cfg.as_ref().map(Config::profiles_info);
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
        // Honour `render.presentation` like the CLI does. This host has no
        // flags, so the config is the only way to ask for anything other than
        // the default — hard-coding it here made the setting silently
        // inoperative for everything played through mpv.
        let presentation = render_cfg
            .as_ref()
            .and_then(renderer::config_fields::presentation::get)
            .map(|p| p.to_string())
            .unwrap_or_else(|| renderer::config_fields::presentation::DEFAULT.to_string());
        if let Err(e) = configure_presentation(&mut bridge.bridge, &presentation) {
            // Not fatal here, unlike the CLI: this host is a decoder inside a
            // player, and refusing to start would drop playback entirely where
            // falling back to the bridge's own default still plays.
            log::warn!("{e}; keeping the bridge default");
        }
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
        let mut renderer = build_spatial_renderer(
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
            // Surfaced in Studio's About to catch host config mismatches. A
            // restored sidecar that was the previous instance's fallback
            // keeps parse_error, whatever the file now holds.
            let status = renderer::config::boot_load_status(path);
            match status {
                renderer::config::ConfigLoadStatus::Loaded => {}
                renderer::config::ConfigLoadStatus::NewerSchema => log::warn!(
                    "config '{}' was written by a newer Omniphony; running on what this build \
                     understands of it, and leaving the file untouched",
                    path.display()
                ),
                _ => log::warn!(
                    "config '{}' not loaded ({}); running on built-in defaults",
                    path.display(),
                    status.as_str()
                ),
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

        // The path asked for (host override, else the config's), not the
        // resolved one: an auto-discovered bridge next to the host binary must
        // not end up in the shared config on the next save.
        crate::renderer_build::record_bridge_path(&control, bridge_path, config_bridge.as_deref());
        // This host reads its input from the player, not from a pipe: keep the
        // config's `render.input_pipe` as is on the next save.
        crate::renderer_build::record_input_path(&control, render_cfg.as_ref());
        if let Some(info) = profiles_info {
            control.set_profiles_info(info);
        }

        // This host publishes monitoring slower than the CLI: it is embedded in
        // a player, where the overlay is the only consumer. Declared before the
        // seed below, which falls back to it, and re-read by any later profile
        // switch.
        control.set_cadence_defaults_hz(EMBEDDED_METER_RATE_HZ, EMBEDDED_DIAG_RATE_HZ);

        // Monitoring cadences, ramp mode, declared live options and the DRC
        // selection: seeded through the shared runtime seed — the same call
        // the CLI-shaped bootstrap semantics expect and the one the live
        // profile switch replays, so the embedded host cannot drift from
        // either (FFI/CLI parity by construction; see docs/config-profiles.md).
        crate::renderer_build::seed_runtime_state_from_render_config(&control, render_cfg.as_ref());

        // Publish the bridge's supported DRC modes (so studio shows the DRC
        // control). The decode-side mode itself is pushed to the bridge lazily
        // in `process` (see `sync_live_options`), mirroring the CLI's decoder thread.
        let supported_drc: Vec<String> = bridge
            .bridge
            .supported_drc_modes()
            .iter()
            .map(|m| m.as_str().to_string())
            .collect();
        control.set_bridge_supported_drc_modes(supported_drc);

        // The band engines (a gain table per crossover band), now that the
        // seed above has set the backend and the crossover engine: here, not
        // on the first frame the player pulls.
        renderer.prepare_speaker_stage()?;

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

    /// Per-channel labels of the rendered output, one entry per output channel
    /// in render order: the layout's speakers, or the binaural pair. The host
    /// (mpv) turns this into a channel map, so it must match
    /// [`channel_count`](Self::channel_count) or the frame is malformed
    /// (silence). Names that do not resolve map to [`RChannelLabel::Unknown`].
    /// Same source as the standalone renderer's sink labels
    /// ([`SpatialRenderer::output_channel_names`]).
    pub fn channel_layout(&self) -> Vec<RChannelLabel> {
        self.renderer
            .output_channel_names()
            .iter()
            .map(|name| crate::channel_layout::label_for_speaker_name(name))
            .collect()
    }

    /// Whether the current presentation carries dynamic objects. A live fact
    /// about the stream (it may flip mid-stream); hosts must not latch it.
    ///
    /// As of the latest packet decoded: with the decode thread on, that can be
    /// ahead of the audio returned so far by what is in flight: about 30 ms of
    /// audio, or one packet if that is longer. Never waits on the bridge.
    pub fn has_objects(&self) -> bool {
        self.bridge_has_objects.load(Ordering::Relaxed)
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
        self.pipeline.stream.dialnorm
    }

    /// Channel labels of the bed of the last object-based frame (empty for plain
    /// multichannel / no bed). For the host's track info display.
    pub fn bed_labels(&self) -> &[RChannelLabel] {
        &self.last_bed_labels
    }

    /// The name the bridge gives the current presentation's format
    /// (`DTS-HD MA + DTS:X 7.1.4`, `Dolby TrueHD + Dolby Atmos`, …), empty
    /// when it states none. Declaration-level: refreshed with the channel
    /// labels, never per frame. For the host's track info display.
    pub fn source_label(&self) -> &str {
        &self.pipeline.stream.declaration.label
    }

    /// Constant DSP latency of the rendered output, in samples at the engine
    /// sample rate (see [`SpatialRenderer::output_latency_samples`]). 0 for
    /// the default filters; non-zero when the linear-phase FIR crossover sits
    /// on the rendered path. May change mid-stream (live option / output-mode
    /// switch); meaningful after the first processed packet. The host uses it
    /// to shift output timestamps for A/V sync.
    pub fn output_latency_samples(&self) -> u64 {
        self.renderer.output_latency_samples() as u64
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

    /// Where the listener is: `us` microseconds into the stream, counted as
    /// the timestamps this engine hands back are (`*out_pts_us`), so from 0
    /// after [`reset`](Self::reset). A host that buffers the rendered audio
    /// plays it later than it renders it, and only the host knows by how much;
    /// this passes it on to OSC clients ([`OscSender::send_heard`]), which can
    /// then show each block when it is heard. The engine itself holds nothing
    /// back.
    pub fn set_heard_us(&mut self, us: i64) {
        let rate = self.sample_rate.max(1);
        // Rounded up: the timestamps are rounded down, so a block's own start
        // comes back to exactly its position rather than a sample short of it.
        let pos = (i128::from(us.max(0)) * i128::from(rate) + 999_999) / 1_000_000;
        if let Some(osc) = self.osc.as_ref() {
            osc.send_heard(u64::try_from(pos).unwrap_or(u64::MAX), rate);
        }
    }

    /// Reset the session after a seek or stream discontinuity. Flushes the
    /// bridge pipeline and the renderer's per-object/ramp state, and clears the
    /// per-stream spatial state. Live parameters (gains, layout, OSC-applied
    /// settings) are preserved — a seek must not lose live adjustments.
    pub fn reset(&mut self) {
        // Audio held for a retry belongs to the stream being flushed.
        if let Some((_, held)) = self.held.take() {
            self.recycle(held);
        }
        // So is whatever the decode thread is still working on.
        self.discard_in_flight();
        self.input_pts_us = None;
        self.last_output_input_pts = None;
        // Nothing is in flight now: a switch the live option asked for while
        // packets were on the thread can land here instead of winding down.
        let want_thread = self.live_decode_thread();
        self.follow_live_decode_thread(want_thread);
        {
            let mut bridge = self.lock_bridge();
            bridge.bridge.reset();
            self.bridge_has_objects
                .store(bridge.bridge.has_objects(), Ordering::Relaxed);
        }
        // The bridge restarts: its next frames come with a declaration read
        // anew, whatever their labels, and the old one no longer applies.
        self.declarations.forget();
        if let Some(worker) = self.decode_worker.as_mut() {
            worker.fresh = true;
        }
        self.carried_declaration = None;
        self.pipeline.stream.declaration = Default::default();
        self.renderer.reset_runtime_state();
        self.reset_segment_state();
        // Object frames are delta-encoded; after a seek the (static) virtual-bed
        // poses would never be re-sent, so force a full re-emit of object
        // positions + names on the next frame.
        // The positions start again from 0, and the next block is marked anew.
        if let Some(osc) = self.osc.as_mut() {
            osc.request_full_object_resend();
            osc.rewind_playout();
        }
        self.decoded_samples = 0;
        self.pipeline.stream.drc = Default::default();
        // Drop overlay scene + motion trails so they don't bridge the seek.
        overlay::clear();
    }

    /// Drop what a segment start invalidates. Not the bridge's declaration:
    /// a segment start or a bridge reset comes with a fresh one (see
    /// [`DeclarationTracker`]), applied to the frame right after this.
    fn reset_segment_state(&mut self) {
        self.pipeline
            .stream
            .reset_segment(Some(&self.renderer.renderer_control()));
        self.reset_track_info();
    }

    /// A segment starts (`is_new_segment`, or the bridge reset itself): the
    /// shared segment start, this host's track info, and the overlay, so the
    /// previous layout's objects do not linger in it.
    fn begin_segment(&mut self) {
        self.pipeline
            .stream
            .begin_segment(&self.renderer, self.osc.as_mut());
        self.reset_track_info();
        overlay::clear();
    }

    /// A new segment may declare a different object count or bed.
    fn reset_track_info(&mut self) {
        self.last_object_count = 0;
        self.last_bed_labels.clear();
    }

    /// Bring the decoder in line with the live options it follows, before the
    /// next packet: the DRC mode (which DRC words the decoder extracts; mirrors
    /// the CLI's [`DrcModeSync`]), the log level and, in
    /// [`DecodeThreadMode::Live`], the decode thread. One read of the live
    /// params per packet; the bridge is locked only when the DRC mode or the
    /// log level changed. The bridge preserves both across `reset`, so a seek
    /// keeps them.
    fn sync_live_options(&mut self) {
        let (drc_changed, want_thread) = {
            let control = self.renderer.renderer_control();
            let live = control.live.read();
            (self.drc_mode.update(&live.drc_mode), live.decode_thread)
        };
        if drc_changed {
            self.lock_bridge()
                .bridge
                .set_drc_mode(self.drc_mode.mode().into());
        }
        if self.log_level.update(live_log::current_runtime_level()) {
            let mut bridge = self.bridge.lock().unwrap_or_else(|e| e.into_inner());
            self.log_level.push(&mut bridge.bridge);
        }
        self.follow_live_decode_thread(want_thread);
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
        if matches!(self.held, Some((HeldFor::Drain, _))) {
            bail!("drain output is pending; retry drain with a larger buffer before new input");
        }
        // Push any DRC-mode, log-level or decode-thread change (config-seeded
        // or OSC-driven) to the decoder before it decodes this packet.
        self.sync_live_options();
        let pts = self.input_pts_us.take();

        if self.decode_worker.is_some() {
            return self.process_pipelined(data, transport, data_type, pts);
        }

        let packet = {
            let mut bridge = self.bridge.lock().unwrap_or_else(|e| e.into_inner());
            let packet = decode_packet(
                &mut bridge.bridge,
                data,
                transport,
                data_type,
                &mut self.declarations,
            );
            self.bridge_has_objects
                .store(bridge.bridge.has_objects(), Ordering::Relaxed);
            packet
        };
        self.render_decoded(packet, pts)
    }

    /// The host's timestamp for the next packet it pushes, carried with that
    /// packet's audio as [`RenderedAudio::input_pts_us`]. Optional: a host that
    /// never calls it gets `None` there.
    pub fn set_input_pts(&mut self, pts_us: Option<i64>) {
        self.input_pts_us = pts_us;
    }

    /// [`RenderedAudio::input_pts_us`] of the first block the last
    /// [`process_raw_within`](Self::process_raw_within) or
    /// [`drain_with_capacity`](Self::drain_with_capacity) call handed back:
    /// `None` when it handed back nothing, or the host gave no timestamp.
    pub fn last_output_input_pts(&self) -> Option<i64> {
        self.last_output_input_pts
    }

    /// Render one packet's worth of decoded frames: the second half of
    /// [`process`](Self::process), shared by the inline and threaded paths.
    fn render_decoded(
        &mut self,
        packet: DecodedPacket,
        pts: Option<i64>,
    ) -> Result<Vec<RenderedAudio>> {
        let per_frame_decode_time_ms = packet.decode_ms_per_frame();
        let DecodedPacket {
            result,
            declaration,
            declaration_frame,
            ..
        } = packet;
        let (mut declaration, declaration_frame) = match declaration {
            Some(own) => {
                self.carried_declaration = None;
                (Some(own), declaration_frame)
            }
            None => (self.carried_declaration.take(), 0),
        };
        if !result.error_message.is_empty() {
            self.carried_declaration = declaration;
            bail!("bridge decode error: {}", result.error_message);
        }
        if result.did_reset {
            // Sync-loss recovery / seek inside the bridge: drop stale spatial
            // state but keep live params and the absolute sample clock. Also bump
            // the content generation, force a full object re-emit and clear the
            // overlay so OSC clients and the overlay purge the pre-seek objects
            // instead of leaving them behind as stale duplicates.
            self.begin_segment();
        }

        let mut out = Vec::with_capacity(result.frames.len());
        for (i, frame) in result.frames.iter().enumerate() {
            let declaration = if i == declaration_frame {
                declaration.take()
            } else {
                None
            };
            if let Some(chunk) = self.render_frame(frame, per_frame_decode_time_ms, declaration)? {
                out.push(chunk);
            }
        }
        if let Some(first) = out.first_mut() {
            first.input_pts_us = pts;
        }
        Ok(out)
    }

    /// Convenience wrapper for hosts that always feed raw access units.
    pub fn process_raw(&mut self, data: &[u8]) -> Result<Vec<RenderedAudio>> {
        self.process(data, RInputTransport::Raw, 0)
    }

    /// [`process_raw`](Self::process_raw) for a host whose output buffer holds
    /// `capacity` interleaved samples.
    ///
    /// Returns `Ok(Some(blocks))` when they fit, and `Ok(None)` when they do
    /// not: the host is expected to call again with the **same packet** and a
    /// larger buffer. The packet has already been decoded by then, and
    /// decoding it again would advance the bridge a second time — the stream
    /// would jump ahead and this packet's audio would be lost. So the blocks
    /// are held, and the retry returns them without touching the decoder.
    ///
    /// A host that moves on to a different packet instead of retrying gets
    /// that packet decoded normally; the held audio is dropped, as it was
    /// before retries were possible.
    pub fn process_raw_within(
        &mut self,
        data: &[u8],
        capacity: usize,
    ) -> Result<Option<Vec<RenderedAudio>>> {
        let chunks = match self.held.take() {
            Some((HeldFor::Packet, held)) if self.held_packet == data => held,
            Some((HeldFor::Packet, stale)) => {
                self.recycle(stale);
                self.process_raw(data)?
            }
            // Held drain output stays held: `process` refuses the packet.
            held => {
                self.held = held;
                self.process_raw(data)?
            }
        };
        let out = self.fit_or_hold(chunks, capacity, HeldFor::Packet);
        if out.is_none() {
            self.held_packet.clear();
            self.held_packet.extend_from_slice(data);
        }
        Ok(out)
    }

    /// `chunks` when they fit in `capacity` interleaved samples; otherwise
    /// `None`, and they are held for the retry `held_for` names. Either way it
    /// records the timestamp [`last_output_input_pts`](Self::last_output_input_pts)
    /// reports.
    fn fit_or_hold(
        &mut self,
        chunks: Vec<RenderedAudio>,
        capacity: usize,
        held_for: HeldFor,
    ) -> Option<Vec<RenderedAudio>> {
        if chunks.iter().map(|c| c.samples.len()).sum::<usize>() > capacity {
            self.held = Some((held_for, chunks));
            self.last_output_input_pts = None;
            return None;
        }
        self.last_output_input_pts = chunks.first().and_then(|c| c.input_pts_us);
        Some(chunks)
    }

    /// Render what the engine still holds, because the stream is over: with the
    /// decode thread on, the packets it has been handed and not returned yet.
    /// One packet's audio per call, oldest first, as [`process`](Self::process)
    /// returns it, so a buffer that fits one packet's audio fits a drain too:
    /// the host calls this at end of stream until it returns nothing, and plays
    /// what comes back.
    ///
    /// Not a [`reset`](Self::reset): the renderer keeps its per-object and ramp
    /// state, because these frames are the continuation of the ones before them.
    /// Safe to call on an idle engine, and safe to call again once it has
    /// returned nothing — it returns nothing again.
    pub fn drain(&mut self) -> Result<Vec<RenderedAudio>> {
        match self.held.take() {
            Some((HeldFor::Drain, held)) => return Ok(held),
            // A retry that never came: the host has moved on, as with a new packet.
            Some((HeldFor::Packet, held)) => self.recycle(held),
            None => {}
        }
        self.next_in_flight()
    }

    /// Drain for a host with bounded output capacity. None means retry this
    /// method with more space, before pushing further input. Keep the rendered
    /// audio on a short buffer: calling the decoder again cannot recreate it.
    pub fn drain_with_capacity(
        &mut self,
        max_samples: usize,
    ) -> Result<Option<Vec<RenderedAudio>>> {
        let chunks = self.drain()?;
        Ok(self.fit_or_hold(chunks, max_samples, HeldFor::Drain))
    }

    /// Decode on a thread of its own, overlapping the render, so the two share
    /// the work across two cores. Off by default, because a packet's audio then
    /// comes back from a later [`process`](Self::process) call - about 30 ms
    /// of audio behind, or one packet if that is longer; one packet's audio
    /// per call, as inline, except while the queue shrinks back to its limit,
    /// when a call returns two - or from [`drain`](Self::drain), and not every host
    /// allows for that: one that turns it on takes its timestamps from the
    /// blocks it gets back ([`RenderedAudio::sample_pos`] for its place in the
    /// stream, [`RenderedAudio::input_pts_us`] for the host's own timestamp)
    /// and drains at end of stream.
    ///
    /// Switch it while nothing is in flight: before the first packet, after
    /// [`reset`](Self::reset), or once [`drain`](Self::drain) has returned
    /// nothing. Turning it off with packets still on the thread is refused
    /// rather than losing them.
    pub fn set_decode_thread(&mut self, on: bool) -> Result<()> {
        self.switch_decode_thread(on, false)?;
        self.decode_thread_mode = if on {
            DecodeThreadMode::On
        } else {
            DecodeThreadMode::Off
        };
        Ok(())
    }

    /// Choose who decides on the decode thread: the host, forcing it
    /// [`On`](DecodeThreadMode::On) or [`Off`](DecodeThreadMode::Off) as
    /// [`set_decode_thread`](Self::set_decode_thread) does, or the live
    /// `decode_thread` option ([`Live`](DecodeThreadMode::Live)), which the
    /// engine then follows at packet boundaries, in both directions and with
    /// packets in flight: turned off, the thread winds down a packet per call
    /// and the engine decodes inline once it is empty.
    pub fn set_decode_thread_mode(&mut self, mode: DecodeThreadMode) -> Result<()> {
        match mode {
            DecodeThreadMode::On => self.set_decode_thread(true),
            DecodeThreadMode::Off => self.set_decode_thread(false),
            DecodeThreadMode::Live => {
                self.decode_thread_mode = DecodeThreadMode::Live;
                let want = self.live_decode_thread();
                self.follow_live_decode_thread(want);
                Ok(())
            }
        }
    }

    /// Who decides on the decode thread.
    pub fn decode_thread_mode(&self) -> DecodeThreadMode {
        self.decode_thread_mode
    }

    /// Whether the engine decodes on a thread of its own right now (it may
    /// still be winding down after the live option was turned off).
    pub fn decode_thread(&self) -> bool {
        self.decode_worker.is_some()
    }

    fn start_decode_worker(&mut self) -> Result<()> {
        // The thread counts packets with a tracker of its own. When it stops,
        // the inline one has missed them all: start it over now.
        self.declarations.forget();
        let has_objects = self.lock_bridge().bridge.has_objects();
        self.bridge_has_objects
            .store(has_objects, Ordering::Relaxed);
        self.decode_worker = Some(DecodeWorker::spawn(
            &self.bridge,
            &self.bridge_has_objects,
            PIPELINE_DEPTH,
        )?);
        Ok(())
    }

    /// Start, keep or stop the decode thread, at a packet boundary. Starting
    /// it is immediate. Stopping it with packets in flight would lose them or
    /// return them all at once, so either the thread winds down (`wind_down`,
    /// the live option: its limit drops to zero, each call hands back up to two
    /// packets while taking one, and once it is empty the next call decodes
    /// inline) or the switch is refused (a host forcing it off).
    fn switch_decode_thread(&mut self, on: bool, wind_down: bool) -> Result<()> {
        match (on, self.decode_worker.as_ref().map(|w| w.in_flight)) {
            (true, None) => self.start_decode_worker()?,
            (false, None) => {}
            (false, Some(0)) => self.decode_worker = None,
            (false, Some(_)) if !wind_down => {
                bail!("packets are still on the decode thread; drain or reset first")
            }
            (_, Some(_)) => {
                if let Some(worker) = self.decode_worker.as_mut() {
                    worker.winding_down = !on;
                }
            }
        }
        Ok(())
    }

    /// The live `decode_thread` option.
    fn live_decode_thread(&self) -> bool {
        self.renderer.renderer_control().live.read().decode_thread
    }

    /// In [`DecodeThreadMode::Live`], bring the decode thread in line with the
    /// live option `want` (see [`switch_decode_thread`](Self::switch_decode_thread)).
    fn follow_live_decode_thread(&mut self, want: bool) {
        if self.decode_thread_mode != DecodeThreadMode::Live {
            return;
        }
        if let Err(e) = self.switch_decode_thread(want, true) {
            log::warn!("decode thread requested but not started, decoding inline: {e:#}");
        }
    }

    fn lock_bridge(&self) -> MutexGuard<'_, LoadedBridge> {
        self.bridge.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn process_pipelined(
        &mut self,
        data: &[u8],
        transport: RInputTransport,
        data_type: u8,
        pts: Option<i64>,
    ) -> Result<Vec<RenderedAudio>> {
        let worker = self
            .decode_worker
            .as_mut()
            .expect("pipelined without a worker");
        worker.submit(data, transport, data_type, pts)?;
        // One packet's audio per call, as inline, only later: a host that paces
        // its feeding by the blocks it gets back - Kodi hands over a packet for
        // each empty answer - would otherwise be given several packets' audio in
        // one block and feed less than it plays. Past the limit, wait for the
        // oldest; otherwise take it only if it is ready.
        let limit = worker.limit();
        let done = if worker.in_flight > limit {
            worker.wait()?
        } else {
            match worker.poll()? {
                Some(done) => done,
                None => return Ok(Vec::new()),
            }
        };
        let mut out = self.render_decoded(done.packet, done.pts)?;
        // Still past the limit, because it has just come down: give back one
        // more, so the queue shrinks to it a packet per call. Waited for, not
        // polled: when decoding is the slower half the next one is never ready
        // yet, and the queue would stay where it was.
        let worker = self
            .decode_worker
            .as_mut()
            .expect("pipelined without a worker");
        if worker.in_flight > limit {
            let extra = worker.wait()?;
            out.extend(self.render_decoded(extra.packet, extra.pts)?);
        }
        Ok(out)
    }

    /// The oldest packet the decode thread still holds that renders to any
    /// audio, rendered; nothing once it holds none.
    fn next_in_flight(&mut self) -> Result<Vec<RenderedAudio>> {
        while self.decode_worker.as_ref().is_some_and(|w| w.in_flight > 0) {
            let done = self.decode_worker.as_mut().unwrap().wait()?;
            let out = self.render_decoded(done.packet, done.pts)?;
            if !out.is_empty() {
                return Ok(out);
            }
        }
        Ok(Vec::new())
    }

    /// Wait for everything the decode thread still holds and throw it away.
    fn discard_in_flight(&mut self) {
        if let Some(worker) = self.decode_worker.as_mut() {
            while worker.in_flight > 0 {
                if worker.wait().is_err() {
                    break;
                }
            }
        }
    }

    /// Hand the sample buffers of a consumed [`process`](Self::process) result
    /// back for reuse.
    ///
    /// Optional: dropping the blocks instead is correct, just one allocation per
    /// block per packet. Call it once the samples have been copied out — the
    /// buffers are recycled as-is, so anything still reading them is reading a
    /// buffer the next frame will overwrite.
    ///
    /// Bounded, because a host is free to call this with more blocks than it
    /// ever renders at once and the pool would otherwise only ever grow.
    pub fn recycle(&mut self, chunks: Vec<RenderedAudio>) {
        const MAX_POOLED: usize = 16;
        for chunk in chunks {
            if self.output_pool.len() >= MAX_POOLED {
                break;
            }
            self.output_pool.push(chunk.samples);
        }
    }

    fn render_frame(
        &mut self,
        frame: &RDecodedFrame,
        decode_time_ms: f32,
        declaration: Option<Declaration>,
    ) -> Result<Option<RenderedAudio>> {
        let channel_count = frame.channel_count as usize;
        let sample_count = frame.sample_count as usize;
        let sample_pos_at_start = self.decoded_samples;

        // A mid-stream format change (the initial TrueHD layout settling, or a
        // 7.1<->5.1 boundary) invalidates the per-segment spatial state: a
        // channel-based segment must never stay on a previous object-based (or
        // differently-sized) layout — the cause of a 5.1 track not
        // spatializing until a track swap. The content generation is bumped
        // and a full re-emit forced so OSC clients and the overlay purge the
        // previous layout's objects.
        if frame.is_new_segment {
            self.begin_segment();
        }
        if let Some(declaration) = declaration {
            let control = self.renderer.renderer_control();
            let live = control.live.read();
            self.pipeline
                .stream
                .apply_declaration(declaration, &live.placement);
        }

        self.pipeline.prepare(
            frame,
            sample_pos_at_start,
            Some(&mut self.renderer),
            self.osc.as_mut(),
        )?;
        self.decoded_samples += sample_count as u64;

        // This host has no live input: a stream that carries objects takes
        // the object path on every frame.
        let objects = self.pipeline.stream.has_objects;
        let donated = self.output_pool.pop().unwrap_or_default();
        let render = self.pipeline.render(
            frame,
            sample_pos_at_start,
            objects,
            &mut self.renderer,
            self.osc.as_mut(),
            &mut self.audio_meter,
            donated,
            decode_time_ms,
            &OutputStageFigures::default(),
        )?;
        let (samples, n_channels) = match render.output {
            FrameOutput::Rendered { samples, channels } => (samples, channels as u32),
            FrameOutput::Silence { samples, channels } => {
                return Ok(Some(self.silence_block(samples, channels, sample_count)));
            }
            // The mpv decoder declines at the spatial probe and falls back to
            // ad_lavc, so this only runs for the discarded probe frame: emit
            // silence so the host still advances by the frame's sample count.
            FrameOutput::Passthrough { unused } => {
                let channels = self.renderer.output_channel_count();
                return Ok(Some(self.silence_block(unused, channels, sample_count)));
            }
        };

        // Dynamic object count = decoded channels minus the fixed channels.
        // Only meaningful for object-based content; plain multichannel
        // reports 0.
        let stream = &self.pipeline.stream;
        let num_beds = stream.fixed_planner.fixed_labels().len();
        let n_objects = (channel_count as u32).saturating_sub(num_beds as u32);
        self.last_object_count = if stream.has_objects { n_objects } else { 0 };

        // Bed composition for the host's track-info display, e.g. "LFE+11
        // objects". The renderer lays bed channels out first in PCM order (see
        // `build_spatial_channel_events`: beds at channels 0..num_beds, objects
        // after), so the bed labels are the first `num_beds` channel labels —
        // NOT `channel_labels[bed_id]` (`bed_indices` are OAMD bed ids, a
        // different space). Reuse the buffer.
        self.last_bed_labels.clear();
        if stream.has_objects {
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
                render.render_ms,
                decode_time_ms,
            );
        }

        Ok(Some(RenderedAudio {
            samples,
            n_channels,
            n_frames: sample_count,
            sample_pos: sample_pos_at_start,
            input_pts_us: None,
        }))
    }

    /// A block of silence `channels` wide for a frame of `sample_count`,
    /// written into `samples`.
    fn silence_block(
        &self,
        mut samples: Vec<f32>,
        channels: usize,
        sample_count: usize,
    ) -> RenderedAudio {
        samples.clear();
        samples.resize(sample_count * channels, 0.0);
        RenderedAudio {
            samples,
            n_channels: channels as u32,
            n_frames: sample_count,
            sample_pos: self.decoded_samples - sample_count as u64,
            input_pts_us: None,
        }
    }
}

/// Audio the decode thread may hold before the renderer waits for a packet.
/// Its job is to let the thread run ahead through cheap packets so a dear one
/// (a major sync) does not stall the renderer. Counted in audio, not packets,
/// because a packet is 0.8 ms of TrueHD but 32 ms of E-AC-3: this is 36 TrueHD
/// access units, capped at [`PIPELINE_DEPTH`], and one E-AC-3 syncframe - never
/// fewer than one packet, or nothing would overlap.
const PIPELINE_LAG_SECS: f64 = 0.030;

/// Packets the decode thread may hold at most, whatever their length. On a
/// real TrueHD Atmos track 2, 32 and 128 came within about 5% of each other.
const PIPELINE_DEPTH: usize = 32;

struct DecodeJob {
    data: Vec<u8>,
    transport: RInputTransport,
    data_type: u8,
    /// First packet since the thread started or the engine forgot the bridge's
    /// declaration: capture it with this one whatever its labels.
    fresh: bool,
    /// The host's timestamp for this packet, handed back with its audio.
    pts: Option<i64>,
}

struct DecodeDone {
    /// The job's buffer, handed back to copy a later packet into.
    data: Vec<u8>,
    /// With the bridge's declaration read under the same lock as the decode,
    /// when the thread's tracker says a frame needs it.
    packet: DecodedPacket,
    /// The job's [`DecodeJob::pts`].
    pts: Option<i64>,
}

/// The bridge's decode, one packet at a time, on a thread of its own.
struct DecodeWorker {
    jobs: Option<mpsc::SyncSender<DecodeJob>>,
    done: mpsc::Receiver<DecodeDone>,
    in_flight: usize,
    depth: usize,
    /// Audio per packet handed back, averaged over about the last `depth`
    /// of them: it turns [`PIPELINE_LAG_SECS`] into packets. Recent ones, so
    /// a host feeding fixed-size chunks gets a quiet passage's chunks measured
    /// by their own length. Zero until one has had any audio.
    packet_secs: f64,
    /// Buffers of returned jobs, so a packet is copied, not allocated.
    spare: Vec<Vec<u8>>,
    /// The next packet is the first since the thread started or a reset.
    fresh: bool,
    /// The live option turned the thread off with packets in flight: hold
    /// none, so the queue empties a packet per call and the engine can go
    /// back to decoding inline.
    winding_down: bool,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DecodeWorker {
    fn spawn(
        bridge: &Arc<Mutex<LoadedBridge>>,
        has_objects: &Arc<AtomicBool>,
        depth: usize,
    ) -> Result<Self> {
        let (jobs, job_rx) = mpsc::sync_channel::<DecodeJob>(depth + 1);
        let (done_tx, done) = mpsc::channel::<DecodeDone>();
        let bridge = Arc::clone(bridge);
        let has_objects = Arc::clone(has_objects);
        let thread = std::thread::Builder::new()
            .name("orender-decode".into())
            .spawn(move || {
                // Read the declaration under the decode's lock, with the packet
                // it follows: by the time the engine renders it, the bridge is
                // on later packets.
                let mut declarations = DeclarationTracker::new();
                for job in job_rx {
                    if job.fresh {
                        declarations.forget();
                    }
                    let mut guard = bridge.lock().unwrap_or_else(|e| e.into_inner());
                    let packet = decode_packet(
                        &mut guard.bridge,
                        &job.data,
                        job.transport,
                        job.data_type,
                        &mut declarations,
                    );
                    has_objects.store(guard.bridge.has_objects(), Ordering::Relaxed);
                    drop(guard);
                    if done_tx
                        .send(DecodeDone {
                            data: job.data,
                            packet,
                            pts: job.pts,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            })
            .map_err(|e| anyhow!("cannot start the decode thread: {e}"))?;
        Ok(Self {
            jobs: Some(jobs),
            done,
            in_flight: 0,
            depth,
            packet_secs: 0.0,
            spare: Vec::new(),
            fresh: true,
            winding_down: false,
            thread: Some(thread),
        })
    }

    /// Packets that may be in flight: as many as make up [`PIPELINE_LAG_SECS`]
    /// at the length of those returned lately, at least one, at most `depth`.
    /// One until a packet has returned any audio to measure by; none while
    /// [`winding_down`](Self::winding_down).
    fn limit(&self) -> usize {
        if self.winding_down {
            return 0;
        }
        if self.packet_secs <= 0.0 {
            return 1;
        }
        ((PIPELINE_LAG_SECS / self.packet_secs).ceil() as usize).clamp(1, self.depth)
    }

    fn submit(
        &mut self,
        data: &[u8],
        transport: RInputTransport,
        data_type: u8,
        pts: Option<i64>,
    ) -> Result<()> {
        let fresh = std::mem::take(&mut self.fresh);
        let mut buf = self.spare.pop().unwrap_or_default();
        buf.clear();
        buf.extend_from_slice(data);
        self.jobs
            .as_ref()
            .ok_or_else(|| anyhow!("decode thread closed"))?
            .send(DecodeJob {
                data: buf,
                transport,
                data_type,
                fresh,
                pts,
            })
            .map_err(|_| anyhow!("decode thread stopped"))?;
        self.in_flight += 1;
        Ok(())
    }

    fn wait(&mut self) -> Result<DecodeDone> {
        let done = self
            .done
            .recv()
            .map_err(|_| anyhow!("decode thread stopped"))?;
        Ok(self.returned(done))
    }

    fn poll(&mut self) -> Result<Option<DecodeDone>> {
        match self.done.try_recv() {
            Ok(done) => Ok(Some(self.returned(done))),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(mpsc::TryRecvError::Disconnected) => bail!("decode thread stopped"),
        }
    }

    /// Count a packet out: its audio towards the average, its buffer to reuse.
    fn returned(&mut self, mut done: DecodeDone) -> DecodeDone {
        self.in_flight -= 1;
        let secs = done.packet.duration_secs();
        self.packet_secs = if self.packet_secs > 0.0 {
            self.packet_secs + (secs - self.packet_secs) / self.depth as f64
        } else {
            secs
        };
        self.spare.push(std::mem::take(&mut done.data));
        done
    }
}

impl Drop for DecodeWorker {
    fn drop(&mut self) {
        drop(self.jobs.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
