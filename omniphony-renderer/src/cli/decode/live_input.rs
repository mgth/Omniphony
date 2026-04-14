use super::decoder_thread::DecoderMessage;
#[cfg(target_os = "linux")]
use super::decoder_thread::{DecodedAudioData, DecodedSource};
use anyhow::Result;
#[cfg(target_os = "linux")]
use audio_input::bridge::{LiveBridgeIngestRuntime, spawn_bridge_decode_worker};
#[cfg(target_os = "linux")]
use audio_input::pipewire::{PipewireBridgeStreamConfig, run_pipewire_bridge_input_stream};
#[cfg(target_os = "linux")]
use audio_input::pipewire_legacy::{
    BridgeCaptureUserData, PipewireBridgeBackendKind, PwDriverTriggerSchedule,
    run_pipewire_bridge_adapter_backend as run_pipewire_bridge_adapter_backend_legacy,
};
#[cfg(target_os = "linux")]
use audio_input::pipewire_pods::{
    build_pipewire_bridge_buffers_pod, build_pipewire_bridge_format_pod,
};
use audio_input::{InputClockMode, InputControl, InputMode};
use audio_output::AudioControl;
#[cfg(target_os = "linux")]
use audio_output::pipewire::PipewireBufferConfig;
#[cfg(target_os = "linux")]
use anyhow::anyhow;
#[cfg(target_os = "linux")]
use audio_input::{InputBackend, InputSampleFormat, RequestedAudioInputConfig};
#[cfg(target_os = "linux")]
use bridge_api::{FormatBridgeBox, RChannelLabel, RDecodedFrame};
#[cfg(target_os = "linux")]
use pipewire as pw;
#[cfg(target_os = "linux")]
use pw::spa;
#[cfg(target_os = "linux")]
use pw::spa::pod::Pod;
#[cfg(target_os = "linux")]
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;
#[cfg(target_os = "linux")]
use std::cell::RefCell;
#[cfg(target_os = "linux")]
use std::rc::Rc;
#[cfg(target_os = "linux")]
use std::sync::atomic::AtomicI64;
#[cfg(target_os = "linux")]
use std::time::Instant;

const DEFAULT_LIVE_INPUT_CHANNELS: u16 = 8;
const DEFAULT_LIVE_INPUT_SAMPLE_RATE_HZ: u32 = 48_000;
const DEFAULT_LIVE_BRIDGE_SAMPLE_RATE_HZ: u32 = 192_000;
const DEFAULT_LIVE_INPUT_NODE: &str = "omniphony_input_7_1";
const DEFAULT_LIVE_INPUT_DESCRIPTION: &str = "Omniphony Input 7.1";
const DEFAULT_LIVE_BRIDGE_NODE: &str = "omniphony";
const DEFAULT_LIVE_BRIDGE_DESCRIPTION: &str = "Omniphony Bridge Input";
#[cfg(target_os = "linux")]
const LIVE_BRIDGE_LOG_INTERVAL: Duration = Duration::from_secs(1);
/// Number of PwStream process callbacks to accumulate before ingesting.
/// PipeWire graphs typically run at 48kHz while the IEC61937 transport is declared at 192kHz,
/// producing a 4x cadence mismatch. Accumulating 4 callbacks restores a full-rate burst.
#[cfg(target_os = "linux")]
const PW_STREAM_ACCUMULATE_CALLBACKS: usize = 4;

/// Minimum interval between idle trigger_process() calls for the DRIVER stream.
/// At 192kHz/512-sample quantum the cycle period is 2.67ms; we use 2ms to stay slightly
/// under that so we never miss a quantum, but avoid the 250K Hz spinning that overloads
/// PipeWire and causes Streaming→Paused resets every ~16 s.
#[cfg(target_os = "linux")]
const PW_DRIVER_IDLE_TRIGGER_INTERVAL: Duration = Duration::from_millis(2);

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
    pub strict_mode: bool,
    pub presentation: String,
    pub clock_mode: InputClockMode,
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
                    && lhs.runtime.strict_mode == rhs.runtime.strict_mode
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
                input_control.set_input_error(None);
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

    let channels = requested.channels.unwrap_or(DEFAULT_LIVE_INPUT_CHANNELS);
    if channels != 8 {
        anyhow::bail!("PipeWire bridge input currently supports only 8-channel IEC958 mode");
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
        sample_rate_hz: requested
            .sample_rate_hz
            .unwrap_or(DEFAULT_LIVE_BRIDGE_SAMPLE_RATE_HZ),
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
    let tx_for_flush = tx.clone();
    spawn_bridge_decode_worker(
        bridge,
        raw_rx,
        config.runtime.strict_mode,
        move |frame, decode_time_ms| {
            let _ = tx_for_frame.try_send(Ok(DecoderMessage::AudioData(DecodedAudioData {
                source: DecodedSource::Bridge,
                frame,
                decode_time_ms,
                sent_at: Instant::now(),
            })));
        },
        move || {
            let _ = tx_for_flush.try_send(Ok(DecoderMessage::FlushRequest(DecodedSource::Bridge)));
        },
        move |err| {
            let _ = tx.try_send(Err(err));
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
        PipewireBridgeBackendKind::PwAdapter => {
            let stream_config = bridge_stream_config(&config);
            run_pipewire_bridge_adapter_backend_legacy(
                input_control,
                stream_config.clone(),
                stop,
                ingest,
                move |mainloop, core, stop, input_control, _config, ingest, capture_props| {
                    run_pipewire_bridge_capture_stream(
                        mainloop,
                        core,
                        stop,
                        input_control,
                        PipewireBridgeInputConfig {
                            node_name: stream_config.node_name.clone(),
                            node_description: stream_config.node_description.clone(),
                            channels: stream_config.channels,
                            sample_rate_hz: stream_config.sample_rate_hz,
                            target_latency_ms: stream_config.target_latency_ms,
                            clock_mode: stream_config.clock_mode,
                            runtime: config.runtime.clone(),
                        },
                        ingest,
                        None,
                        capture_props,
                        "omniphony-live-bridge-monitor-capture",
                        "PipeWire bridge monitor capture",
                    )
                },
            )
        }
        PipewireBridgeBackendKind::PwClientNode => {
            audio_input::pipewire_client_node::run_pipewire_bridge_client_node_backend(
                input_control,
                bridge_stream_config(&config),
                stop,
                ingest,
            )
        }
        PipewireBridgeBackendKind::PwExportedNode => {
            audio_input::pipewire_exported::run_pipewire_bridge_exported_node_backend(
                input_control,
                bridge_stream_config(&config),
                stop,
                ingest,
            )
        }
        PipewireBridgeBackendKind::PwStream => {
            run_pipewire_bridge_pw_stream_backend(input_control, config, stop, ingest)
        }
        PipewireBridgeBackendKind::PwFilter => {
            audio_input::pipewire_legacy::run_pipewire_bridge_filter_backend(
                input_control,
                bridge_stream_config(&config),
                stop,
                ingest,
            )
        }
    }
}

// PipeWire DRIVER pacing helpers for the bridge capture stream.

#[cfg(target_os = "linux")]
fn selected_pipewire_bridge_backend(clock_mode: InputClockMode) -> PipewireBridgeBackendKind {
    match clock_mode {
        InputClockMode::Upstream => PipewireBridgeBackendKind::PwClientNode,
        InputClockMode::Dac | InputClockMode::Pipewire => PipewireBridgeBackendKind::PwStream,
    }
}

#[cfg(target_os = "linux")]
fn current_pw_driver_trigger_interval(user_data: &BridgeCaptureUserData) -> Duration {
    user_data
        .dynamic_trigger_interval
        .unwrap_or(PW_DRIVER_IDLE_TRIGGER_INTERVAL)
}

#[cfg(target_os = "linux")]
fn current_direct_pw_driver_trigger_interval(input_control: &InputControl) -> Duration {
    let rate_hz = input_control.input_trigger_rate_hz().max(1) as u128;
    let quantum_frames = input_control.input_trigger_quantum_frames().max(1) as u128;
    let nanos = ((quantum_frames * 1_000_000_000u128) / rate_hz).max(500_000);
    Duration::from_nanos(nanos.min(u64::MAX as u128) as u64)
}

#[cfg(target_os = "linux")]
fn schedule_pw_stream_driver_trigger(
    schedule: &Rc<RefCell<PwDriverTriggerSchedule>>,
    delay: Duration,
    reason: &'static str,
) {
    let requested_at = Instant::now() + delay;
    let mut schedule = schedule.borrow_mut();
    match schedule.next_trigger_at {
        Some(current_at) if current_at <= requested_at => {}
        _ => {
            schedule.next_trigger_at = Some(requested_at);
            schedule.pending_reason = Some(reason);
        }
    }
}

#[cfg(target_os = "linux")]
fn next_pw_stream_driver_timeout(schedule: &Rc<RefCell<PwDriverTriggerSchedule>>) -> Duration {
    let schedule = schedule.borrow();
    match schedule.next_trigger_at {
        Some(deadline) => deadline
            .checked_duration_since(Instant::now())
            .unwrap_or(Duration::ZERO)
            .min(Duration::from_millis(100)),
        None => Duration::from_millis(100),
    }
}

#[cfg(target_os = "linux")]
fn next_direct_pw_stream_driver_timeout(
    pending: Option<&Arc<AtomicI64>>,
    next_trigger_at: Option<Instant>,
) -> Duration {
    if pending.is_some_and(|p| p.load(Ordering::Relaxed) > 0) {
        match next_trigger_at {
            Some(deadline) => deadline
                .checked_duration_since(Instant::now())
                .unwrap_or(Duration::ZERO)
                .min(Duration::from_millis(20)),
            None => Duration::ZERO,
        }
    } else {
        Duration::from_millis(20)
    }
}

#[cfg(target_os = "linux")]
fn drain_direct_pw_stream_driver_trigger(
    stream: &pw::stream::Stream,
    pending: Option<&Arc<AtomicI64>>,
    next_trigger_at: &mut Option<Instant>,
    trigger_interval: Duration,
    log_prefix: &'static str,
) {
    if stream.state() != pw::stream::StreamState::Streaming {
        if let Some(pending) = pending {
            pending.store(0, Ordering::Release);
        }
        *next_trigger_at = None;
        return;
    }

    let Some(pending) = pending else {
        *next_trigger_at = None;
        return;
    };
    if pending.load(Ordering::Relaxed) <= 0 {
        *next_trigger_at = None;
        return;
    }

    let now = Instant::now();
    let deadline = next_trigger_at.get_or_insert(now);
    if *deadline > now {
        return;
    }

    let pending_before = pending.load(Ordering::Relaxed);
    if pending_before <= 0 {
        *next_trigger_at = None;
        return;
    }

    pending.fetch_sub(1, Ordering::AcqRel);
    match stream.trigger_process() {
        Ok(()) => {
            log::trace!(
                "{} direct trigger_process ok: pending_before={} interval_ms={:.3}",
                log_prefix,
                pending_before,
                trigger_interval.as_secs_f64() * 1000.0
            );
        }
        Err(err) => {
            log::warn!(
                "{} direct trigger_process failed: pending_before={} error={:?}",
                log_prefix,
                pending_before,
                err
            );
        }
    }

    let remaining = pending.load(Ordering::Relaxed);
    if remaining > 0 {
        *next_trigger_at = Some((*deadline + trigger_interval).max(now + Duration::from_millis(1)));
    } else {
        *next_trigger_at = None;
    }
}

#[cfg(target_os = "linux")]
fn drain_scheduled_pw_stream_trigger(
    stream: &pw::stream::Stream,
    schedule: &Rc<RefCell<PwDriverTriggerSchedule>>,
    log_prefix: &'static str,
) {
    if stream.state() != pw::stream::StreamState::Streaming {
        let mut schedule = schedule.borrow_mut();
        schedule.next_trigger_at = None;
        schedule.pending_reason = None;
        return;
    }

    let reason = {
        let mut schedule = schedule.borrow_mut();
        let Some(deadline) = schedule.next_trigger_at else {
            return;
        };
        if deadline > Instant::now() {
            return;
        }
        schedule.next_trigger_at = None;
        schedule.pending_reason.take().unwrap_or("scheduled")
    };

    let mut schedule = schedule.borrow_mut();
    schedule.trigger_calls_since_log += 1;
    match stream.trigger_process() {
        Ok(()) => {
            if schedule.trigger_calls_since_log <= 8 {
                log::trace!(
                    "{} trigger_process ok: reason={} trigger_calls={} trigger_errors={}",
                    log_prefix,
                    reason,
                    schedule.trigger_calls_since_log,
                    schedule.trigger_errors_since_log
                );
            }
        }
        Err(err) => {
            schedule.trigger_errors_since_log += 1;
            log::warn!(
                "{} trigger_process failed: reason={} error={:?} trigger_calls={} trigger_errors={}",
                log_prefix,
                reason,
                err,
                schedule.trigger_calls_since_log,
                schedule.trigger_errors_since_log
            );
        }
    }
}

#[cfg(target_os = "linux")]
fn refresh_pw_stream_driver_timing(
    stream: &pw::stream::Stream,
    input_control: &InputControl,
    user_data: &mut BridgeCaptureUserData,
    log_prefix: &'static str,
) {
    let mut time = MaybeUninit::<pw::sys::pw_time>::zeroed();
    let res = unsafe {
        pw::sys::pw_stream_get_time_n(
            stream.as_raw_ptr(),
            time.as_mut_ptr(),
            std::mem::size_of::<pw::sys::pw_time>(),
        )
    };
    if res < 0 {
        return;
    }
    let time = unsafe { time.assume_init() };
    if time.rate.num == 0 || time.rate.denom == 0 || time.size == 0 {
        return;
    }

    // For this interleaved 8-channel IEC61937 stream, pw_time.size is reported in samples,
    // while the callback chunk cadence is driven by transport frames. Convert back to frames
    // before deriving the graph quantum, otherwise we overestimate by `channels`.
    let transport_frames = (time.size / user_data.channels.max(1) as u64).max(1);
    input_control
        .register_direct_trigger_quantum_frames(transport_frames.min(u32::MAX as u64) as u32);
    let quantum_ns = (transport_frames as u128 * time.rate.num as u128 * 1_000_000_000u128)
        / time.rate.denom as u128;
    let quantum_ns = quantum_ns.min(u64::MAX as u128) as u64;
    if quantum_ns == 0 {
        return;
    }

    // Follow the actual graph quantum, corrected by output rate-adjust feedback.
    // rate_adjust < 1.0 → output is being slowed (DRIVER too fast) → stretch interval.
    // rate_adjust > 1.0 → output is being sped up (DRIVER too slow) → shrink interval.
    // Correction is clamped to ±5% to avoid instability.
    let rate_adjust = f32::from_bits(user_data.output_rate_adjust.load(Ordering::Relaxed));
    let correction = if rate_adjust > 0.0 {
        (1.0f64 / rate_adjust as f64).clamp(0.95, 1.05)
    } else {
        1.0
    };
    let scheduled_ns = (quantum_ns as f64 * correction) as u64;
    let scheduled_ns = scheduled_ns.max(500_000);
    let scheduled_ns = scheduled_ns.min(20_000_000);
    user_data.dynamic_trigger_interval = Some(Duration::from_nanos(scheduled_ns));

    let now = Instant::now();
    if now.duration_since(user_data.last_pw_time_log_at) >= LIVE_BRIDGE_LOG_INTERVAL {
        user_data.last_pw_time_log_at = now;
        let quantum_ms = quantum_ns as f64 / 1_000_000.0;
        let scheduled_ms = scheduled_ns as f64 / 1_000_000.0;
        log::debug!(
            "{} pw_time: rate={}/{} size={} transport_frames={} queued={} buffered={} queued_buffers={} avail_buffers={} delay={} quantum_ms={:.3} trigger_ms={:.3} rate_adjust={:.6} correction={:.4}",
            log_prefix,
            time.rate.num,
            time.rate.denom,
            time.size,
            transport_frames,
            time.queued,
            time.buffered,
            time.queued_buffers,
            time.avail_buffers,
            time.delay,
            quantum_ms,
            scheduled_ms,
            rate_adjust,
            correction
        );
    }
}

#[cfg(target_os = "linux")]
fn run_pipewire_bridge_pw_stream_backend(
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
    run_pipewire_bridge_input_stream(input_control, stream_config, stop, move |chunk| {
        ingest.borrow_mut().process_chunk(chunk)
    })
}

#[cfg(target_os = "linux")]
fn run_pipewire_bridge_capture_stream(
    mainloop: &pw::main_loop::MainLoopRc,
    core: &pw::core::CoreRc,
    stop: Arc<AtomicBool>,
    input_control: Arc<InputControl>,
    config: PipewireBridgeInputConfig,
    ingest: LiveBridgeIngestRuntime,
    target_id: Option<u32>,
    props: pw::properties::PropertiesBox,
    stream_name: &str,
    log_prefix: &'static str,
) -> Result<()> {
    let stream = pw::stream::StreamBox::new(core, stream_name, props)
        .map_err(|e| anyhow!("Failed to create PipeWire bridge input stream: {e:?}"))?;

    let stop_for_process = Arc::clone(&stop);
    let input_control_for_state = Arc::clone(&input_control);
    let config_for_state = config.clone();
    let input_control_for_process = Arc::clone(&input_control);
    let trigger_schedule = Rc::new(RefCell::new(PwDriverTriggerSchedule::default()));
    let _trigger_schedule_for_state = Rc::clone(&trigger_schedule);
    let trigger_schedule_for_process = Rc::clone(&trigger_schedule);
    let ingest = RefCell::new(ingest);
    let _listener = stream
        .add_local_listener_with_user_data(BridgeCaptureUserData {
            rate_hz: config.sample_rate_hz,
            channels: config.channels as u32,
            last_log_at: Instant::now(),
            add_buffer_calls_since_log: 0,
            remove_buffer_calls_since_log: 0,
            drained_calls_since_log: 0,
            io_changed_calls_since_log: 0,
            first_process_logged: false,
            first_buffer_layout_logged: false,
            process_calls_since_log: 0,
            datas_empty_since_log: 0,
            data_missing_since_log: 0,
            zero_size_chunks_since_log: 0,
            oversized_chunks_since_log: 0,
            bytes_since_log: 0,
            buffers_since_log: 0,
            sync_buffers_since_log: 0,
            packets_since_log: 0,
            frames_since_log: 0,
            empty_polls_since_log: 0,
            callback_chunk_logs_remaining: 8,
            accumulate_buf: Vec::new(),
            accumulate_count: 0,
            last_idle_trigger: Instant::now(),
            dynamic_trigger_interval: None,
            last_pw_time_log_at: Instant::now(),
            output_rate_adjust: input_control.output_rate_adjust_atomic(),
        })
        .state_changed(move |_stream, _user_data, old, new| {
            log::info!(
                "{} state changed: {:?} -> {:?}",
                log_prefix,
                old,
                new
            );
            if new == pw::stream::StreamState::Streaming {
                log::info!("{} is now STREAMING — triggering initial driver cycle", log_prefix);
                // As DRIVER, nothing calls our process callback until we schedule the first cycle.
                schedule_pw_stream_driver_trigger(
                    &_trigger_schedule_for_state,
                    Duration::ZERO,
                    "state_changed_streaming",
                );
            }
            if matches!(new, pw::stream::StreamState::Error(_)) {
                input_control_for_state.set_input_error(Some(format!(
                    "{} stream entered error state on {}",
                    log_prefix,
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
            if media_type != pw::spa::param::format::MediaType::Audio {
                return;
            }

            if media_subtype == pw::spa::param::format::MediaSubtype::Raw {
                let mut format = pw::spa::param::audio::AudioInfoRaw::new();
                if format.parse(param).is_ok() {
                    if format.rate() != 0 {
                        user_data.rate_hz = format.rate();
                    }
                    if format.channels() != 0 {
                        user_data.channels = format.channels();
                    }
                    log::info!(
                        "{} format negotiated: subtype=raw rate={}Hz channels={} format={:?}",
                        log_prefix,
                        user_data.rate_hz,
                        user_data.channels,
                        format.format()
                    );
                }
            } else {
                log::info!(
                    "{} format negotiated: subtype={:?}",
                    log_prefix,
                    media_subtype
                );
            }
        })
        .io_changed(move |_, user_data, id, area, size| {
            user_data.io_changed_calls_since_log += 1;
                log::debug!(
                    "{} io_changed: id={} area={:p} size={} io_changed_calls={} add_calls={} process_calls={}",
                log_prefix,
                id,
                area,
                size,
                user_data.io_changed_calls_since_log,
                user_data.add_buffer_calls_since_log,
                user_data.process_calls_since_log
            );
        })
        .add_buffer(move |_, user_data, buffer| {
            user_data.add_buffer_calls_since_log += 1;
            log::debug!(
                "{} add_buffer: buffer={:p} add_calls={} remove_calls={} drained_calls={} io_changed_calls={} process_calls={}",
                log_prefix,
                buffer,
                user_data.add_buffer_calls_since_log,
                user_data.remove_buffer_calls_since_log,
                user_data.drained_calls_since_log,
                user_data.io_changed_calls_since_log,
                user_data.process_calls_since_log
            );
        })
        .remove_buffer(move |_, user_data, buffer| {
            user_data.remove_buffer_calls_since_log += 1;
            log::debug!(
                "{} remove_buffer: buffer={:p} add_calls={} remove_calls={} drained_calls={} io_changed_calls={} process_calls={}",
                log_prefix,
                buffer,
                user_data.add_buffer_calls_since_log,
                user_data.remove_buffer_calls_since_log,
                user_data.drained_calls_since_log,
                user_data.io_changed_calls_since_log,
                user_data.process_calls_since_log
            );
        })
        .process(move |stream, user_data| {
            if stop_for_process.load(Ordering::Relaxed) {
                return;
            }
            refresh_pw_stream_driver_timing(
                stream,
                input_control_for_process.as_ref(),
                user_data,
                log_prefix,
            );
            user_data.process_calls_since_log += 1;
            if !user_data.first_process_logged {
                user_data.first_process_logged = true;
                log::info!(
                    "{} first process callback: add_calls={} remove_calls={} drained_calls={} io_changed_calls={} state={:?}",
                    log_prefix,
                    user_data.add_buffer_calls_since_log,
                    user_data.remove_buffer_calls_since_log,
                    user_data.drained_calls_since_log,
                    user_data.io_changed_calls_since_log,
                    stream.state()
                );
            }
            let Some(mut buffer) = stream.dequeue_buffer() else {
                user_data.empty_polls_since_log += 1;
                let now = Instant::now();
                if now.duration_since(user_data.last_log_at) >= LIVE_BRIDGE_LOG_INTERVAL {
                    log::debug!(
                        "{} ingest idle: add_buffers={} remove_buffers={} drained={} io_changed={} process_calls={} empty_polls={} datas_empty={} data_missing={} zero_chunks={} oversized_chunks={} rate={}Hz channels={}",
                        log_prefix,
                        user_data.add_buffer_calls_since_log,
                        user_data.remove_buffer_calls_since_log,
                        user_data.drained_calls_since_log,
                        user_data.io_changed_calls_since_log,
                        user_data.process_calls_since_log,
                        user_data.empty_polls_since_log,
                        user_data.datas_empty_since_log,
                        user_data.data_missing_since_log,
                        user_data.zero_size_chunks_since_log,
                        user_data.oversized_chunks_since_log,
                        user_data.rate_hz,
                        user_data.channels
                    );
                    user_data.last_log_at = now;
                    user_data.add_buffer_calls_since_log = 0;
                    user_data.remove_buffer_calls_since_log = 0;
                    user_data.drained_calls_since_log = 0;
                    user_data.io_changed_calls_since_log = 0;
                    user_data.process_calls_since_log = 0;
                    user_data.datas_empty_since_log = 0;
                    user_data.data_missing_since_log = 0;
                    user_data.zero_size_chunks_since_log = 0;
                    user_data.oversized_chunks_since_log = 0;
                    user_data.empty_polls_since_log = 0;
                }
                // Rate-limit idle triggers to ~one quantum period to avoid
                // overwhelming PipeWire with a tight spin loop.
                if now.duration_since(user_data.last_idle_trigger)
                    >= current_pw_driver_trigger_interval(user_data)
                {
                    user_data.last_idle_trigger = now;
                    schedule_pw_stream_driver_trigger(
                        &trigger_schedule_for_process,
                        current_pw_driver_trigger_interval(user_data),
                        "idle_no_buffer",
                    );
                }
                return;
            };
            let datas = buffer.datas_mut();
            if !user_data.first_buffer_layout_logged {
                user_data.first_buffer_layout_logged = true;
                log::debug!(
                    "{} first buffer layout: datas_len={}",
                    log_prefix,
                    datas.len()
                );
                for (index, data) in datas.iter_mut().enumerate() {
                    let chunk = data.chunk();
                    let raw = data.as_raw();
                    let data_type = data.type_();
                    let maxsize = raw.maxsize;
                    let mapoffset = raw.mapoffset;
                    let chunk_offset = chunk.offset();
                    let chunk_size = chunk.size();
                    let chunk_stride = chunk.stride();
                    let has_data = data.data().is_some();
                    log::debug!(
                        "{} first buffer data[{}]: type={:?} maxsize={} mapoffset={} chunk.offset={} chunk.size={} chunk.stride={} has_data={}",
                        log_prefix,
                        index,
                        data_type,
                        maxsize,
                        mapoffset,
                        chunk_offset,
                        chunk_size,
                        chunk_stride,
                        has_data
                    );
                }
            }
            if datas.is_empty() {
                user_data.datas_empty_since_log += 1;
                let now = Instant::now();
                if now.duration_since(user_data.last_idle_trigger)
                    >= current_pw_driver_trigger_interval(user_data)
                {
                    user_data.last_idle_trigger = now;
                    schedule_pw_stream_driver_trigger(
                        &trigger_schedule_for_process,
                        current_pw_driver_trigger_interval(user_data),
                        "datas_empty",
                    );
                }
                return;
            }
            let data = &mut datas[0];
            let byte_len = data.chunk().size() as usize;
            let Some(bytes) = data.data() else {
                user_data.data_missing_since_log += 1;
                let now = Instant::now();
                if now.duration_since(user_data.last_idle_trigger)
                    >= current_pw_driver_trigger_interval(user_data)
                {
                    user_data.last_idle_trigger = now;
                    schedule_pw_stream_driver_trigger(
                        &trigger_schedule_for_process,
                        current_pw_driver_trigger_interval(user_data),
                        "data_missing",
                    );
                }
                return;
            };
            if byte_len == 0 {
                user_data.zero_size_chunks_since_log += 1;
                let now = Instant::now();
                if now.duration_since(user_data.last_idle_trigger)
                    >= current_pw_driver_trigger_interval(user_data)
                {
                    user_data.last_idle_trigger = now;
                    schedule_pw_stream_driver_trigger(
                        &trigger_schedule_for_process,
                        current_pw_driver_trigger_interval(user_data),
                        "zero_size_chunk",
                    );
                }
                return;
            }
            if byte_len > bytes.len() {
                user_data.oversized_chunks_since_log += 1;
                let now = Instant::now();
                if now.duration_since(user_data.last_idle_trigger)
                    >= current_pw_driver_trigger_interval(user_data)
                {
                    user_data.last_idle_trigger = now;
                    schedule_pw_stream_driver_trigger(
                        &trigger_schedule_for_process,
                        current_pw_driver_trigger_interval(user_data),
                        "oversized_chunk",
                    );
                }
                return;
            }
            if user_data.channels == 0 {
                let now = Instant::now();
                if now.duration_since(user_data.last_idle_trigger)
                    >= current_pw_driver_trigger_interval(user_data)
                {
                    user_data.last_idle_trigger = now;
                    schedule_pw_stream_driver_trigger(
                        &trigger_schedule_for_process,
                        current_pw_driver_trigger_interval(user_data),
                        "zero_channels",
                    );
                }
                return;
            }
            let chunk = &bytes[..byte_len];
            if user_data.callback_chunk_logs_remaining > 0
                && user_data.channels > 0
                && user_data.rate_hz > 0
            {
                user_data.callback_chunk_logs_remaining -= 1;
                let transport_ms = byte_len as f64
                    / (user_data.channels as f64 * std::mem::size_of::<u16>() as f64)
                    / user_data.rate_hz as f64
                    * 1000.0;
                log::debug!(
                    "{} callback chunk: bytes={} transport_ms={:.3} rate={}Hz channels={}",
                    log_prefix,
                    byte_len,
                    transport_ms,
                    user_data.rate_hz,
                    user_data.channels
                );
            }
            let has_spdif_sync = chunk.windows(4).any(|w| {
                u16::from_le_bytes([w[0], w[1]]) == 0xF872
                    && u16::from_le_bytes([w[2], w[3]]) == 0x4E1F
            });
            user_data.bytes_since_log += byte_len;
            user_data.buffers_since_log += 1;
            if has_spdif_sync {
                user_data.sync_buffers_since_log += 1;
            }
            user_data.accumulate_buf.extend_from_slice(chunk);
            user_data.accumulate_count += 1;
            let (packet_count, frame_count) = if user_data.accumulate_count >= PW_STREAM_ACCUMULATE_CALLBACKS {
                let result = ingest.borrow_mut().process_chunk(&user_data.accumulate_buf);
                user_data.accumulate_buf.clear();
                user_data.accumulate_count = 0;
                result
            } else {
                (0, 0)
            };
            user_data.packets_since_log += packet_count;
            user_data.frames_since_log += frame_count;
            let now = Instant::now();
            if now.duration_since(user_data.last_log_at) >= LIVE_BRIDGE_LOG_INTERVAL {
                log::debug!(
                    "{} ingest: add_buffers={} remove_buffers={} drained={} io_changed={} process_calls={} buffers={} bytes={} sync_buffers={} packets={} frames={} empty_polls={} datas_empty={} data_missing={} zero_chunks={} oversized_chunks={} rate={}Hz channels={}",
                    log_prefix,
                    user_data.add_buffer_calls_since_log,
                    user_data.remove_buffer_calls_since_log,
                    user_data.drained_calls_since_log,
                    user_data.io_changed_calls_since_log,
                    user_data.process_calls_since_log,
                    user_data.buffers_since_log,
                    user_data.bytes_since_log,
                    user_data.sync_buffers_since_log,
                    user_data.packets_since_log,
                    user_data.frames_since_log,
                    user_data.empty_polls_since_log,
                    user_data.datas_empty_since_log,
                    user_data.data_missing_since_log,
                    user_data.zero_size_chunks_since_log,
                    user_data.oversized_chunks_since_log,
                    user_data.rate_hz,
                    user_data.channels
                );
                if user_data.buffers_since_log > 0 && user_data.sync_buffers_since_log == 0 {
                    log::debug!("{} ingest has audio buffers but no IEC61937 sync words yet", log_prefix);
                }
                user_data.last_log_at = now;
                user_data.add_buffer_calls_since_log = 0;
                user_data.remove_buffer_calls_since_log = 0;
                user_data.drained_calls_since_log = 0;
                user_data.io_changed_calls_since_log = 0;
                user_data.process_calls_since_log = 0;
                user_data.datas_empty_since_log = 0;
                user_data.data_missing_since_log = 0;
                user_data.zero_size_chunks_since_log = 0;
                user_data.oversized_chunks_since_log = 0;
                user_data.bytes_since_log = 0;
                user_data.buffers_since_log = 0;
                user_data.sync_buffers_since_log = 0;
                user_data.packets_since_log = 0;
                user_data.frames_since_log = 0;
                user_data.empty_polls_since_log = 0;
            }
            // In DRIVER mode, trigger the next cycle on the next quantum boundary instead of
            // retriggering synchronously from inside process(), which only yields two immediate
            // callbacks before the loop stalls.
            schedule_pw_stream_driver_trigger(
                &trigger_schedule_for_process,
                current_pw_driver_trigger_interval(user_data),
                "post_process",
            );
        })
        .drained(move |_, user_data| {
            user_data.drained_calls_since_log += 1;
            log::debug!(
                "{} drained: add_calls={} remove_calls={} drained_calls={} io_changed_calls={} process_calls={} buffers={} bytes={}",
                log_prefix,
                user_data.add_buffer_calls_since_log,
                user_data.remove_buffer_calls_since_log,
                user_data.drained_calls_since_log,
                user_data.io_changed_calls_since_log,
                user_data.process_calls_since_log,
                user_data.buffers_since_log,
                user_data.bytes_since_log
            );
        })
        .register()
        .map_err(|e| anyhow!("Failed to register PipeWire bridge input listeners: {e:?}"))?;

    let format_values = build_pipewire_bridge_format_pod(
        config.sample_rate_hz,
        config.channels,
        spa::param::ParamType::EnumFormat,
    )?;
    let format_pod =
        Pod::from_bytes(&format_values).ok_or_else(|| anyhow!("Invalid PipeWire format pod"))?;
    let buffers_values = build_pipewire_bridge_buffers_pod(config.channels, config.sample_rate_hz)?;
    let buffers_pod =
        Pod::from_bytes(&buffers_values).ok_or_else(|| anyhow!("Invalid PipeWire buffers pod"))?;
    let mut params = [format_pod, buffers_pod];

    stream
        .connect(
            spa::utils::Direction::Input,
            target_id,
            pw::stream::StreamFlags::AUTOCONNECT
                | pw::stream::StreamFlags::MAP_BUFFERS
                | pw::stream::StreamFlags::DRIVER,
            &mut params,
        )
        .map_err(|e| anyhow!("Failed to connect PipeWire bridge input stream: {e:?}"))?;
    log::info!(
        "{} sink connected: node={} node_id={}",
        log_prefix,
        config.node_name,
        stream.node_id()
    );

    // Register capture rate for output-side Bresenham.  The actual trigger_process() calls are
    // made from THIS thread (capture mainloop) by draining the pending counter — cross-thread
    // trigger_process() on a DRIVER stream is unreliable and was causing severe under-triggering.
    input_control.register_direct_trigger_target(config.sample_rate_hz);
    log::info!(
        "{} registered direct trigger target: capture_rate={}Hz",
        log_prefix,
        config.sample_rate_hz
    );
    let direct_trigger_active = input_control.direct_trigger_active_arc();
    let mut next_direct_trigger_at: Option<Instant> = None;

    while !stop.load(Ordering::Relaxed)
        && !sys::ShutdownHandle::is_requested()
        && !sys::ShutdownHandle::is_restart_from_config_requested()
    {
        if direct_trigger_active.load(Ordering::Relaxed) {
            let pending = input_control.pending_input_triggers();
            let trigger_interval =
                current_direct_pw_driver_trigger_interval(input_control.as_ref());
            let _ = mainloop
                .loop_()
                .iterate(next_direct_pw_stream_driver_timeout(
                    pending.as_ref(),
                    next_direct_trigger_at,
                ));
            drain_direct_pw_stream_driver_trigger(
                &stream,
                pending.as_ref(),
                &mut next_direct_trigger_at,
                trigger_interval,
                log_prefix,
            );
        } else {
            let _ = mainloop
                .loop_()
                .iterate(next_pw_stream_driver_timeout(&trigger_schedule));
            drain_scheduled_pw_stream_trigger(&stream, &trigger_schedule, log_prefix);
        }
    }

    log::info!(
        "{} capture loop exiting: stop={} shutdown={} restart_from_config={} state={:?}",
        log_prefix,
        stop.load(Ordering::Relaxed),
        sys::ShutdownHandle::is_requested(),
        sys::ShutdownHandle::is_restart_from_config_requested(),
        stream.state()
    );

    let _ = stream.disconnect();
    log::info!("{} stream disconnected", log_prefix);
    Ok(())
}

// Bridge decode/runtime helpers.

#[cfg(target_os = "linux")]
fn instantiate_live_bridge(runtime: &LiveBridgeRuntimeConfig) -> Result<FormatBridgeBox> {
    let new_bridge = runtime
        .lib
        .new_bridge()
        .ok_or_else(|| anyhow!("Bridge plugin is missing the `new_bridge` export"))?;
    let mut bridge = new_bridge(runtime.strict_mode);
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
        let scaled = (sample.clamp(-1.0, 1.0) * i32::MAX as f32).round() as i32;
        pcm.push(scaled);
    }

    RDecodedFrame {
        sampling_frequency: sample_rate_hz,
        sample_count: frame_count as u32,
        channel_count: channel_count as u32,
        pcm: pcm.into(),
        channel_labels: seven_one_channel_labels().into(),
        metadata: Vec::new().into(),
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
