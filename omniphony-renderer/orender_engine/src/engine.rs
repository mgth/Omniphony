//! The headless decode→render session.
//!
//! [`Engine`] owns a loaded decoder bridge plugin and a [`SpatialRenderer`], and
//! turns raw compressed packets into VBAP-rendered interleaved multichannel PCM.
//! It performs no audio I/O: the host (the `orender` CLI, or `liborender.so`
//! inside mpv) feeds packets in and consumes rendered samples.

use crate::bridge_loader::{LoadedBridge, find_bridge_next_to_exe};
use crate::events::Configuration;
use crate::osc::{ObjectMeta, OscSender};
use crate::overlay;
use crate::renderer_build::{SpatialRendererParams, build_spatial_renderer};
use crate::{render, spatial, virtual_bed};
use anyhow::{Result, anyhow, bail};
use bridge_api::{RChannelLabel, RCoordinateFormat, RDecodedFrame, RInputTransport};
use renderer::config::Config;
use renderer::metering::AudioMeter;
use renderer::speaker_layout::SpeakerLayout;
use renderer::spatial_renderer::{SpatialChannelEvent, SpatialRenderer};
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
    bed_indices: Option<Vec<usize>>,
    has_objects: bool,
    loudness_applied: bool,
    decoded_samples: u64,

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
}

impl Engine {
    /// Build a session around an already-loaded bridge and a constructed
    /// renderer. The bridge must already be configured (presentation, DRC mode)
    /// before the first [`process`](Self::process) call.
    pub fn new(bridge: LoadedBridge, renderer: SpatialRenderer, sample_rate: u32) -> Self {
        let coordinate_format = bridge.bridge.coordinate_format();
        Self {
            bridge,
            renderer,
            sample_rate,
            coordinate_format,
            bed_indices: None,
            has_objects: false,
            loudness_applied: false,
            decoded_samples: 0,
            drc_gain: 1.0,
            drc_target_gain: 1.0,
            drc_ramp_samples_remaining: 0,
            applied_drc_mode: String::new(),
            frame_events: Vec::new(),
            pcm_f32_buf: Vec::new(),
            osc: None,
            audio_meter: None,
            object_names: HashMap::new(),
        }
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

        let target = SocketAddrV4::from_str(&format!("{}:{}", opts.host, opts.port_out))
            .map_err(|e| anyhow!("invalid OSC target {}:{}: {e}", opts.host, opts.port_out))?;
        let mut sender = OscSender::new(target)?;
        sender.attach_renderer_control(self.renderer.renderer_control());
        sender.start_listener(opts.port_in)?;
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
        let render_cfg = config_yaml_path
            .map(Config::load_or_default)
            .and_then(|c| c.render);

        let layout = if let Some(p) = speaker_layout_path {
            SpeakerLayout::from_file(p)?
        } else if let Some(l) = render_cfg.as_ref().and_then(|c| c.current_layout.clone()) {
            l
        } else {
            SpeakerLayout::preset("7.1.4")?
        };

        // Resolve the bridge path:
        //  1. explicit override (CLI / FFI param) — strict, must exist or error
        //  2. config render.bridge_path — used when it points at a real file
        //  3. exe-relative fallback — *_bridge.{so,dll,dylib} next to the host
        //     binary (covers Windows-bundle installs without a config)
        let config_bridge = render_cfg.as_ref().and_then(|c| c.bridge_path.clone());
        let resolved_bridge = if let Some(explicit) = bridge_path {
            if !explicit.is_file() {
                bail!(
                    "Bridge path '{}' does not exist or is not a file",
                    explicit.display()
                );
            }
            explicit.to_path_buf()
        } else if let Some(p) = config_bridge.as_ref().filter(|p| p.is_file()) {
            p.clone()
        } else {
            find_bridge_next_to_exe().map_err(|fallback_err| match &config_bridge {
                Some(stale) => anyhow!(
                    "render.bridge_path '{}' does not exist, and exe-relative \
                     fallback failed: {fallback_err}",
                    stale.display()
                ),
                None => anyhow!(
                    "no decoder bridge path: pass one explicitly, set \
                     render.bridge_path in the config YAML, or drop a \
                     *_bridge.{{so,dll,dylib}} next to the host binary.\n\
                     Fallback details: {fallback_err}"
                ),
            })?
        };

        // The renderer's table mode/defaults come from the bridge, so load and
        // configure it before building the renderer.
        let t_bridge = std::time::Instant::now();
        let mut bridge = LoadedBridge::load_with_params(&resolved_bridge, false)?;
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
        control.set_meter_rate_hz(render_cfg.as_ref().and_then(|c| c.meter_rate).unwrap_or(10.0));
        control.set_diag_rate_hz(render_cfg.as_ref().and_then(|c| c.diag_rate).unwrap_or(10.0));

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
            let mut live = control.live.write().unwrap();
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

    /// Number of output channels the renderer produces (speaker count).
    pub fn channel_count(&self) -> u32 {
        self.renderer.num_speakers() as u32
    }

    /// Per-channel labels of the active output layout, one entry per speaker in
    /// render (output channel) order. The host (mpv) turns this into a channel
    /// map. Speakers whose layout name is unrecognised map to
    /// [`RChannelLabel::Unknown`].
    pub fn channel_layout(&self) -> Vec<RChannelLabel> {
        self.renderer
            .speaker_layout()
            .speakers
            .iter()
            .map(|s| crate::channel_layout::label_for_speaker_name(&s.name))
            .collect()
    }

    /// Whether the current presentation may carry spatial objects. Valid after
    /// the bridge has been configured; drives the host's spatial-vs-plain
    /// fallback decision.
    pub fn is_spatial(&self) -> bool {
        self.bridge.bridge.is_spatial()
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
        self.decoded_samples = 0;
        self.drc_gain = 1.0;
        self.drc_target_gain = 1.0;
        self.drc_ramp_samples_remaining = 0;
        // Drop overlay scene + motion trails so they don't bridge the seek.
        overlay::clear();
    }

    fn reset_segment_state(&mut self) {
        self.has_objects = false;
        self.bed_indices = None;
        self.frame_events.clear();
        self.loudness_applied = false;
        self.object_names.clear();
    }

    /// Push the live DRC mode to the bridge when it changes (selects which DRC
    /// words the decoder extracts). Cheap no-op when unchanged. Mirrors the
    /// CLI's `DecoderCommand::SetDrcMode` handling. The bridge preserves the
    /// mode across `reset`, so a seek keeps the current DRC setting.
    fn sync_drc_mode(&mut self) {
        let live_mode = {
            let control = self.renderer.renderer_control();
            let live = control.live.read().unwrap();
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

        let result = self
            .bridge
            .bridge
            .push_packet(data.into(), transport, data_type);

        if !result.error_message.is_empty() {
            bail!("bridge decode error: {}", result.error_message);
        }
        if result.did_reset {
            // Sync-loss recovery inside the bridge: drop stale spatial state but
            // keep live params and the absolute sample clock.
            self.renderer.reset_runtime_state();
            self.reset_segment_state();
        }

        let mut out = Vec::with_capacity(result.frames.len());
        for frame in result.frames.iter() {
            if let Some(chunk) = self.render_frame(frame)? {
                out.push(chunk);
            }
        }
        Ok(out)
    }

    /// Convenience wrapper for hosts that always feed raw access units.
    pub fn process_raw(&mut self, data: &[u8]) -> Result<Vec<RenderedAudio>> {
        self.process(data, RInputTransport::Raw, 0)
    }

    fn render_frame(&mut self, frame: &RDecodedFrame) -> Result<Option<RenderedAudio>> {
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

        // Dialogue normalisation (from major-sync frames), applied once.
        if !self.loudness_applied {
            if let Some(dialogue_level) = frame.dialogue_level.into_option() {
                self.renderer.set_loudness(dialogue_level);
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
            if !meta.bed_indices.is_empty() {
                let new_bed: Vec<usize> = meta.bed_indices.iter().copied().collect();
                if self.bed_indices.as_ref() != Some(&new_bed) {
                    self.bed_indices = Some(new_bed);
                    self.renderer
                        .configure_beds(self.bed_indices.as_deref().unwrap_or(&[]));
                }
            }
            let conf = Configuration::from(meta);
            let bed = self.bed_indices.as_deref().unwrap_or(&[]);
            spatial::build_spatial_channel_events(
                &conf,
                self.coordinate_format,
                bed,
                &mut self.frame_events,
            );

            // Outgoing: broadcast object positions/names to OSC clients and/or
            // feed the in-process mpv overlay.
            if want_objects {
                for upd in meta.name_updates.iter() {
                    self.object_names.insert(upd.id, upd.name.to_string());
                }
                let layout = self.renderer.speaker_layout();
                let objects = spatial::build_object_metas(
                    &conf,
                    self.coordinate_format,
                    Some(&layout),
                    &self.object_names,
                );
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

        self.decoded_samples += sample_count as u64;

        // Bed-only / pre-metadata frames carry no OAMD objects: fall back to the
        // virtual-bed path so each input channel renders through VBAP at its
        // speaker pose (matches the CLI's file-decode behaviour). The embedded
        // host has no live input device, so there is no input layout to bias the
        // poses (`None`), exactly as in the CLI's file-decode path.
        if !self.has_objects {
            let labels: Vec<RChannelLabel> = frame.channel_labels.iter().copied().collect();
            let (room_ratio, room_ratio_rear, room_ratio_lower, room_ratio_center_blend) = {
                let control = self.renderer.renderer_control();
                let live = control.live.read().unwrap();
                (
                    live.room_ratio,
                    live.room_ratio_rear,
                    live.room_ratio_lower,
                    live.room_ratio_center_blend,
                )
            };

            match virtual_bed::build_virtual_bed_events(
                &labels,
                None,
                room_ratio,
                room_ratio_rear,
                room_ratio_lower,
                room_ratio_center_blend,
            ) {
                Some(events) => self.frame_events = events,
                None => {
                    // No virtual-bed VBAP map for these labels → emit silence so
                    // the host still advances by the frame's sample count.
                    self.frame_events.clear();
                    let n_channels = self.renderer.num_speakers() as u32;
                    return Ok(Some(RenderedAudio {
                        samples: vec![0.0; sample_count * n_channels as usize],
                        n_channels,
                        n_frames: sample_count,
                        sample_pos: sample_pos_at_start,
                    }));
                }
            }

            // Outgoing: broadcast the virtual-bed channel poses as OSC objects
            // and/or feed the in-process mpv overlay.
            if want_objects {
                if let Some(objects) = virtual_bed::build_virtual_bed_objects(
                    &labels,
                    None,
                    room_ratio,
                    room_ratio_rear,
                    room_ratio_lower,
                    room_ratio_center_blend,
                ) {
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
            .unwrap()
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
                meter.update_channel_count(channel_count);
                for chunk in pcm_f32.chunks_exact(channel_count) {
                    meter.process_objects(chunk, channel_count);
                }
            }
        }

        let render_started = std::time::Instant::now();
        let rendered = self.renderer.render_frame(
            &pcm_f32,
            channel_count,
            &self.frame_events,
            Vec::new(),
            want_metering,
        )?;
        let render_time_ms = render_started.elapsed().as_secs_f32() * 1000.0;

        // Return scratch buffers for reuse next frame.
        self.pcm_f32_buf = pcm_f32;
        self.frame_events.clear();

        let n_channels = self.renderer.num_speakers() as u32;

        if want_metering {
            let frame_duration_ms = sample_count as f32 / sample_rate as f32 * 1000.0;
            let drc_gain = self.drc_gain;
            if let Some(meter) = self.audio_meter.as_mut() {
                meter.process_speakers(&rendered.samples, n_channels as usize);
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
                            None,
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
