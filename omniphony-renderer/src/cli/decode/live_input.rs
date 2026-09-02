use super::decoder_thread::DecoderMessage;
#[cfg(target_os = "linux")]
use super::decoder_thread::{DecodedAudioData, DecodedSource};
#[cfg(target_os = "linux")]
use super::output::I32_PCM_FULL_SCALE;
use anyhow::Result;
#[cfg(target_os = "linux")]
use anyhow::anyhow;
#[cfg(target_os = "linux")]
use audio_input::bridge::{BridgeDecodeDiag, LiveBridgeIngestRuntime, spawn_bridge_decode_worker};
#[cfg(target_os = "linux")]
use audio_input::pipewire::{
    PipewireBridgeBackendKind, PipewireBridgeStreamConfig, run_pipewire_bridge_input_stream,
};
#[cfg(target_os = "linux")]
use audio_input::{InputBackend, InputSampleFormat, RequestedAudioInputConfig};
use audio_input::{InputClockMode, InputControl, InputMode};
use audio_output::AudioControl;
#[cfg(target_os = "linux")]
use audio_output::pipewire::PipewireBufferConfig;
#[cfg(target_os = "linux")]
use bridge_api::{FormatBridgeBox, RChannelLabel, RDecodedFrame};
#[cfg(target_os = "linux")]
use orender_engine::bridge_loader::install_bridge_host_log_sink;
#[cfg(target_os = "linux")]
use pipewire as pw;
#[cfg(target_os = "linux")]
use pw::spa;
#[cfg(target_os = "linux")]
use pw::spa::pod::Pod;
#[cfg(target_os = "linux")]
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::time::Instant;

const DEFAULT_LIVE_INPUT_CHANNELS: u16 = 8;
const DEFAULT_LIVE_INPUT_SAMPLE_RATE_HZ: u32 = 48_000;
const DEFAULT_LIVE_BRIDGE_CHANNELS: u16 = 2;
const DEFAULT_LIVE_BRIDGE_SAMPLE_RATE_HZ: u32 = 192_000;
const DEFAULT_LIVE_INPUT_NODE: &str = "omniphony_input_7_1";
const DEFAULT_LIVE_INPUT_DESCRIPTION: &str = "Omniphony Input 7.1";
const DEFAULT_LIVE_BRIDGE_NODE: &str = "omniphony";
const DEFAULT_LIVE_BRIDGE_DESCRIPTION: &str = "Omniphony Bridge Input";
#[cfg(target_os = "linux")]
const LIVE_BRIDGE_LOG_INTERVAL: Duration = Duration::from_secs(1);
// Manager and requested/applied capture configuration.

#[derive(Debug, Clone, PartialEq, Eq)]
struct PipewirePcmInputConfig {
    node_name: String,
    node_description: String,
    channels: u16,
    sample_rate_hz: u32,
    target_latency_ms: u32,
}

#[derive(Clone)]
pub struct LiveBridgeRuntimeConfig {
    pub lib: bridge_api::BridgeLibRef,
    pub presentation: String,
    pub clock_mode: InputClockMode,
    pub requested_drc_mode: Arc<std::sync::RwLock<String>>,
}

#[derive(Clone)]
struct PipewireBridgeInputConfig {
    node_name: String,
    node_description: String,
    channels: u16,
    sample_rate_hz: u32,
    target_latency_ms: u32,
    clock_mode: InputClockMode,
    runtime: LiveBridgeRuntimeConfig,
}

#[derive(Clone)]
enum ActiveCaptureConfig {
    Pcm(PipewirePcmInputConfig),
    Bridge(PipewireBridgeInputConfig),
}

impl ActiveCaptureConfig {
    fn target_latency_ms(&self) -> u32 {
        match self {
            Self::Pcm(config) => config.target_latency_ms,
            Self::Bridge(config) => config.target_latency_ms,
        }
    }

    fn same_runtime_shape(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Pcm(lhs), Self::Pcm(rhs)) => lhs == rhs,
            (Self::Bridge(lhs), Self::Bridge(rhs)) => {
                lhs.node_name == rhs.node_name
                    && lhs.node_description == rhs.node_description
                    && lhs.channels == rhs.channels
                    && lhs.sample_rate_hz == rhs.sample_rate_hz
                    && lhs.target_latency_ms == rhs.target_latency_ms
                    && lhs.clock_mode == rhs.clock_mode
                    && lhs.runtime.presentation == rhs.runtime.presentation
            }
            _ => false,
        }
    }
}

pub struct LiveInputManagerHandle {
    stop: Arc<AtomicBool>,
    join: thread::JoinHandle<()>,
}

impl LiveInputManagerHandle {
    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.join.join();
    }
}

struct CaptureThreadHandle {
    config: ActiveCaptureConfig,
    stop: Arc<AtomicBool>,
    join: thread::JoinHandle<()>,
}

impl CaptureThreadHandle {
    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.join.join();
    }

    fn request_stop(self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(target_os = "linux")]
fn bridge_stream_config(config: &PipewireBridgeInputConfig) -> PipewireBridgeStreamConfig {
    PipewireBridgeStreamConfig {
        node_name: config.node_name.clone(),
        node_description: config.node_description.clone(),
        channels: config.channels,
        sample_rate_hz: config.sample_rate_hz,
        target_latency_ms: config.target_latency_ms,
        clock_mode: config.clock_mode,
    }
}

pub fn spawn_live_input_manager(
    tx: mpsc::SyncSender<Result<DecoderMessage>>,
    input_control: Arc<InputControl>,
    audio_control: Arc<AudioControl>,
    bridge_runtime: LiveBridgeRuntimeConfig,
) -> LiveInputManagerHandle {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);
    let join = thread::Builder::new()
        .name("live-input-manager".into())
        .spawn(move || {
            let mut current_capture: Option<CaptureThreadHandle> = None;
            let mut bootstrap = true;

            while !stop_for_thread.load(Ordering::Relaxed)
                && !sys::ShutdownHandle::is_requested()
                && !sys::ShutdownHandle::is_restart_from_config_requested()
            {
                let apply_requested = input_control.take_apply_pending();
                if bootstrap || apply_requested {
                    bootstrap = false;
                    reconcile_live_input(
                        &tx,
                        &input_control,
                        &audio_control,
                        &bridge_runtime,
                        &mut current_capture,
                    );
                }

                #[cfg(target_os = "linux")]
                if input_control.requested_snapshot().mode != InputMode::Bridge
                    && current_capture.as_ref().is_some_and(|capture| {
                        requested_live_input_latency_ms(&audio_control)
                            != Some(capture.config.target_latency_ms())
                    })
                {
                    reconcile_live_input(
                        &tx,
                        &input_control,
                        &audio_control,
                        &bridge_runtime,
                        &mut current_capture,
                    );
                }

                let capture_finished = current_capture
                    .as_ref()
                    .map(|capture| capture.join.is_finished())
                    .unwrap_or(false);
                if capture_finished {
                    if let Some(capture) = current_capture.take() {
                        let _ = capture.join.join();
                    }
                    if matches!(
                        input_control.requested_snapshot().mode,
                        InputMode::Live | InputMode::PipewireBridge
                    ) {
                        input_control.set_input_state(
                            InputMode::Bridge,
                            None,
                            None,
                            None,
                            None,
                            None,
                            Some("bridge-decoded".to_string()),
                        );
                        if input_control.applied_snapshot().input_error.is_none() {
                            input_control.set_input_error(Some(
                                "live input capture thread stopped unexpectedly".to_string(),
                            ));
                        }
                    }
                }

                thread::sleep(Duration::from_millis(50));
            }

            if let Some(capture) = current_capture.take() {
                if stop_for_thread.load(Ordering::Relaxed) || sys::ShutdownHandle::is_requested() {
                    capture.request_stop();
                } else {
                    capture.stop();
                }
            }
        })
        .expect("failed to spawn live input manager");

    LiveInputManagerHandle { stop, join }
}

// Runtime reconciliation and capture thread orchestration.

fn reconcile_live_input(
    tx: &mpsc::SyncSender<Result<DecoderMessage>>,
    input_control: &Arc<InputControl>,
    #[allow(unused_variables)] audio_control: &Arc<AudioControl>,
    #[allow(unused_variables)] bridge_runtime: &LiveBridgeRuntimeConfig,
    current_capture: &mut Option<CaptureThreadHandle>,
) {
    let requested = input_control.requested_snapshot();

    if requested.mode == InputMode::Bridge {
        if let Some(capture) = current_capture.take() {
            capture.stop();
        }
        input_control.set_input_state(
            InputMode::Bridge,
            None,
            None,
            None,
            None,
            None,
            Some("bridge-decoded".to_string()),
        );
        input_control.set_input_error(None);
        log::info!("Live input manager applied bridge mode");
        return;
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = tx;
        let _ = current_capture;
        input_control.set_input_state(
            InputMode::Bridge,
            None,
            None,
            None,
            None,
            None,
            Some("bridge-decoded".to_string()),
        );
        input_control.set_input_error(Some(
            "live input is not implemented on this platform".to_string(),
        ));
        log::warn!("Live input requested on unsupported platform");
    }

    #[cfg(target_os = "linux")]
    {
        match resolve_capture_config(&requested, audio_control, bridge_runtime) {
            Ok(config) => {
                let needs_restart = current_capture
                    .as_ref()
                    .map(|capture| !capture.config.same_runtime_shape(&config))
                    .unwrap_or(true);

                // Cleared before the capture thread exists: that thread posts
                // its own diagnostics as soon as it connects (a foreign sink
                // already holding the node name), and a reset issued after
                // the spawn would race it and wipe them.
                input_control.set_input_error(None);

                if needs_restart {
                    if let Some(capture) = current_capture.take() {
                        capture.stop();
                    }
                    match spawn_pipewire_capture(
                        tx.clone(),
                        Arc::clone(input_control),
                        config.clone(),
                    ) {
                        Ok(capture) => {
                            *current_capture = Some(capture);
                        }
                        Err(err) => {
                            input_control.set_input_state(
                                InputMode::Bridge,
                                None,
                                None,
                                None,
                                None,
                                None,
                                Some("bridge-decoded".to_string()),
                            );
                            input_control.set_input_error(Some(err.to_string()));
                            log::error!("Failed to start PipeWire live input: {err}");
                            return;
                        }
                    }
                }

                match &config {
                    ActiveCaptureConfig::Pcm(config) => {
                        input_control.set_input_state(
                            InputMode::Live,
                            Some(InputBackend::Pipewire),
                            Some(config.channels),
                            Some(config.sample_rate_hz),
                            Some(config.node_name.clone()),
                            Some(config.node_description.clone()),
                            Some("pipewire-f32".to_string()),
                        );
                    }
                    ActiveCaptureConfig::Bridge(config) => {
                        input_control.set_input_state(
                            InputMode::PipewireBridge,
                            Some(InputBackend::Pipewire),
                            Some(config.channels),
                            Some(config.sample_rate_hz),
                            Some(config.node_name.clone()),
                            Some(config.node_description.clone()),
                            Some("pipewire-iec61937".to_string()),
                        );
                    }
                }
                match &config {
                    ActiveCaptureConfig::Pcm(config) => log::info!(
                        "Live input active: mode=live backend=pipewire node={} channels={} rate={}Hz",
                        config.node_name,
                        config.channels,
                        config.sample_rate_hz
                    ),
                    ActiveCaptureConfig::Bridge(config) => log::info!(
                        "Live input active: mode=pipewire_bridge backend=pipewire node={} channels={} rate={}Hz",
                        config.node_name,
                        config.channels,
                        config.sample_rate_hz
                    ),
                }
            }
            Err(err) => {
                if let Some(capture) = current_capture.take() {
                    capture.stop();
                }
                input_control.set_input_state(
                    InputMode::Bridge,
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some("bridge-decoded".to_string()),
                );
                input_control.set_input_error(Some(err.to_string()));
                log::warn!("Live input request rejected: {err}");
            }
        }
    }
}

// Requested config resolution.

#[cfg(target_os = "linux")]
fn resolve_capture_config(
    requested: &RequestedAudioInputConfig,
    audio_control: &AudioControl,
    bridge_runtime: &LiveBridgeRuntimeConfig,
) -> Result<ActiveCaptureConfig> {
    match requested.mode {
        InputMode::Bridge => anyhow::bail!("bridge mode does not spawn a live PipeWire capture"),
        InputMode::Live => {
            resolve_pipewire_pcm_config(requested, audio_control).map(ActiveCaptureConfig::Pcm)
        }
        InputMode::PipewireBridge => {
            resolve_pipewire_bridge_config(requested, audio_control, bridge_runtime)
                .map(ActiveCaptureConfig::Bridge)
        }
    }
}

#[cfg(target_os = "linux")]
fn resolve_pipewire_pcm_config(
    requested: &RequestedAudioInputConfig,
    audio_control: &AudioControl,
) -> Result<PipewirePcmInputConfig> {
    let backend = requested.backend.unwrap_or(InputBackend::Pipewire);
    if backend != InputBackend::Pipewire {
        anyhow::bail!("only the PipeWire live input backend is implemented on Linux");
    }

    let sample_format = requested.sample_format.unwrap_or(InputSampleFormat::F32);
    if sample_format != InputSampleFormat::F32 {
        anyhow::bail!("PipeWire live input currently supports only f32 interleaved audio");
    }

    let channels = requested.channels.unwrap_or(DEFAULT_LIVE_INPUT_CHANNELS);
    if channels != 8 {
        anyhow::bail!("PipeWire live input currently supports only 8-channel 7.1 mode");
    }

    Ok(PipewirePcmInputConfig {
        node_name: requested
            .node_name
            .clone()
            .unwrap_or_else(|| DEFAULT_LIVE_INPUT_NODE.to_string()),
        node_description: requested
            .node_description
            .clone()
            .unwrap_or_else(|| DEFAULT_LIVE_INPUT_DESCRIPTION.to_string()),
        channels,
        sample_rate_hz: requested
            .sample_rate_hz
            .unwrap_or(DEFAULT_LIVE_INPUT_SAMPLE_RATE_HZ),
        target_latency_ms: requested_live_input_latency_ms(audio_control)
            .unwrap_or(PipewireBufferConfig::default().latency_ms)
            .max(1),
    })
}

#[cfg(target_os = "linux")]
fn resolve_pipewire_bridge_config(
    requested: &RequestedAudioInputConfig,
    audio_control: &AudioControl,
    bridge_runtime: &LiveBridgeRuntimeConfig,
) -> Result<PipewireBridgeInputConfig> {
    let backend = requested.backend.unwrap_or(InputBackend::Pipewire);
    if backend != InputBackend::Pipewire {
        anyhow::bail!("only the PipeWire bridge input backend is implemented on Linux");
    }

    let channels = requested.channels.unwrap_or(DEFAULT_LIVE_BRIDGE_CHANNELS);
    if channels != 2 && channels != 8 {
        anyhow::bail!(
            "PipeWire bridge input supports 2-channel or 8-channel IEC958 mode, got {}",
            channels
        );
    }

    Ok(PipewireBridgeInputConfig {
        node_name: requested
            .node_name
            .clone()
            .unwrap_or_else(|| DEFAULT_LIVE_BRIDGE_NODE.to_string()),
        node_description: requested
            .node_description
            .clone()
            .unwrap_or_else(|| DEFAULT_LIVE_BRIDGE_DESCRIPTION.to_string()),
        channels,
        sample_rate_hz: DEFAULT_LIVE_BRIDGE_SAMPLE_RATE_HZ,
        target_latency_ms: requested_live_input_latency_ms(audio_control)
            .unwrap_or(PipewireBufferConfig::default().latency_ms)
            .max(1),
        clock_mode: requested.clock_mode,
        runtime: bridge_runtime.clone(),
    })
}

#[cfg(target_os = "linux")]
fn requested_live_input_latency_ms(audio_control: &AudioControl) -> Option<u32> {
    audio_control.requested_latency_target_ms()
}

// Live PCM and bridge capture entrypoints.

#[cfg(target_os = "linux")]
fn spawn_pipewire_capture(
    tx: mpsc::SyncSender<Result<DecoderMessage>>,
    input_control: Arc<InputControl>,
    config: ActiveCaptureConfig,
) -> Result<CaptureThreadHandle> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);
    let thread_name = match &config {
        ActiveCaptureConfig::Pcm(config) => format!("pw-live-input-{}", config.node_name),
        ActiveCaptureConfig::Bridge(config) => format!("pw-live-bridge-{}", config.node_name),
    };
    let config_for_thread = config.clone();
    let join = thread::Builder::new().name(thread_name).spawn(move || {
        let result = match config_for_thread.clone() {
            ActiveCaptureConfig::Pcm(config) => {
                run_pipewire_pcm_capture_loop(tx, input_control, config, stop_for_thread)
            }
            ActiveCaptureConfig::Bridge(config) => {
                run_pipewire_bridge_capture_loop(tx, input_control, config, stop_for_thread)
            }
        };
        if let Err(err) = result {
            log::error!("PipeWire live input thread exited with error: {err}");
        }
    })?;

    Ok(CaptureThreadHandle { config, stop, join })
}

#[cfg(target_os = "linux")]
fn run_pipewire_pcm_capture_loop(
    tx: mpsc::SyncSender<Result<DecoderMessage>>,
    input_control: Arc<InputControl>,
    config: PipewirePcmInputConfig,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| anyhow!("Failed to create PipeWire main loop: {e:?}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|e| anyhow!("Failed to create PipeWire context: {e:?}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| anyhow!("Failed to connect to PipeWire core: {e:?}"))?;

    let mut props = pw::properties::PropertiesBox::new();
    props.insert(*pw::keys::MEDIA_TYPE, "Audio");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Playback");
    props.insert(*pw::keys::MEDIA_ROLE, "Music");
    props.insert("media.class", "Audio/Sink");
    props.insert("node.virtual", "true");
    props.insert("node.name", config.node_name.clone());
    props.insert("node.description", config.node_description.clone());
    props.insert("media.name", config.node_description.clone());
    props.insert("audio.channels", config.channels.to_string());
    props.insert("audio.position", "FL,FR,FC,LFE,SL,SR,RL,RR");
    let requested_latency_frames =
        ((config.target_latency_ms as u64 * config.sample_rate_hz as u64) / 1000).max(1) as u32;
    let requested_latency = format!("{}/{}", requested_latency_frames, config.sample_rate_hz);
    props.insert("node.latency", requested_latency.as_str());

    let stream = pw::stream::StreamBox::new(&core, "omniphony-live-input", props)
        .map_err(|e| anyhow!("Failed to create PipeWire input stream: {e:?}"))?;
    log::info!(
        "Publishing PipeWire live input sink: node={} description={} channels={} rate={}Hz latency={}",
        config.node_name,
        config.node_description,
        config.channels,
        config.sample_rate_hz,
        requested_latency
    );

    struct CaptureUserData {
        rate_hz: u32,
        channels: u32,
    }

    let tx_for_process = tx.clone();
    let stop_for_process = Arc::clone(&stop);
    let input_control_for_state = Arc::clone(&input_control);
    let config_for_state = config.clone();
    let _listener = stream
        .add_local_listener_with_user_data(CaptureUserData {
            rate_hz: config.sample_rate_hz,
            channels: config.channels as u32,
        })
        .state_changed(move |_, _, old, new| {
            log::info!("PipeWire live input state changed: {:?} -> {:?}", old, new);
            if matches!(new, pw::stream::StreamState::Error(_)) {
                input_control_for_state.set_input_error(Some(format!(
                    "PipeWire live input stream entered error state on {}",
                    config_for_state.node_name
                )));
            }
        })
        .param_changed(move |_, user_data, id, param| {
            let Some(param) = param else {
                return;
            };
            if id != pw::spa::param::ParamType::Format.as_raw() {
                return;
            }
            let (media_type, media_subtype) =
                match pw::spa::param::format_utils::parse_format(param) {
                    Ok(v) => v,
                    Err(_) => return,
                };
            if media_type != pw::spa::param::format::MediaType::Audio
                || media_subtype != pw::spa::param::format::MediaSubtype::Raw
            {
                return;
            }

            let mut format = pw::spa::param::audio::AudioInfoRaw::new();
            if format.parse(param).is_ok() {
                if format.rate() != 0 {
                    user_data.rate_hz = format.rate();
                }
                if format.channels() != 0 {
                    user_data.channels = format.channels();
                }
            }
        })
        .process(move |stream, user_data| {
            if stop_for_process.load(Ordering::Relaxed) {
                return;
            }
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let datas = buffer.datas_mut();
            if datas.is_empty() {
                return;
            }
            let data = &mut datas[0];
            let sample_len = (data.chunk().size() as usize) / std::mem::size_of::<f32>();
            let Some(bytes) = data.data() else {
                return;
            };
            if sample_len == 0 || user_data.channels == 0 {
                return;
            }
            let sample_bytes = &bytes[..sample_len * std::mem::size_of::<f32>()];
            let frame = build_live_input_frame(
                sample_bytes,
                user_data.rate_hz,
                user_data.channels as usize,
            );

            let _ = tx_for_process.try_send(Ok(DecoderMessage::AudioData(DecodedAudioData {
                source: DecodedSource::Live,
                frame,
                decode_time_ms: 0.0,
                sent_at: Instant::now(),
            })));
        })
        .register()
        .map_err(|e| anyhow!("Failed to register PipeWire live input listeners: {e:?}"))?;

    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(config.sample_rate_hz);
    audio_info.set_channels(config.channels as u32);
    let obj = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: audio_info.into(),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire live input format pod: {e:?}"))?
    .0
    .into_inner();
    let pod = Pod::from_bytes(&values).ok_or_else(|| anyhow!("Invalid PipeWire format pod"))?;
    let mut params = [pod];

    stream
        .connect(
            spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|e| anyhow!("Failed to connect PipeWire live input stream: {e:?}"))?;
    log::info!(
        "PipeWire live input sink connected: node={}",
        config.node_name
    );

    while !stop.load(Ordering::Relaxed)
        && !sys::ShutdownHandle::is_requested()
        && !sys::ShutdownHandle::is_restart_from_config_requested()
    {
        let _ = mainloop.loop_().iterate(Duration::from_millis(100));
    }

    let _ = stream.disconnect();
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_pipewire_bridge_capture_loop(
    tx: mpsc::SyncSender<Result<DecoderMessage>>,
    input_control: Arc<InputControl>,
    config: PipewireBridgeInputConfig,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let (raw_tx, raw_rx) = mpsc::sync_channel::<(u8, Vec<u8>)>(256);
    let bridge = instantiate_live_bridge(&config.runtime)?;
    let tx_for_frame = tx.clone();
    // DIAG iec958-chain: capture bridge plugin output cadence. Registry-handed
    // diag metrics — updated each time the harletty plugin emits a decoded
    // PCM frame, so the Studio plot can see whether the plugin batches
    // frames at ~1 s intervals.
    let diag = input_control.diag_registry();
    let bridge_frame_samples_out =
        diag.register("bridge_frame_samples", "Frame samples", "bridge", "samples");
    let bridge_frame_dt_us_out = diag.register("bridge_frame_dt_us", "Frame dt", "bridge", "us");
    let bridge_frame_count_out = diag.register(
        "bridge_frame_count",
        "Frames emitted (counter)",
        "bridge",
        "",
    );
    let bridge_frames_per_push_packet_out = diag.register(
        "bridge_frames_per_push_packet",
        "Frames per push_packet",
        "bridge",
        "",
    );
    let bridge_push_packet_dt_us_out =
        diag.register("bridge_push_packet_dt_us", "push_packet dt", "bridge", "us");
    let mut last_bridge_frame_at: Option<Instant> = None;
    let mut bridge_frame_count: u64 = 0;
    spawn_bridge_decode_worker(
        bridge,
        raw_rx,
        Some(config.runtime.requested_drc_mode.clone()),
        Some(BridgeDecodeDiag {
            frames_per_push_packet: bridge_frames_per_push_packet_out,
            push_packet_dt_us: bridge_push_packet_dt_us_out,
        }),
        move |frame, decode_time_ms| {
            let now = Instant::now();
            let dt_us = last_bridge_frame_at
                .map(|prev| now.saturating_duration_since(prev).as_micros() as u64)
                .unwrap_or(0);
            last_bridge_frame_at = Some(now);
            bridge_frame_count = bridge_frame_count.saturating_add(1);
            bridge_frame_samples_out.store(
                (frame.sample_count as f64).to_bits(),
                std::sync::atomic::Ordering::Relaxed,
            );
            // Filter out sub-ms back-to-back updates: the harletty plugin
            // emits multiple frames per push_packet result, and those are
            // dispatched in a tight loop with microsecond spacing — meaningless
            // as a cadence metric. We only publish the dt when it's > 1 ms,
            // which captures the actual inter-batch intervals.
            if dt_us >= 1000 {
                bridge_frame_dt_us_out.store(
                    (dt_us as f64).to_bits(),
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
            bridge_frame_count_out.store(
                (bridge_frame_count as f64).to_bits(),
                std::sync::atomic::Ordering::Relaxed,
            );
            let _ = tx_for_frame.try_send(Ok(DecoderMessage::AudioData(DecodedAudioData {
                source: DecodedSource::Bridge,
                frame,
                decode_time_ms,
                sent_at: Instant::now(),
            })));
        },
    )?;
    let ingest = LiveBridgeIngestRuntime::new(raw_tx);

    let backend = selected_pipewire_bridge_backend(config.clock_mode);
    log::info!(
        "PipeWire bridge backend selection: node={} clock_mode={:?} backend={:?}",
        config.node_name,
        config.clock_mode,
        backend
    );

    match backend {
        PipewireBridgeBackendKind::PwClientNode => {
            audio_input::pipewire_client_node::run_pipewire_bridge_client_node_backend(
                input_control,
                bridge_stream_config(&config),
                stop,
                ingest,
            )
        }
        PipewireBridgeBackendKind::PwStream => {
            run_pipewire_bridge_pw_stream_backend(tx, input_control, config, stop, ingest)
        }
    }
}

#[cfg(target_os = "linux")]
fn selected_pipewire_bridge_backend(clock_mode: InputClockMode) -> PipewireBridgeBackendKind {
    match clock_mode {
        InputClockMode::Upstream => PipewireBridgeBackendKind::PwClientNode,
        InputClockMode::Dac | InputClockMode::Pipewire => PipewireBridgeBackendKind::PwStream,
    }
}

#[cfg(target_os = "linux")]
fn run_pipewire_bridge_pw_stream_backend(
    tx: mpsc::SyncSender<Result<DecoderMessage>>,
    input_control: Arc<InputControl>,
    config: PipewireBridgeInputConfig,
    stop: Arc<AtomicBool>,
    ingest: LiveBridgeIngestRuntime,
) -> Result<()> {
    let stream_config = PipewireBridgeStreamConfig {
        node_name: config.node_name,
        node_description: config.node_description,
        channels: config.channels,
        sample_rate_hz: config.sample_rate_hz,
        target_latency_ms: config.target_latency_ms,
        clock_mode: config.clock_mode,
    };
    let ingest = RefCell::new(ingest);
    run_pipewire_bridge_input_stream(
        input_control,
        stream_config,
        stop,
        move |chunk| ingest.borrow_mut().process_chunk(chunk),
        move |bytes, channels, sample_rate_hz| {
            // The fixed input map is a 7.1 layout, and the PCM format we
            // advertise is the only way a client reaches this path, so anything
            // else means the negotiation produced a shape we cannot label.
            // Compared against the constant rather than the label vector: this
            // runs on every buffer, and building that vector to read its length
            // would allocate once per graph cycle.
            if channels != DEFAULT_LIVE_INPUT_CHANNELS as u32 {
                log::warn!(
                    "Dropping live PCM frame: negotiated {channels} channels, fixed input map expects {DEFAULT_LIVE_INPUT_CHANNELS}"
                );
                return;
            }
            let frame = build_live_input_frame(bytes, sample_rate_hz, channels as usize);
            let _ = tx.try_send(Ok(DecoderMessage::AudioData(DecodedAudioData {
                source: DecodedSource::Live,
                frame,
                decode_time_ms: 0.0,
                sent_at: Instant::now(),
            })));
        },
    )
}

// Bridge decode/runtime helpers.

#[cfg(target_os = "linux")]
fn instantiate_live_bridge(runtime: &LiveBridgeRuntimeConfig) -> Result<FormatBridgeBox> {
    install_bridge_host_log_sink(&runtime.lib);
    let new_bridge = runtime.lib.new_bridge();
    // strict mode removed: bridges ignore it; the host always requests non-strict.
    let mut bridge = new_bridge(false);
    if !bridge.configure("presentation".into(), runtime.presentation.as_str().into()) {
        anyhow::bail!(
            "Bridge rejected presentation value '{}'",
            runtime.presentation
        );
    }
    Ok(bridge)
}

#[cfg(target_os = "linux")]
fn build_live_input_frame(
    bytes: &[u8],
    sample_rate_hz: u32,
    channel_count: usize,
) -> RDecodedFrame {
    let sample_count = bytes.len() / std::mem::size_of::<f32>();
    let frame_count = sample_count / channel_count.max(1);
    let mut pcm = Vec::with_capacity(frame_count * channel_count);
    for chunk in bytes.chunks_exact(4) {
        let sample = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        // Unity is 2^23, the scale every decoded frame arrives in and the one
        // `AudioSamples::to_f32` divides by. Scaling to `i32::MAX` instead put
        // live PCM 256x above everything else in the renderer, which clipped
        // into noise rather than playing loud.
        let scaled = (sample.clamp(-1.0, 1.0) * I32_PCM_FULL_SCALE as f32).round() as i32;
        pcm.push(scaled);
    }

    RDecodedFrame {
        sampling_frequency: sample_rate_hz,
        sample_count: frame_count as u32,
        channel_count: channel_count as u32,
        pcm: pcm.into(),
        channel_labels: seven_one_channel_labels().into(),
        metadata: Vec::new().into(),
        drc_gain: 1.0,
        drc_ramp_duration: 0,
        dialogue_level: abi_stable::std_types::ROption::RNone,
        is_new_segment: false,
    }
}

#[cfg(target_os = "linux")]
fn seven_one_channel_labels() -> Vec<RChannelLabel> {
    vec![
        RChannelLabel::L,
        RChannelLabel::R,
        RChannelLabel::C,
        RChannelLabel::LFE,
        RChannelLabel::Ls,
        RChannelLabel::Rs,
        RChannelLabel::Lb,
        RChannelLabel::Rb,
    ]
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    fn interleaved_f32(samples: &[f32]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    /// Live PCM has to arrive in the same integer domain as every decoded
    /// frame — unity at 2^23, which is what both `AudioSamples::to_f32` and the
    /// renderer's `fill_pcm_f32_drc` divide by. Scaling to `i32::MAX` instead
    /// puts it 256x above the rest of the pipeline.
    #[test]
    fn live_pcm_uses_the_decoder_full_scale() {
        let bytes = interleaved_f32(&[1.0, -1.0, 0.5, 0.0, 0.25, -0.25, 0.0, 0.0]);
        let frame = build_live_input_frame(&bytes, 48_000, 8);

        assert_eq!(frame.pcm[0], I32_PCM_FULL_SCALE);
        assert_eq!(frame.pcm[1], -I32_PCM_FULL_SCALE);
        assert_eq!(frame.pcm[2], I32_PCM_FULL_SCALE / 2);
        assert_eq!(frame.pcm[4], I32_PCM_FULL_SCALE / 4);
        assert_eq!(frame.sample_count, 1);
        assert_eq!(frame.channel_count, 8);
        assert_eq!(frame.sampling_frequency, 48_000);
    }

    /// Samples beyond full scale must clamp, not wrap into the opposite sign.
    #[test]
    fn live_pcm_clamps_out_of_range_samples() {
        let bytes = interleaved_f32(&[4.0, -4.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let frame = build_live_input_frame(&bytes, 48_000, 8);

        assert_eq!(frame.pcm[0], I32_PCM_FULL_SCALE);
        assert_eq!(frame.pcm[1], -I32_PCM_FULL_SCALE);
    }
}
