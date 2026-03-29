use super::decoder_thread::{DecodedAudioData, DecodedSource, DecoderMessage};
use anyhow::{Result, anyhow};
use audio_output::{
    AudioControl, InputBackend, InputControl, InputMode, InputSampleFormat,
    RequestedAudioInputConfig,
};
#[cfg(target_os = "linux")]
use audio_output::pipewire::PipewireBufferConfig;
use bridge_api::{RChannelLabel, RDecodedFrame};
#[cfg(target_os = "linux")]
use pipewire as pw;
#[cfg(target_os = "linux")]
use pw::spa;
#[cfg(target_os = "linux")]
use pw::spa::pod::Pod;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_LIVE_INPUT_CHANNELS: u16 = 8;
const DEFAULT_LIVE_INPUT_SAMPLE_RATE_HZ: u32 = 48_000;
const DEFAULT_LIVE_INPUT_NODE: &str = "omniphony_input_7_1";
const DEFAULT_LIVE_INPUT_DESCRIPTION: &str = "Omniphony Input 7.1";

#[derive(Debug, Clone, PartialEq, Eq)]
struct PipewireLiveInputConfig {
    node_name: String,
    node_description: String,
    channels: u16,
    sample_rate_hz: u32,
    target_latency_ms: u32,
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
    config: PipewireLiveInputConfig,
    stop: Arc<AtomicBool>,
    join: thread::JoinHandle<()>,
}

impl CaptureThreadHandle {
    fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.join.join();
    }
}

pub fn spawn_live_input_manager(
    tx: mpsc::SyncSender<Result<DecoderMessage>>,
    input_control: Arc<InputControl>,
    audio_control: Arc<AudioControl>,
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
                if bootstrap || input_control.take_apply_pending() {
                    bootstrap = false;
                    reconcile_live_input(&tx, &input_control, &audio_control, &mut current_capture);
                }

                #[cfg(target_os = "linux")]
                if requested_live_input_latency_ms(&audio_control)
                    != current_capture
                        .as_ref()
                        .map(|capture| capture.config.target_latency_ms)
                {
                    reconcile_live_input(&tx, &input_control, &audio_control, &mut current_capture);
                }

                let capture_finished = current_capture
                    .as_ref()
                    .map(|capture| capture.join.is_finished())
                    .unwrap_or(false);
                if capture_finished {
                    if let Some(capture) = current_capture.take() {
                        let _ = capture.join.join();
                    }
                    if input_control.requested_snapshot().mode == InputMode::Live {
                        input_control.set_input_state(
                            InputMode::Bridge,
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
                capture.stop();
            }
        })
        .expect("failed to spawn live input manager");

    LiveInputManagerHandle { stop, join }
}

fn reconcile_live_input(
    tx: &mpsc::SyncSender<Result<DecoderMessage>>,
    input_control: &Arc<InputControl>,
    #[allow(unused_variables)] audio_control: &Arc<AudioControl>,
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
            Some("bridge-decoded".to_string()),
        );
        input_control.set_input_error(Some(
            "live input is not implemented on this platform".to_string(),
        ));
        log::warn!("Live input requested on unsupported platform");
    }

    #[cfg(target_os = "linux")]
    {
        match resolve_pipewire_config(&requested, audio_control) {
            Ok(config) => {
                let needs_restart = current_capture
                    .as_ref()
                    .map(|capture| capture.config != config)
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
                                Some("bridge-decoded".to_string()),
                            );
                            input_control.set_input_error(Some(err.to_string()));
                            log::error!("Failed to start PipeWire live input: {err}");
                            return;
                        }
                    }
                }

                input_control.set_input_state(
                    InputMode::Live,
                    Some(InputBackend::Pipewire),
                    Some(config.channels),
                    Some(config.sample_rate_hz),
                    Some(config.node_name.clone()),
                    Some("pipewire-f32".to_string()),
                );
                input_control.set_input_error(None);
                log::info!(
                    "Live input active: backend=pipewire node={} channels={} rate={}Hz",
                    config.node_name,
                    config.channels,
                    config.sample_rate_hz
                );
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
                    Some("bridge-decoded".to_string()),
                );
                input_control.set_input_error(Some(err.to_string()));
                log::warn!("Live input request rejected: {err}");
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn resolve_pipewire_config(
    requested: &RequestedAudioInputConfig,
    audio_control: &AudioControl,
) -> Result<PipewireLiveInputConfig> {
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

    Ok(PipewireLiveInputConfig {
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
fn requested_live_input_latency_ms(audio_control: &AudioControl) -> Option<u32> {
    audio_control.requested_latency_target_ms()
}

#[cfg(target_os = "linux")]
fn spawn_pipewire_capture(
    tx: mpsc::SyncSender<Result<DecoderMessage>>,
    input_control: Arc<InputControl>,
    config: PipewireLiveInputConfig,
) -> Result<CaptureThreadHandle> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);
    let thread_name = format!("pw-live-input-{}", config.node_name);
    let config_for_thread = config.clone();
    let join = thread::Builder::new().name(thread_name).spawn(move || {
        if let Err(err) =
            run_pipewire_capture_loop(tx, input_control, config_for_thread, stop_for_thread)
        {
            log::error!("PipeWire live input thread exited with error: {err}");
        }
    })?;

    Ok(CaptureThreadHandle { config, stop, join })
}

#[cfg(target_os = "linux")]
fn run_pipewire_capture_loop(
    tx: mpsc::SyncSender<Result<DecoderMessage>>,
    input_control: Arc<InputControl>,
    config: PipewireLiveInputConfig,
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
        .param_changed(|_, user_data, id, param| {
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
