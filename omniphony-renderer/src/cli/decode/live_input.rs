use super::decoder_thread::{DecodedAudioData, DecodedSource, DecoderMessage};
use anyhow::{Result, anyhow};
use audio_output::{
    AudioControl, InputBackend, InputControl, InputMode, InputSampleFormat,
    RequestedAudioInputConfig,
};
#[cfg(target_os = "linux")]
use audio_output::pipewire::PipewireBufferConfig;
use bridge_api::{FormatBridgeBox, RChannelLabel, RDecodedFrame, RInputTransport};
#[cfg(target_os = "linux")]
use pipewire as pw;
#[cfg(target_os = "linux")]
use pw::spa;
#[cfg(target_os = "linux")]
use pw::spa::pod::{object, property};
#[cfg(target_os = "linux")]
use pw::spa::pod::Pod;
use spdif::SpdifParser;
use std::cell::{Cell, RefCell};
use std::ffi::{CStr, CString};
#[cfg(target_os = "linux")]
use std::mem::MaybeUninit;
#[cfg(target_os = "linux")]
use std::os::fd::RawFd;
use std::os::raw::c_void;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const DEFAULT_LIVE_INPUT_CHANNELS: u16 = 8;
const DEFAULT_LIVE_INPUT_SAMPLE_RATE_HZ: u32 = 48_000;
const DEFAULT_LIVE_BRIDGE_SAMPLE_RATE_HZ: u32 = 192_000;
const DEFAULT_LIVE_INPUT_NODE: &str = "omniphony_input_7_1";
const DEFAULT_LIVE_INPUT_DESCRIPTION: &str = "Omniphony Input 7.1";
const DEFAULT_LIVE_BRIDGE_NODE: &str = "omniphony";
const DEFAULT_LIVE_BRIDGE_DESCRIPTION: &str = "Omniphony Bridge Input";
#[cfg(target_os = "linux")]
const TRUEHD_ONLY_IEC958_CODECS_PROP: &str = "[ \"TRUEHD\" ]";
#[cfg(target_os = "linux")]
const IEC958_AUDIO_POSITION_PROP: &str = "[ FL FR C LFE SL SR RL RR ]";
#[cfg(target_os = "linux")]
const LIVE_BRIDGE_LOG_INTERVAL: Duration = Duration::from_secs(1);
#[cfg(target_os = "linux")]
const PIPEWIRE_BRIDGE_NODE_MAX_BUFFERS: usize = 64;
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
}

#[derive(Clone)]
struct PipewireBridgeInputConfig {
    node_name: String,
    node_description: String,
    channels: u16,
    sample_rate_hz: u32,
    target_latency_ms: u32,
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

#[cfg(target_os = "linux")]
enum PipewireBridgeBackendKind {
    PwAdapter,
    PwClientNode,
    PwExportedNode,
    PwStream,
    PwFilter,
}

#[cfg(target_os = "linux")]
struct LiveBridgeIngestRuntime {
    raw_tx: mpsc::SyncSender<(u8, Vec<u8>)>,
    spdif_parser: SpdifParser,
}

#[cfg(target_os = "linux")]
impl LiveBridgeIngestRuntime {
    fn process_chunk(&mut self, chunk: &[u8]) -> (usize, usize) {
        let mut packet_count = 0usize;
        self.spdif_parser.push_bytes(chunk);
        while let Some(packet) = self.spdif_parser.get_next_packet() {
            packet_count += 1;
            let _ = self.raw_tx.try_send((packet.data_type, packet.payload));
        }
        (packet_count, 0)
    }
}

#[cfg(target_os = "linux")]
struct BridgeDecodeWorker {
    bridge: FormatBridgeBox,
    tx: mpsc::SyncSender<Result<DecoderMessage>>,
    raw_rx: mpsc::Receiver<(u8, Vec<u8>)>,
    strict_mode: bool,
    first_frame_logs_remaining: usize,
}

#[cfg(target_os = "linux")]
impl BridgeDecodeWorker {
    fn run(mut self) {
        while let Ok((data_type, payload)) = self.raw_rx.recv() {
            let decode_started_at = Instant::now();
            let result = self.bridge.push_packet(
                payload.as_slice().into(),
                RInputTransport::Iec61937,
                data_type,
            );
            let decode_time_ms = decode_started_at.elapsed().as_secs_f32() * 1000.0;
            if result.frames.is_empty() || !result.error_message.is_empty() || result.did_reset {
                log::info!(
                    "PipeWire bridge packet: data_type=0x{:02X} payload_bytes={} frames={} reset={} error={}",
                    data_type,
                    payload.len(),
                    result.frames.len(),
                    result.did_reset,
                    result.error_message
                );
            }
            if result.did_reset {
                if self.strict_mode && !result.error_message.is_empty() {
                    let _ = self.tx.try_send(Err(anyhow!("{}", result.error_message)));
                    return;
                }
                if self.strict_mode {
                    let _ = self
                        .tx
                        .try_send(Ok(DecoderMessage::FlushRequest(DecodedSource::Bridge)));
                }
            }
            let frame_count = result.frames.len().max(1) as f32;
            let per_frame_decode_time_ms = decode_time_ms / frame_count;
            for frame in result.frames {
                if self.first_frame_logs_remaining > 0 {
                    self.first_frame_logs_remaining -= 1;
                    let frame_ms =
                        frame.sample_count as f64 / frame.sampling_frequency.max(1) as f64 * 1000.0;
                    log::warn!(
                        "PipeWire bridge decoded frame: sr={} sample_count={} ch={} frame_ms={:.3} data_type=0x{:02X} payload_bytes={}",
                        frame.sampling_frequency,
                        frame.sample_count,
                        frame.channel_count,
                        frame_ms,
                        data_type,
                        payload.len()
                    );
                }
                let send_result = self.tx.try_send(Ok(DecoderMessage::AudioData(DecodedAudioData {
                    source: DecodedSource::Bridge,
                    frame,
                    decode_time_ms: per_frame_decode_time_ms,
                    sent_at: Instant::now(),
                })));
                if send_result.is_err() {
                    break;
                }
            }
        }
    }
}

#[cfg(target_os = "linux")]
struct BridgeCaptureUserData {
    rate_hz: u32,
    channels: u32,
    last_log_at: Instant,
    add_buffer_calls_since_log: usize,
    remove_buffer_calls_since_log: usize,
    drained_calls_since_log: usize,
    io_changed_calls_since_log: usize,
    first_process_logged: bool,
    first_buffer_layout_logged: bool,
    process_calls_since_log: usize,
    datas_empty_since_log: usize,
    data_missing_since_log: usize,
    zero_size_chunks_since_log: usize,
    oversized_chunks_since_log: usize,
    bytes_since_log: usize,
    buffers_since_log: usize,
    sync_buffers_since_log: usize,
    packets_since_log: usize,
    frames_since_log: usize,
    empty_polls_since_log: usize,
    /// Accumulation buffer for PwStream clock-domain correction (192kHz transport over 48kHz graph).
    accumulate_buf: Vec<u8>,
    /// Number of process callbacks accumulated so far.
    accumulate_count: usize,
    /// Last time we fired trigger_process() on an idle/empty callback.
    /// Used to rate-limit idle spinning to ~one quantum period, preventing the DRIVER
    /// from overwhelming PipeWire with 250K graph cycles/s when no data is available.
    last_idle_trigger: Instant,
    /// Last trigger interval derived from pw_stream_get_time_n().
    /// When unavailable, we fall back to PW_DRIVER_IDLE_TRIGGER_INTERVAL.
    dynamic_trigger_interval: Option<Duration>,
    /// Last time we logged pw_time-derived scheduling data.
    last_pw_time_log_at: Instant,
    /// Rate-adjust feedback from the output resampler.
    /// Shared with the output side via InputControl. Used to correct dynamic_trigger_interval
    /// so the DRIVER clock converges toward the hardware DAC rate and reduces resampler load.
    output_rate_adjust: Arc<AtomicU32>,
}

#[cfg(target_os = "linux")]
#[derive(Default)]
struct PwDriverTriggerSchedule {
    next_trigger_at: Option<Instant>,
    pending_reason: Option<&'static str>,
    trigger_calls_since_log: usize,
    trigger_errors_since_log: usize,
}

#[cfg(target_os = "linux")]
struct PipewireBridgeFilterUserData {
    input_control: Arc<InputControl>,
    config: PipewireBridgeInputConfig,
    stop: Arc<AtomicBool>,
    ingest: RefCell<LiveBridgeIngestRuntime>,
    input_port: Cell<*mut c_void>,
    metrics: RefCell<BridgeCaptureUserData>,
}

#[cfg(target_os = "linux")]
struct PipewireBridgeExportNode {
    iface: spa::sys::spa_node,
    hooks: spa::sys::spa_hook_list,
    node_info: spa::sys::spa_node_info,
    port_info: spa::sys::spa_port_info,
    monitor_port_info: spa::sys::spa_port_info,
    node_params: [spa::sys::spa_param_info; 8],
    port_params: [spa::sys::spa_param_info; 7],
    monitor_port_params: [spa::sys::spa_param_info; 7],
    node_props: spa::sys::spa_dict,
    node_prop_items: [spa::sys::spa_dict_item; 7],
    port_props: spa::sys::spa_dict,
    port_prop_items: [spa::sys::spa_dict_item; 5],
    monitor_port_props: spa::sys::spa_dict,
    monitor_port_prop_items: [spa::sys::spa_dict_item; 6],
    node_name_value: CString,
    node_description_value: CString,
    media_name_value: CString,
    port_group_value: CString,
    port_name_value: CString,
    port_alias_value: CString,
    port_audio_channel_value: CString,
    monitor_port_group_value: CString,
    monitor_port_name_value: CString,
    monitor_port_alias_value: CString,
    monitor_port_audio_channel_value: CString,
    enum_port_config_bytes: Vec<u8>,
    port_config_bytes: Vec<u8>,
    enum_format_bytes: Vec<u8>,
    format_bytes: Vec<u8>,
    props_param_bytes: Vec<u8>,
    meta_param_bytes: Vec<u8>,
    latency_param_bytes: Vec<u8>,
    process_latency_param_bytes: Vec<u8>,
    tag_param_bytes: Vec<u8>,
    monitor_tag_param_bytes: Vec<u8>,
    buffers_param_bytes: Vec<u8>,
    io_buffers_param_bytes: Vec<u8>,
    io_buffers: *mut spa::sys::spa_io_buffers,
    io_clock: *mut spa::sys::spa_io_clock,
    io_position: *mut spa::sys::spa_io_position,
    buffers: [*mut spa::sys::spa_buffer; PIPEWIRE_BRIDGE_NODE_MAX_BUFFERS],
    owned_buffer_data: [Option<Vec<u8>>; PIPEWIRE_BRIDGE_NODE_MAX_BUFFERS],
    n_buffers: u32,
    input_control: Arc<InputControl>,
    config: PipewireBridgeInputConfig,
    stop: Arc<AtomicBool>,
    ingest: RefCell<LiveBridgeIngestRuntime>,
    metrics: RefCell<BridgeCaptureUserData>,
    format_configured: bool,
    started: bool,
    suspended: bool,
}

#[cfg(target_os = "linux")]
struct PipewireBridgeClientNodeState {
    hook: spa::sys::spa_hook,
    core_hook: spa::sys::spa_hook,
    proxy_hook: spa::sys::spa_hook,
    node_hook: spa::sys::spa_hook,
    client_node: *mut pw::sys::pw_client_node,
    input_control: Arc<InputControl>,
    config: PipewireBridgeInputConfig,
    stop: Arc<AtomicBool>,
    ingest: RefCell<LiveBridgeIngestRuntime>,
    node_info: spa::sys::spa_node_info,
    port_info: spa::sys::spa_port_info,
    node_params: [spa::sys::spa_param_info; 8],
    port_params: [spa::sys::spa_param_info; 7],
    node_props: spa::sys::spa_dict,
    node_prop_items: [spa::sys::spa_dict_item; 7],
    port_props: spa::sys::spa_dict,
    port_prop_items: [spa::sys::spa_dict_item; 5],
    node_name_value: CString,
    node_description_value: CString,
    media_name_value: CString,
    port_group_value: CString,
    port_name_value: CString,
    port_alias_value: CString,
    port_audio_channel_value: CString,
    enum_port_config_bytes: Vec<u8>,
    port_config_bytes: Vec<u8>,
    enum_format_bytes: Vec<u8>,
    format_bytes: Vec<u8>,
    props_param_bytes: Vec<u8>,
    meta_param_bytes: Vec<u8>,
    latency_param_bytes: Vec<u8>,
    process_latency_param_bytes: Vec<u8>,
    tag_param_bytes: Vec<u8>,
    buffers_param_bytes: Vec<u8>,
    io_buffers_param_bytes: Vec<u8>,
    mapped_mems: Vec<PipewireBridgeMappedMem>,
    transport_ptr: *mut c_void,
    transport_mem_id: Option<u32>,
    transport_size: u32,
    activation_ptr: *mut c_void,
    activation_mem_id: Option<u32>,
    activation_size: u32,
    io_clock: *mut spa::sys::spa_io_clock,
    io_position: *mut spa::sys::spa_io_position,
    format_configured: bool,
}

#[cfg(target_os = "linux")]
struct PipewireBridgeMappedMem {
    id: u32,
    #[allow(dead_code)]
    mem_type: u32,
    #[allow(dead_code)]
    flags: u32,
    fd: RawFd,
    ptr: *mut c_void,
    size: usize,
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct PipewireBridgePwNodeActivationState {
    status: u32,
    pending: i32,
}

#[cfg(target_os = "linux")]
#[repr(C)]
struct PipewireBridgePwNodeActivation {
    status: u32,
    status_flags: u32,
    state: [PipewireBridgePwNodeActivationState; 2],
    signal_time: u64,
    awake_time: u64,
    finish_time: u64,
    prev_signal_time: u64,
    reposition: spa::sys::spa_io_segment,
    segment: spa::sys::spa_io_segment,
    segment_owner: [u32; 16],
    prev_awake_time: u64,
    prev_finish_time: u64,
    padding: [u32; 7],
    client_version: u32,
    server_version: u32,
    active_driver_id: u32,
    driver_id: u32,
    flags: u32,
    position: spa::sys::spa_io_position,
    sync_timeout: u64,
    sync_left: u64,
    cpu_load: [f32; 3],
    xrun_count: u32,
    xrun_time: u64,
    xrun_delay: u64,
    max_delay: u64,
    command: u32,
    reposition_owner: u32,
}

#[cfg(target_os = "linux")]
struct PipewireBridgeAdapterState {
    hook: spa::sys::spa_hook,
    config: PipewireBridgeInputConfig,
    bound: Cell<bool>,
    removed: Cell<bool>,
    errored: Cell<bool>,
    global_id: Cell<u32>,
    object_serial: RefCell<Option<String>>,
    node_name: RefCell<Option<String>>,
}

#[cfg(target_os = "linux")]
#[derive(Copy, Clone)]
struct RawSpaPodKey(u32);

#[cfg(target_os = "linux")]
impl RawSpaPodKey {
    fn as_raw(&self) -> u32 {
        self.0
    }
}

#[cfg(target_os = "linux")]
impl PipewireBridgeExportNode {
    fn new(
        input_control: Arc<InputControl>,
        config: PipewireBridgeInputConfig,
        stop: Arc<AtomicBool>,
        ingest: LiveBridgeIngestRuntime,
    ) -> Result<Box<Self>> {
        let node_name_value = CString::new(config.node_name.clone()).expect("valid CString");
        let node_description_value =
            CString::new(config.node_description.clone()).expect("valid CString");
        let media_name_value =
            CString::new(config.node_description.clone()).expect("valid CString");
        let port_group_value = CString::new("stream.0").expect("valid CString");
        let port_name_value = CString::new("playback_FL").expect("valid CString");
        let port_alias_value = CString::new(format!("{}:playback_FL", config.node_description))
            .expect("valid CString");
        let port_audio_channel_value = CString::new("FL").expect("valid CString");
        let monitor_port_group_value = CString::new("stream.0").expect("valid CString");
        let monitor_port_name_value = CString::new("monitor_FL").expect("valid CString");
        let monitor_port_alias_value =
            CString::new(format!("{}:monitor_FL", config.node_description))
                .expect("valid CString");
        let monitor_port_audio_channel_value = CString::new("FL").expect("valid CString");
        let mut node = Box::new(Self {
            iface: spa::sys::spa_node {
                iface: spa::sys::spa_interface {
                    type_: std::ptr::null(),
                    version: spa::sys::SPA_VERSION_NODE,
                    cb: spa::sys::spa_callbacks {
                        funcs: std::ptr::null(),
                        data: std::ptr::null_mut(),
                    },
                },
            },
            hooks: unsafe { std::mem::zeroed() },
            node_info: spa::sys::spa_node_info {
                max_input_ports: 1,
                max_output_ports: 1,
                change_mask: (spa::sys::SPA_NODE_CHANGE_MASK_FLAGS
                    | spa::sys::SPA_NODE_CHANGE_MASK_PROPS
                    | spa::sys::SPA_NODE_CHANGE_MASK_PARAMS) as u64,
                flags: (spa::sys::SPA_NODE_FLAG_RT | spa::sys::SPA_NODE_FLAG_NEED_CONFIGURE)
                    as u64,
                props: std::ptr::null_mut(),
                params: std::ptr::null_mut(),
                n_params: 0,
            },
            port_info: spa::sys::spa_port_info {
                change_mask: (spa::sys::SPA_PORT_CHANGE_MASK_FLAGS
                    | spa::sys::SPA_PORT_CHANGE_MASK_PROPS
                    | spa::sys::SPA_PORT_CHANGE_MASK_PARAMS) as u64,
                flags: (spa::sys::SPA_PORT_FLAG_CAN_ALLOC_BUFFERS
                    | spa::sys::SPA_PORT_FLAG_NO_REF
                    | spa::sys::SPA_PORT_FLAG_TERMINAL) as u64,
                rate: spa::sys::spa_fraction { num: 0, denom: 1 },
                props: std::ptr::null(),
                params: std::ptr::null_mut(),
                n_params: 0,
            },
            monitor_port_info: spa::sys::spa_port_info {
                change_mask: (spa::sys::SPA_PORT_CHANGE_MASK_FLAGS
                    | spa::sys::SPA_PORT_CHANGE_MASK_PROPS
                    | spa::sys::SPA_PORT_CHANGE_MASK_PARAMS) as u64,
                flags: (spa::sys::SPA_PORT_FLAG_NO_REF | spa::sys::SPA_PORT_FLAG_TERMINAL) as u64,
                rate: spa::sys::spa_fraction { num: 0, denom: 1 },
                props: std::ptr::null(),
                params: std::ptr::null_mut(),
                n_params: 0,
            },
            node_params: [
                spa_param_info(spa::sys::SPA_PARAM_Props, spa::sys::SPA_PARAM_INFO_READWRITE),
                spa_param_info(spa::sys::SPA_PARAM_EnumFormat, spa::sys::SPA_PARAM_INFO_READ),
                spa_param_info(spa::sys::SPA_PARAM_Format, spa::sys::SPA_PARAM_INFO_WRITE),
                spa_param_info(
                    spa::sys::SPA_PARAM_EnumPortConfig,
                    spa::sys::SPA_PARAM_INFO_READ,
                ),
                spa_param_info(
                    spa::sys::SPA_PARAM_PortConfig,
                    spa::sys::SPA_PARAM_INFO_READWRITE,
                ),
                spa_param_info(
                    spa::sys::SPA_PARAM_Latency,
                    spa::sys::SPA_PARAM_INFO_READWRITE,
                ),
                spa_param_info(
                    spa::sys::SPA_PARAM_ProcessLatency,
                    spa::sys::SPA_PARAM_INFO_READWRITE,
                ),
                spa_param_info(spa::sys::SPA_PARAM_Tag, spa::sys::SPA_PARAM_INFO_READWRITE),
            ],
            port_params: [
                spa_param_info(spa::sys::SPA_PARAM_EnumFormat, spa::sys::SPA_PARAM_INFO_READ),
                spa_param_info(spa::sys::SPA_PARAM_Meta, spa::sys::SPA_PARAM_INFO_READ),
                spa_param_info(spa::sys::SPA_PARAM_IO, spa::sys::SPA_PARAM_INFO_READ),
                spa_param_info(spa::sys::SPA_PARAM_Format, spa::sys::SPA_PARAM_INFO_WRITE),
                spa_param_info(spa::sys::SPA_PARAM_Buffers, 0),
                spa_param_info(spa::sys::SPA_PARAM_Latency, spa::sys::SPA_PARAM_INFO_READWRITE),
                spa_param_info(spa::sys::SPA_PARAM_Tag, spa::sys::SPA_PARAM_INFO_READWRITE),
            ],
            monitor_port_params: [
                spa_param_info(spa::sys::SPA_PARAM_EnumFormat, spa::sys::SPA_PARAM_INFO_READ),
                spa_param_info(spa::sys::SPA_PARAM_Meta, spa::sys::SPA_PARAM_INFO_READ),
                spa_param_info(spa::sys::SPA_PARAM_IO, spa::sys::SPA_PARAM_INFO_READ),
                spa_param_info(spa::sys::SPA_PARAM_Format, spa::sys::SPA_PARAM_INFO_WRITE),
                spa_param_info(spa::sys::SPA_PARAM_Buffers, 0),
                spa_param_info(spa::sys::SPA_PARAM_Latency, spa::sys::SPA_PARAM_INFO_READWRITE),
                spa_param_info(spa::sys::SPA_PARAM_Tag, spa::sys::SPA_PARAM_INFO_READWRITE),
            ],
            node_props: spa::sys::spa_dict {
                flags: 0,
                n_items: 7,
                items: std::ptr::null(),
            },
            node_prop_items: [
                spa::sys::spa_dict_item {
                    key: pw::keys::NODE_NAME.as_ptr().cast(),
                    value: node_name_value.as_ptr(),
                },
                spa::sys::spa_dict_item {
                    key: pw::keys::NODE_DESCRIPTION.as_ptr().cast(),
                    value: node_description_value.as_ptr(),
                },
                spa::sys::spa_dict_item {
                    key: pw::keys::MEDIA_NAME.as_ptr().cast(),
                    value: media_name_value.as_ptr(),
                },
                spa::sys::spa_dict_item {
                    key: pw::keys::MEDIA_CLASS.as_ptr().cast(),
                    value: b"Audio/Sink\0".as_ptr().cast(),
                },
                spa::sys::spa_dict_item {
                    key: pw::keys::MEDIA_TYPE.as_ptr().cast(),
                    value: b"Audio\0".as_ptr().cast(),
                },
                spa::sys::spa_dict_item {
                    key: pw::keys::MEDIA_CATEGORY.as_ptr().cast(),
                    value: b"Playback\0".as_ptr().cast(),
                },
                spa::sys::spa_dict_item {
                    key: pw::keys::MEDIA_ROLE.as_ptr().cast(),
                    value: b"Movie\0".as_ptr().cast(),
                },
            ],
            port_props: spa::sys::spa_dict {
                flags: 0,
                n_items: 5,
                items: std::ptr::null(),
            },
            port_prop_items: [
                spa::sys::spa_dict_item {
                    key: b"format.dsp\0".as_ptr().cast(),
                    value: b"32 bit float mono audio\0".as_ptr().cast(),
                },
                spa::sys::spa_dict_item {
                    key: b"port.group\0".as_ptr().cast(),
                    value: port_group_value.as_ptr(),
                },
                spa::sys::spa_dict_item {
                    key: pw::keys::PORT_NAME.as_ptr().cast(),
                    value: port_name_value.as_ptr(),
                },
                spa::sys::spa_dict_item {
                    key: pw::keys::PORT_ALIAS.as_ptr().cast(),
                    value: port_alias_value.as_ptr(),
                },
                spa::sys::spa_dict_item {
                    key: b"audio.channel\0".as_ptr().cast(),
                    value: port_audio_channel_value.as_ptr(),
                },
            ],
            monitor_port_props: spa::sys::spa_dict {
                flags: 0,
                n_items: 6,
                items: std::ptr::null(),
            },
            monitor_port_prop_items: [
                spa::sys::spa_dict_item {
                    key: b"format.dsp\0".as_ptr().cast(),
                    value: b"32 bit float mono audio\0".as_ptr().cast(),
                },
                spa::sys::spa_dict_item {
                    key: b"port.monitor\0".as_ptr().cast(),
                    value: b"true\0".as_ptr().cast(),
                },
                spa::sys::spa_dict_item {
                    key: b"port.group\0".as_ptr().cast(),
                    value: monitor_port_group_value.as_ptr(),
                },
                spa::sys::spa_dict_item {
                    key: pw::keys::PORT_NAME.as_ptr().cast(),
                    value: monitor_port_name_value.as_ptr(),
                },
                spa::sys::spa_dict_item {
                    key: pw::keys::PORT_ALIAS.as_ptr().cast(),
                    value: monitor_port_alias_value.as_ptr(),
                },
                spa::sys::spa_dict_item {
                    key: b"audio.channel\0".as_ptr().cast(),
                    value: monitor_port_audio_channel_value.as_ptr(),
                },
            ],
            node_name_value,
            node_description_value,
            media_name_value,
            port_group_value,
            port_name_value,
            port_alias_value,
            port_audio_channel_value,
            monitor_port_group_value,
            monitor_port_name_value,
            monitor_port_alias_value,
            monitor_port_audio_channel_value,
            enum_port_config_bytes: build_pipewire_bridge_enum_port_config_pod()?,
            port_config_bytes: build_pipewire_bridge_port_config_pod()?,
            enum_format_bytes: build_pipewire_bridge_format_pod(
                &config,
                spa::param::ParamType::EnumFormat,
            )?,
            format_bytes: build_pipewire_bridge_format_pod(
                &config,
                spa::param::ParamType::Format,
            )?,
            props_param_bytes: build_pipewire_bridge_props_pod()?,
            meta_param_bytes: build_pipewire_bridge_meta_pod()?,
            latency_param_bytes: build_pipewire_bridge_latency_pod()?,
            process_latency_param_bytes: build_pipewire_bridge_process_latency_pod()?,
            tag_param_bytes: build_pipewire_bridge_tag_pod(spa::sys::SPA_DIRECTION_INPUT)?,
            monitor_tag_param_bytes: build_pipewire_bridge_tag_pod(spa::sys::SPA_DIRECTION_OUTPUT)?,
            buffers_param_bytes: build_pipewire_bridge_buffers_pod(&config)?,
            io_buffers_param_bytes: build_pipewire_bridge_io_buffers_pod()?,
            io_buffers: std::ptr::null_mut(),
            io_clock: std::ptr::null_mut(),
            io_position: std::ptr::null_mut(),
            buffers: [std::ptr::null_mut(); PIPEWIRE_BRIDGE_NODE_MAX_BUFFERS],
            owned_buffer_data: std::array::from_fn(|_| None),
            n_buffers: 0,
            input_control,
            config: config.clone(),
            stop,
            ingest: RefCell::new(ingest),
            metrics: RefCell::new(BridgeCaptureUserData {
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
                accumulate_buf: Vec::new(),
                accumulate_count: 0,
                last_idle_trigger: Instant::now(),
                dynamic_trigger_interval: None,
                last_pw_time_log_at: Instant::now(),
                output_rate_adjust: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            }),
            format_configured: false,
            started: false,
            suspended: false,
        });

        node.node_info.params = node.node_params.as_mut_ptr();
        node.node_info.n_params = node.node_params.len() as u32;
        node.node_props.items = node.node_prop_items.as_ptr();
        node.node_info.props = (&mut node.node_props as *mut spa::sys::spa_dict).cast();
        node.port_props.items = node.port_prop_items.as_ptr();
        node.port_info.props = (&mut node.port_props as *mut spa::sys::spa_dict).cast();
        node.port_info.params = node.port_params.as_mut_ptr();
        node.port_info.n_params = node.port_params.len() as u32;
        node.monitor_port_props.items = node.monitor_port_prop_items.as_ptr();
        node.monitor_port_info.props =
            (&mut node.monitor_port_props as *mut spa::sys::spa_dict).cast();
        node.monitor_port_info.params = node.monitor_port_params.as_mut_ptr();
        node.monitor_port_info.n_params = node.monitor_port_params.len() as u32;

        unsafe {
            spa::sys::spa_hook_list_init(&mut node.hooks);
        }

        let node_ptr: *mut Self = &mut *node;
        node.iface.iface.type_ = spa::sys::SPA_TYPE_INTERFACE_Node.as_ptr().cast();
        node.iface.iface.version = spa::sys::SPA_VERSION_NODE;
        node.iface.iface.cb.funcs = (&PIPEWIRE_BRIDGE_EXPORTED_NODE_METHODS
            as *const spa::sys::spa_node_methods)
            .cast();
        node.iface.iface.cb.data = node_ptr.cast();

        Ok(node)
    }

    fn update_port_params_for_format(&mut self, configured: bool) {
        self.format_configured = configured;
        self.node_info.change_mask = (spa::sys::SPA_NODE_CHANGE_MASK_FLAGS
            | spa::sys::SPA_NODE_CHANGE_MASK_PARAMS) as u64;
        self.node_info.flags = if configured {
            spa::sys::SPA_NODE_FLAG_RT as u64
        } else {
            (spa::sys::SPA_NODE_FLAG_RT | spa::sys::SPA_NODE_FLAG_NEED_CONFIGURE) as u64
        };
        self.port_info.change_mask = (spa::sys::SPA_PORT_CHANGE_MASK_PROPS
            | spa::sys::SPA_PORT_CHANGE_MASK_PARAMS) as u64;
        self.port_info.params = self.port_params.as_mut_ptr();
        self.port_info.n_params = self.port_params.len() as u32;
        self.monitor_port_info.change_mask = (spa::sys::SPA_PORT_CHANGE_MASK_PROPS
            | spa::sys::SPA_PORT_CHANGE_MASK_PARAMS) as u64;
        self.monitor_port_info.params = self.monitor_port_params.as_mut_ptr();
        self.monitor_port_info.n_params = self.monitor_port_params.len() as u32;
    }
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
                if bootstrap || input_control.take_apply_pending() {
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
                    Some("bridge-decoded".to_string()),
                );
                input_control.set_input_error(Some(err.to_string()));
                log::warn!("Live input request rejected: {err}");
            }
        }
    }
}

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
        InputMode::PipewireBridge => resolve_pipewire_bridge_config(
            requested,
            audio_control,
            bridge_runtime,
        )
        .map(ActiveCaptureConfig::Bridge),
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
        runtime: bridge_runtime.clone(),
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
    let worker = BridgeDecodeWorker {
        bridge,
        tx,
        raw_rx,
        strict_mode: config.runtime.strict_mode,
        first_frame_logs_remaining: 16,
    };
    let ingest = LiveBridgeIngestRuntime {
        raw_tx,
        spdif_parser: SpdifParser::new(),
    };
    thread::Builder::new()
        .name("bridge-decode".to_string())
        .spawn(move || worker.run())
        .map_err(|e| anyhow!("Failed to spawn bridge decode worker: {e}"))?;

    match selected_pipewire_bridge_backend() {
        PipewireBridgeBackendKind::PwAdapter => {
            run_pipewire_bridge_adapter_backend(input_control, config, stop, ingest)
        }
        PipewireBridgeBackendKind::PwClientNode => {
            run_pipewire_bridge_client_node_backend(input_control, config, stop, ingest)
        }
        PipewireBridgeBackendKind::PwExportedNode => {
            run_pipewire_bridge_exported_node_backend(input_control, config, stop, ingest)
        }
        PipewireBridgeBackendKind::PwStream => {
            run_pipewire_bridge_pw_stream_backend(input_control, config, stop, ingest)
        }
        PipewireBridgeBackendKind::PwFilter => {
            run_pipewire_bridge_filter_backend(input_control, config, stop, ingest)
        }
    }
}

#[cfg(target_os = "linux")]
fn selected_pipewire_bridge_backend() -> PipewireBridgeBackendKind {
    PipewireBridgeBackendKind::PwStream
}

#[cfg(target_os = "linux")]
fn current_pw_driver_trigger_interval(user_data: &BridgeCaptureUserData) -> Duration {
    user_data
        .dynamic_trigger_interval
        .unwrap_or(PW_DRIVER_IDLE_TRIGGER_INTERVAL)
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
fn next_pw_stream_driver_timeout(
    schedule: &Rc<RefCell<PwDriverTriggerSchedule>>,
) -> Duration {
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
fn drain_scheduled_pw_stream_trigger(
    stream: &pw::stream::Stream,
    schedule: &Rc<RefCell<PwDriverTriggerSchedule>>,
    log_prefix: &'static str,
) {
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
                log::info!(
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
        log::info!(
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
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| anyhow!("Failed to create PipeWire main loop: {e:?}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|e| anyhow!("Failed to create PipeWire context: {e:?}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| anyhow!("Failed to connect to PipeWire core: {e:?}"))?;

    let requested_latency_frames =
        ((config.target_latency_ms as u64 * config.sample_rate_hz as u64) / 1000).max(1) as u32;
    let requested_latency = format!("{}/{}", requested_latency_frames, config.sample_rate_hz);
    let props = build_pipewire_bridge_stream_properties(&config, &requested_latency);
    log::info!(
        "Publishing PipeWire bridge input sink: node={} description={} channels={} rate={}Hz latency={} iec958.codecs={} audio.position={} resample.disable=true",
        config.node_name,
        config.node_description,
        config.channels,
        config.sample_rate_hz,
        requested_latency,
        TRUEHD_ONLY_IEC958_CODECS_PROP,
        IEC958_AUDIO_POSITION_PROP
    );

    run_pipewire_bridge_capture_stream(
        &mainloop,
        &core,
        stop,
        input_control,
        config,
        ingest,
        None,
        props,
        "omniphony-live-bridge-input",
        "PipeWire bridge input",
    )
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
            accumulate_buf: Vec::new(),
            accumulate_count: 0,
            last_idle_trigger: Instant::now(),
            dynamic_trigger_interval: None,
            last_pw_time_log_at: Instant::now(),
            output_rate_adjust: input_control.output_rate_adjust_atomic(),
        })
        .state_changed(move |stream, user_data, old, new| {
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
            log::info!(
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
            log::info!(
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
            log::info!(
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
            refresh_pw_stream_driver_timing(stream, user_data, log_prefix);
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
                    log::info!(
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
                log::info!(
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
                    log::info!(
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
            if user_data.process_calls_since_log <= 8 && user_data.channels > 0 && user_data.rate_hz > 0
            {
                let transport_ms = byte_len as f64
                    / (user_data.channels as f64 * std::mem::size_of::<u16>() as f64)
                    / user_data.rate_hz as f64
                    * 1000.0;
                log::warn!(
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
                log::info!(
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
                    log::warn!("{} ingest has audio buffers but no IEC61937 sync words yet", log_prefix);
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
            log::info!(
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

    let format_values =
        build_pipewire_bridge_format_pod(&config, spa::param::ParamType::EnumFormat)?;
    let format_pod =
        Pod::from_bytes(&format_values).ok_or_else(|| anyhow!("Invalid PipeWire format pod"))?;
    let buffers_values = build_pipewire_bridge_buffers_pod(&config)?;
    let buffers_pod = Pod::from_bytes(&buffers_values)
        .ok_or_else(|| anyhow!("Invalid PipeWire buffers pod"))?;
    let mut params = [format_pod, buffers_pod];

    stream
        .connect(
            spa::utils::Direction::Input,
            target_id,
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::DRIVER,
            &mut params,
        )
        .map_err(|e| anyhow!("Failed to connect PipeWire bridge input stream: {e:?}"))?;
    log::info!(
        "{} sink connected: node={} node_id={}",
        log_prefix,
        config.node_name,
        stream.node_id()
    );

    while !stop.load(Ordering::Relaxed)
        && !sys::ShutdownHandle::is_requested()
        && !sys::ShutdownHandle::is_restart_from_config_requested()
    {
        let _ = mainloop
            .loop_()
            .iterate(next_pw_stream_driver_timeout(&trigger_schedule));
        drain_scheduled_pw_stream_trigger(&stream, &trigger_schedule, log_prefix);
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

#[cfg(target_os = "linux")]
fn run_pipewire_bridge_adapter_backend(
    input_control: Arc<InputControl>,
    config: PipewireBridgeInputConfig,
    stop: Arc<AtomicBool>,
    ingest: LiveBridgeIngestRuntime,
) -> Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| anyhow!("Failed to create PipeWire main loop: {e:?}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|e| anyhow!("Failed to create PipeWire context: {e:?}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| anyhow!("Failed to connect to PipeWire core: {e:?}"))?;

    let requested_latency_frames =
        ((config.target_latency_ms as u64 * config.sample_rate_hz as u64) / 1000).max(1) as u32;
    let requested_latency = format!("{}/{}", requested_latency_frames, config.sample_rate_hz);

    let adapter_props = build_pipewire_bridge_adapter_properties(&config, &requested_latency);
    let adapter_props_raw = adapter_props.into_raw();
    let factory_name = CString::new("adapter").expect("valid CString");
    let mut adapter_state = Box::new(PipewireBridgeAdapterState {
        hook: unsafe { std::mem::zeroed() },
        config: config.clone(),
        bound: Cell::new(false),
        removed: Cell::new(false),
        errored: Cell::new(false),
        global_id: Cell::new(u32::MAX),
        object_serial: RefCell::new(None),
        node_name: RefCell::new(None),
    });
    let adapter_state_ptr: *mut PipewireBridgeAdapterState = &mut *adapter_state;
    let adapter_proxy = unsafe {
        let object = pw_core_create_object_raw(
            core.as_raw_ptr(),
            factory_name.as_ptr(),
            pw::sys::PW_TYPE_INTERFACE_Node.as_ptr().cast(),
            pw::sys::PW_VERSION_NODE,
            &(*adapter_props_raw).dict,
            0,
        );
        pw::sys::pw_properties_free(adapter_props_raw);
        object.cast::<pw::sys::pw_proxy>()
    };
    if adapter_proxy.is_null() {
        anyhow::bail!("Failed to create PipeWire bridge adapter sink");
    }
    let adapter_proxy_events = pw::sys::pw_proxy_events {
        version: pw::sys::PW_VERSION_PROXY_EVENTS,
        destroy: Some(pipewire_bridge_adapter_proxy_destroy),
        bound: Some(pipewire_bridge_adapter_proxy_bound),
        removed: Some(pipewire_bridge_adapter_proxy_removed),
        done: Some(pipewire_bridge_adapter_proxy_done),
        error: Some(pipewire_bridge_adapter_proxy_error),
        bound_props: Some(pipewire_bridge_adapter_proxy_bound_props),
    };
    unsafe {
        pw::sys::pw_proxy_add_listener(
            adapter_proxy,
            &mut adapter_state.hook,
            &adapter_proxy_events,
            adapter_state_ptr.cast(),
        );
    }

    log::info!(
        "Publishing PipeWire bridge adapter sink: node={} description={} channels={} rate={}Hz latency={} factory.name=support.null-audio-sink iec958.codecs={} audio.position={} resample.disable=true",
        config.node_name,
        config.node_description,
        config.channels,
        config.sample_rate_hz,
        requested_latency,
        TRUEHD_ONLY_IEC958_CODECS_PROP,
        IEC958_AUDIO_POSITION_PROP
    );

    let start_wait = Instant::now();
    while !adapter_state.bound.get()
        && !adapter_state.errored.get()
        && !adapter_state.removed.get()
        && start_wait.elapsed() < Duration::from_secs(2)
        && !stop.load(Ordering::Relaxed)
    {
        let _ = mainloop.loop_().iterate(Duration::from_millis(50));
    }
    log::info!(
        "PipeWire bridge adapter sink ready check: node={} bound={} removed={} errored={} global_id={}",
        config.node_name,
        adapter_state.bound.get(),
        adapter_state.removed.get(),
        adapter_state.errored.get(),
        adapter_state.global_id.get()
    );
    let capture_target = wait_for_pipewire_bridge_adapter_target(
        &mainloop,
        &core,
        adapter_state.global_id.get(),
        &config,
        &stop,
    )?
    .or_else(|| adapter_state.node_name.borrow().clone())
    .or_else(|| adapter_state.object_serial.borrow().clone())
    .unwrap_or_else(|| config.node_name.clone());
    log::info!(
        "PipeWire bridge adapter capture target: node={} target.object={} target.id={}",
        config.node_name,
        capture_target,
        adapter_state.global_id.get()
    );

    let capture_props = build_pipewire_bridge_capture_stream_properties(&config, &capture_target);
    let capture_result = run_pipewire_bridge_capture_stream(
        &mainloop,
        &core,
        stop,
        input_control,
        config,
        ingest,
        None,
        capture_props,
        "omniphony-live-bridge-monitor-capture",
        "PipeWire bridge monitor capture",
    );

    unsafe {
        pw::sys::pw_proxy_destroy(adapter_proxy);
    }
    drop(adapter_state);
    capture_result
}

#[cfg(target_os = "linux")]
fn wait_for_pipewire_bridge_adapter_target(
    mainloop: &pw::main_loop::MainLoopRc,
    core: &pw::core::CoreRc,
    target_global_id: u32,
    config: &PipewireBridgeInputConfig,
    stop: &Arc<AtomicBool>,
) -> Result<Option<String>> {
    let registry = core
        .get_registry()
        .map_err(|e| anyhow!("Failed to get PipeWire registry for adapter target: {e:?}"))?;
    let target_found = Rc::new(Cell::new(false));
    let resolved_target = Rc::new(RefCell::new(None::<String>));
    let target_found_clone = Rc::clone(&target_found);
    let resolved_target_clone = Rc::clone(&resolved_target);
    let _registry_listener = registry
        .add_listener_local()
        .global(move |global| {
            if global.type_ != pw::types::ObjectType::Node {
                return;
            }
            let Some(props) = global.props.as_ref() else {
                return;
            };
            let node_name = props.get(*pw::keys::NODE_NAME).unwrap_or("<unnamed>");
            let media_class = props.get(*pw::keys::MEDIA_CLASS).unwrap_or("<unknown>");
            log::info!(
                "PipeWire bridge registry global: id={} node.name={} media.class={}",
                global.id,
                node_name,
                media_class
            );
            if global.id == target_global_id {
                target_found_clone.set(true);
                let target = props
                    .get(*pw::keys::NODE_NAME)
                    .map(str::to_owned)
                    .or_else(|| props.get("object.serial").map(str::to_owned));
                *resolved_target_clone.borrow_mut() = target.clone();
                log::info!(
                    "PipeWire bridge adapter target discovered in registry: requested_id={} node.id={} node.name={} object.serial={:?}",
                    target_global_id,
                    global.id,
                    node_name,
                    props.get("object.serial")
                );
            }
        })
        .register();

    let pending = core
        .sync(0)
        .map_err(|e| anyhow!("PipeWire sync failed while waiting for adapter monitor: {e:?}"))?;
    let sync_done = Rc::new(Cell::new(false));
    let sync_done_clone = Rc::clone(&sync_done);
    let loop_clone = mainloop.clone();
    let _core_listener = core
        .add_listener_local()
        .done(move |id, seq| {
            if id == pw::core::PW_ID_CORE && seq == pending {
                sync_done_clone.set(true);
                loop_clone.quit();
            }
        })
        .register();

    while !sync_done.get() && !stop.load(Ordering::Relaxed) {
        mainloop.run();
    }

    log::info!(
        "PipeWire bridge adapter target snapshot: requested_id={} found={} resolved_target={:?}",
        target_global_id,
        target_found.get(),
        resolved_target.borrow().as_deref()
    );

    let start_wait = Instant::now();
    while !target_found.get()
        && !stop.load(Ordering::Relaxed)
        && !sys::ShutdownHandle::is_requested()
        && !sys::ShutdownHandle::is_restart_from_config_requested()
        && start_wait.elapsed() < Duration::from_secs(2)
    {
        let _ = mainloop.loop_().iterate(Duration::from_millis(100));
    }

    log::info!(
        "PipeWire bridge adapter target ready check: node={} requested_id={} found={} resolved_target={:?}",
        config.node_name,
        target_global_id,
        target_found.get(),
        resolved_target.borrow().as_deref()
    );
    Ok(resolved_target.borrow().clone())
}

#[cfg(target_os = "linux")]
fn run_pipewire_bridge_exported_node_backend(
    input_control: Arc<InputControl>,
    config: PipewireBridgeInputConfig,
    stop: Arc<AtomicBool>,
    ingest: LiveBridgeIngestRuntime,
) -> Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| anyhow!("Failed to create PipeWire main loop: {e:?}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|e| anyhow!("Failed to create PipeWire context: {e:?}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| anyhow!("Failed to connect to PipeWire core: {e:?}"))?;

    let requested_latency_frames =
        ((config.target_latency_ms as u64 * config.sample_rate_hz as u64) / 1000).max(1) as u32;
    let requested_latency = format!("{}/{}", requested_latency_frames, config.sample_rate_hz);
    let props = build_pipewire_bridge_stream_properties(&config, &requested_latency);
    let props_raw = props.into_raw();

    let mut node = PipewireBridgeExportNode::new(input_control, config.clone(), stop, ingest)?;
    let proxy = unsafe {
        let proxy = pw::sys::pw_core_export(
            core.as_raw_ptr(),
            spa::sys::SPA_TYPE_INTERFACE_Node.as_ptr().cast(),
            &(*props_raw).dict,
            (&mut node.iface as *mut spa::sys::spa_node).cast(),
            0,
        );
        pw::sys::pw_properties_free(props_raw);
        proxy
    };
    if proxy.is_null() {
        anyhow::bail!("Failed to export PipeWire bridge input node");
    }

    log::info!(
        "Publishing PipeWire bridge exported sink: node={} description={} channels={} rate={}Hz latency={} iec958.codecs={} audio.position={} resample.disable=true",
        config.node_name,
        config.node_description,
        config.channels,
        config.sample_rate_hz,
        requested_latency,
        TRUEHD_ONLY_IEC958_CODECS_PROP,
        IEC958_AUDIO_POSITION_PROP
    );

    while !node.stop.load(Ordering::Relaxed)
        && !sys::ShutdownHandle::is_requested()
        && !sys::ShutdownHandle::is_restart_from_config_requested()
    {
        let _ = mainloop.loop_().iterate(Duration::from_millis(100));
    }

    unsafe {
        pw::sys::pw_proxy_destroy(proxy);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_pipewire_bridge_filter_backend(
    input_control: Arc<InputControl>,
    config: PipewireBridgeInputConfig,
    stop: Arc<AtomicBool>,
    ingest: LiveBridgeIngestRuntime,
) -> Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| anyhow!("Failed to create PipeWire main loop: {e:?}"))?;

    let requested_latency_frames =
        ((config.target_latency_ms as u64 * config.sample_rate_hz as u64) / 1000).max(1) as u32;
    let requested_latency = format!("{}/{}", requested_latency_frames, config.sample_rate_hz);
    let props = build_pipewire_bridge_stream_properties(&config, &requested_latency);

    let user_data = Box::new(PipewireBridgeFilterUserData {
        input_control,
        config: config.clone(),
        stop,
        ingest: RefCell::new(ingest),
        input_port: Cell::new(std::ptr::null_mut()),
        metrics: RefCell::new(BridgeCaptureUserData {
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
            accumulate_buf: Vec::new(),
            accumulate_count: 0,
            last_idle_trigger: Instant::now(),
            dynamic_trigger_interval: None,
            last_pw_time_log_at: Instant::now(),
            output_rate_adjust: Arc::new(AtomicU32::new(1.0f32.to_bits())),
        }),
    });
    let user_data_ptr = Box::into_raw(user_data);

    unsafe {
        let mut events: pw::sys::pw_filter_events = std::mem::zeroed();
        events.version = pw::sys::PW_VERSION_FILTER_EVENTS;
        events.state_changed = Some(pipewire_bridge_filter_state_changed);
        events.param_changed = Some(pipewire_bridge_filter_param_changed);
        events.process = Some(pipewire_bridge_filter_process);

        let filter = pw::sys::pw_filter_new_simple(
            mainloop.loop_().as_raw_ptr(),
            c"omniphony-live-bridge-input".as_ptr(),
            props.into_raw(),
            &events,
            user_data_ptr.cast(),
        );
        if filter.is_null() {
            let _ = Box::from_raw(user_data_ptr);
            anyhow::bail!("Failed to create PipeWire bridge input filter");
        }

        log::info!(
            "Publishing PipeWire bridge filter sink: node={} description={} channels={} rate={}Hz latency={} iec958.codecs={} audio.position={} resample.disable=true",
            config.node_name,
            config.node_description,
            config.channels,
            config.sample_rate_hz,
            requested_latency,
            TRUEHD_ONLY_IEC958_CODECS_PROP,
            IEC958_AUDIO_POSITION_PROP
        );

        let format_values =
            build_pipewire_bridge_format_pod(&config, spa::param::ParamType::EnumFormat)?;
        let format_pod = Pod::from_bytes(&format_values)
            .ok_or_else(|| anyhow!("Invalid PipeWire bridge format pod"))?;
        let mut port_params = [format_pod.as_raw_ptr().cast_const()];

        let port_props = {
            let mut props = pw::properties::PropertiesBox::new();
            props.insert("port.name", "input");
            props
        };

        let port_data = pw::sys::pw_filter_add_port(
            filter,
            spa::sys::SPA_DIRECTION_INPUT,
            pw::sys::pw_filter_port_flags_PW_FILTER_PORT_FLAG_MAP_BUFFERS,
            0,
            port_props.into_raw(),
            port_params.as_mut_ptr(),
            port_params.len() as u32,
        );
        if port_data.is_null() {
            pw::sys::pw_filter_destroy(filter);
            let _ = Box::from_raw(user_data_ptr);
            anyhow::bail!("Failed to add PipeWire bridge input filter port");
        }
        (*user_data_ptr).input_port.set(port_data);

        let connect_result = pw::sys::pw_filter_connect(
            filter,
            pw::sys::pw_filter_flags_PW_FILTER_FLAG_RT_PROCESS,
            std::ptr::null_mut(),
            0,
        );
        if connect_result < 0 {
            pw::sys::pw_filter_destroy(filter);
            let _ = Box::from_raw(user_data_ptr);
            anyhow::bail!(
                "Failed to connect PipeWire bridge input filter: {}",
                connect_result
            );
        }

        log::info!(
            "PipeWire bridge filter connected: node={} node_id={}",
            config.node_name,
            pw::sys::pw_filter_get_node_id(filter)
        );

        while !(*user_data_ptr).stop.load(Ordering::Relaxed)
            && !sys::ShutdownHandle::is_requested()
            && !sys::ShutdownHandle::is_restart_from_config_requested()
        {
            let _ = mainloop.loop_().iterate(Duration::from_millis(100));
        }

        let _ = pw::sys::pw_filter_disconnect(filter);
        pw::sys::pw_filter_destroy(filter);
        let _ = Box::from_raw(user_data_ptr);
    }

    Ok(())
}

#[cfg(target_os = "linux")]
unsafe fn pw_core_create_object_raw(
    core: *mut pw::sys::pw_core,
    factory_name: *const std::os::raw::c_char,
    type_name: *const std::os::raw::c_char,
    version: u32,
    props: *const spa::sys::spa_dict,
    user_data_size: usize,
) -> *mut c_void {
    let iface = core.cast::<spa::sys::spa_interface>();
    if iface.is_null() {
        return std::ptr::null_mut();
    }
    let methods = unsafe { (*(*iface).cb.funcs.cast::<pw::sys::pw_core_methods>()) };
    let Some(create_object) = methods.create_object else {
        return std::ptr::null_mut();
    };
    unsafe { create_object((*iface).cb.data, factory_name, type_name, version, props, user_data_size) }
}

#[cfg(target_os = "linux")]
unsafe fn pw_core_add_listener_raw(
    core: *mut pw::sys::pw_core,
    listener: *mut spa::sys::spa_hook,
    events: *const pw::sys::pw_core_events,
    data: *mut c_void,
) -> i32 {
    let iface = core.cast::<spa::sys::spa_interface>();
    if iface.is_null() {
        return -libc::EINVAL;
    }
    let methods = unsafe { (*(*iface).cb.funcs.cast::<pw::sys::pw_core_methods>()) };
    let Some(add_listener) = methods.add_listener else {
        return -libc::ENOTSUP;
    };
    unsafe { add_listener((*iface).cb.data, listener, events, data) }
}

#[cfg(target_os = "linux")]
unsafe fn pw_client_node_add_listener_raw(
    client_node: *mut pw::sys::pw_client_node,
    listener: *mut spa::sys::spa_hook,
    events: *const pw::sys::pw_client_node_events,
    data: *mut c_void,
) -> i32 {
    let iface = client_node.cast::<spa::sys::spa_interface>();
    let methods = unsafe { (*(*iface).cb.funcs.cast::<pw::sys::pw_client_node_methods>()) };
    let Some(add_listener) = methods.add_listener else {
        return -libc::ENOTSUP;
    };
    unsafe { add_listener((*iface).cb.data, listener, events, data) }
}

#[cfg(target_os = "linux")]
unsafe fn pw_client_node_get_node_raw(
    client_node: *mut pw::sys::pw_client_node,
    version: u32,
    user_data_size: usize,
) -> *mut pw::sys::pw_node {
    let iface = client_node.cast::<spa::sys::spa_interface>();
    let methods = unsafe { (*(*iface).cb.funcs.cast::<pw::sys::pw_client_node_methods>()) };
    let Some(get_node) = methods.get_node else {
        return std::ptr::null_mut();
    };
    unsafe { get_node((*iface).cb.data, version, user_data_size) }
}

#[cfg(target_os = "linux")]
unsafe fn pw_client_node_update_raw(
    client_node: *mut pw::sys::pw_client_node,
    change_mask: u32,
    n_params: u32,
    params: *mut *const spa::sys::spa_pod,
    info: *const spa::sys::spa_node_info,
) -> i32 {
    let iface = client_node.cast::<spa::sys::spa_interface>();
    let methods = unsafe { (*(*iface).cb.funcs.cast::<pw::sys::pw_client_node_methods>()) };
    let Some(update) = methods.update else {
        return -libc::ENOTSUP;
    };
    unsafe { update((*iface).cb.data, change_mask, n_params, params, info) }
}

#[cfg(target_os = "linux")]
unsafe fn pw_client_node_port_update_raw(
    client_node: *mut pw::sys::pw_client_node,
    direction: spa::sys::spa_direction,
    port_id: u32,
    change_mask: u32,
    n_params: u32,
    params: *mut *const spa::sys::spa_pod,
    info: *const spa::sys::spa_port_info,
) -> i32 {
    let iface = client_node.cast::<spa::sys::spa_interface>();
    let methods = unsafe { (*(*iface).cb.funcs.cast::<pw::sys::pw_client_node_methods>()) };
    let Some(port_update) = methods.port_update else {
        return -libc::ENOTSUP;
    };
    unsafe { port_update((*iface).cb.data, direction, port_id, change_mask, n_params, params, info) }
}

#[cfg(target_os = "linux")]
unsafe fn pw_client_node_set_active_raw(
    client_node: *mut pw::sys::pw_client_node,
    active: bool,
) -> i32 {
    let iface = client_node.cast::<spa::sys::spa_interface>();
    let methods = unsafe { (*(*iface).cb.funcs.cast::<pw::sys::pw_client_node_methods>()) };
    let Some(set_active) = methods.set_active else {
        return -libc::ENOTSUP;
    };
    unsafe { set_active((*iface).cb.data, active) }
}

#[cfg(target_os = "linux")]
unsafe fn pw_client_node_port_buffers_raw(
    client_node: *mut pw::sys::pw_client_node,
    direction: spa::sys::spa_direction,
    port_id: u32,
    mix_id: u32,
    n_buffers: u32,
    buffers: *mut *mut spa::sys::spa_buffer,
) -> i32 {
    let iface = client_node.cast::<spa::sys::spa_interface>();
    let methods = unsafe { (*(*iface).cb.funcs.cast::<pw::sys::pw_client_node_methods>()) };
    let Some(port_buffers) = methods.port_buffers else {
        return -libc::ENOTSUP;
    };
    unsafe { port_buffers((*iface).cb.data, direction, port_id, mix_id, n_buffers, buffers) }
}

#[cfg(target_os = "linux")]
unsafe fn pw_node_add_listener_raw(
    node: *mut pw::sys::pw_node,
    listener: *mut spa::sys::spa_hook,
    events: *const pw::sys::pw_node_events,
    data: *mut c_void,
) -> i32 {
    let iface = node.cast::<spa::sys::spa_interface>();
    let methods = unsafe { *(*iface).cb.funcs.cast::<pw::sys::pw_node_methods>() };
    let Some(add_listener) = methods.add_listener else {
        return -libc::ENOTSUP;
    };
    unsafe { add_listener((*iface).cb.data, listener, events, data) }
}

#[cfg(target_os = "linux")]
unsafe fn pw_node_subscribe_params_raw(
    node: *mut pw::sys::pw_node,
    ids: *mut u32,
    n_ids: u32,
) -> i32 {
    let iface = node.cast::<spa::sys::spa_interface>();
    let methods = unsafe { *(*iface).cb.funcs.cast::<pw::sys::pw_node_methods>() };
    let Some(subscribe_params) = methods.subscribe_params else {
        return -libc::ENOTSUP;
    };
    unsafe { subscribe_params((*iface).cb.data, ids, n_ids) }
}

#[cfg(target_os = "linux")]
fn run_pipewire_bridge_client_node_backend(
    input_control: Arc<InputControl>,
    config: PipewireBridgeInputConfig,
    stop: Arc<AtomicBool>,
    ingest: LiveBridgeIngestRuntime,
) -> Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| anyhow!("Failed to create PipeWire main loop: {e:?}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|e| anyhow!("Failed to create PipeWire context: {e:?}"))?;
    let core = context
        .connect_rc(None)
        .map_err(|e| anyhow!("Failed to connect to PipeWire core: {e:?}"))?;

    let requested_latency_frames =
        ((config.target_latency_ms as u64 * config.sample_rate_hz as u64) / 1000).max(1) as u32;
    let requested_latency = format!("{}/{}", requested_latency_frames, config.sample_rate_hz);
    let node_name_value = CString::new(config.node_name.clone()).expect("valid CString");
    let node_description_value =
        CString::new(config.node_description.clone()).expect("valid CString");
    let media_name_value =
        CString::new(config.node_description.clone()).expect("valid CString");
    let port_group_value = CString::new("stream.0").expect("valid CString");
    let port_name_value = CString::new("playback_FL").expect("valid CString");
    let port_alias_value = CString::new(format!("{}:playback_FL", config.node_description))
        .expect("valid CString");
    let port_audio_channel_value = CString::new("FL").expect("valid CString");
    let props = build_pipewire_bridge_stream_properties(&config, &requested_latency);
    let props_raw = props.into_raw();

    let mut state = Box::new(PipewireBridgeClientNodeState {
        hook: unsafe { std::mem::zeroed() },
        core_hook: unsafe { std::mem::zeroed() },
        proxy_hook: unsafe { std::mem::zeroed() },
        node_hook: unsafe { std::mem::zeroed() },
        client_node: std::ptr::null_mut(),
        input_control,
        config: config.clone(),
        stop,
        ingest: RefCell::new(ingest),
        node_info: spa::sys::spa_node_info {
            max_input_ports: 1,
            max_output_ports: 0,
            change_mask: (spa::sys::SPA_NODE_CHANGE_MASK_FLAGS
                | spa::sys::SPA_NODE_CHANGE_MASK_PARAMS) as u64,
            flags: (spa::sys::SPA_NODE_FLAG_RT | spa::sys::SPA_NODE_FLAG_NEED_CONFIGURE) as u64,
            props: std::ptr::null_mut(),
            params: std::ptr::null_mut(),
            n_params: 0,
        },
        port_info: spa::sys::spa_port_info {
            change_mask: (spa::sys::SPA_PORT_CHANGE_MASK_FLAGS
                | spa::sys::SPA_PORT_CHANGE_MASK_PROPS
                | spa::sys::SPA_PORT_CHANGE_MASK_PARAMS) as u64,
            flags: (spa::sys::SPA_PORT_FLAG_CAN_ALLOC_BUFFERS
                | spa::sys::SPA_PORT_FLAG_NO_REF
                | spa::sys::SPA_PORT_FLAG_TERMINAL) as u64,
            rate: spa::sys::spa_fraction { num: 0, denom: 1 },
            props: std::ptr::null_mut(),
            params: std::ptr::null_mut(),
            n_params: 0,
        },
        node_params: [
            spa_param_info(spa::sys::SPA_PARAM_Props, spa::sys::SPA_PARAM_INFO_READWRITE),
            spa_param_info(spa::sys::SPA_PARAM_EnumFormat, spa::sys::SPA_PARAM_INFO_READ),
            spa_param_info(spa::sys::SPA_PARAM_Format, spa::sys::SPA_PARAM_INFO_WRITE),
            spa_param_info(
                spa::sys::SPA_PARAM_EnumPortConfig,
                spa::sys::SPA_PARAM_INFO_READ,
            ),
            spa_param_info(
                spa::sys::SPA_PARAM_PortConfig,
                spa::sys::SPA_PARAM_INFO_READWRITE,
            ),
            spa_param_info(spa::sys::SPA_PARAM_Latency, spa::sys::SPA_PARAM_INFO_READWRITE),
            spa_param_info(
                spa::sys::SPA_PARAM_ProcessLatency,
                spa::sys::SPA_PARAM_INFO_READWRITE,
            ),
            spa_param_info(spa::sys::SPA_PARAM_Tag, spa::sys::SPA_PARAM_INFO_READWRITE),
        ],
        node_props: spa::sys::spa_dict {
            flags: 0,
            n_items: 7,
            items: std::ptr::null(),
        },
        node_prop_items: [
            spa::sys::spa_dict_item {
                key: pw::keys::NODE_NAME.as_ptr().cast(),
                value: node_name_value.as_ptr(),
            },
            spa::sys::spa_dict_item {
                key: pw::keys::NODE_DESCRIPTION.as_ptr().cast(),
                value: node_description_value.as_ptr(),
            },
            spa::sys::spa_dict_item {
                key: pw::keys::MEDIA_NAME.as_ptr().cast(),
                value: media_name_value.as_ptr(),
            },
            spa::sys::spa_dict_item {
                key: pw::keys::MEDIA_CLASS.as_ptr().cast(),
                value: b"Audio/Sink\0".as_ptr().cast(),
            },
            spa::sys::spa_dict_item {
                key: pw::keys::MEDIA_TYPE.as_ptr().cast(),
                value: b"Audio\0".as_ptr().cast(),
            },
            spa::sys::spa_dict_item {
                key: pw::keys::MEDIA_CATEGORY.as_ptr().cast(),
                value: b"Playback\0".as_ptr().cast(),
            },
            spa::sys::spa_dict_item {
                key: pw::keys::MEDIA_ROLE.as_ptr().cast(),
                value: b"Movie\0".as_ptr().cast(),
            },
        ],
        port_params: [
            spa_param_info(spa::sys::SPA_PARAM_EnumFormat, spa::sys::SPA_PARAM_INFO_READ),
            spa_param_info(spa::sys::SPA_PARAM_Meta, spa::sys::SPA_PARAM_INFO_READ),
            spa_param_info(spa::sys::SPA_PARAM_IO, spa::sys::SPA_PARAM_INFO_READ),
            spa_param_info(spa::sys::SPA_PARAM_Format, spa::sys::SPA_PARAM_INFO_WRITE),
            spa_param_info(spa::sys::SPA_PARAM_Buffers, 0),
            spa_param_info(spa::sys::SPA_PARAM_Latency, spa::sys::SPA_PARAM_INFO_READWRITE),
            spa_param_info(spa::sys::SPA_PARAM_Tag, spa::sys::SPA_PARAM_INFO_READWRITE),
        ],
        port_props: spa::sys::spa_dict {
            flags: 0,
            n_items: 5,
            items: std::ptr::null(),
        },
        port_prop_items: [
            spa::sys::spa_dict_item {
                key: b"format.dsp\0".as_ptr().cast(),
                value: b"32 bit float mono audio\0".as_ptr().cast(),
            },
            spa::sys::spa_dict_item {
                key: b"port.group\0".as_ptr().cast(),
                value: port_group_value.as_ptr(),
            },
            spa::sys::spa_dict_item {
                key: pw::keys::PORT_NAME.as_ptr().cast(),
                value: port_name_value.as_ptr(),
            },
            spa::sys::spa_dict_item {
                key: pw::keys::PORT_ALIAS.as_ptr().cast(),
                value: port_alias_value.as_ptr(),
            },
            spa::sys::spa_dict_item {
                key: b"audio.channel\0".as_ptr().cast(),
                value: port_audio_channel_value.as_ptr(),
            },
        ],
        node_name_value,
        node_description_value,
        media_name_value,
        port_group_value,
        port_name_value,
        port_alias_value,
        port_audio_channel_value,
        enum_port_config_bytes: build_pipewire_bridge_enum_port_config_pod()?,
        port_config_bytes: build_pipewire_bridge_port_config_pod()?,
        enum_format_bytes: build_pipewire_bridge_format_pod(
            &config,
            spa::param::ParamType::EnumFormat,
        )?,
        format_bytes: build_pipewire_bridge_format_pod(
            &config,
            spa::param::ParamType::Format,
        )?,
        props_param_bytes: build_pipewire_bridge_props_pod()?,
        meta_param_bytes: build_pipewire_bridge_meta_pod()?,
        latency_param_bytes: build_pipewire_bridge_latency_pod()?,
        process_latency_param_bytes: build_pipewire_bridge_process_latency_pod()?,
        tag_param_bytes: build_pipewire_bridge_tag_pod(spa::sys::SPA_DIRECTION_INPUT)?,
        buffers_param_bytes: build_pipewire_bridge_buffers_pod(&config)?,
        io_buffers_param_bytes: build_pipewire_bridge_io_buffers_pod()?,
        mapped_mems: Vec::new(),
        transport_ptr: std::ptr::null_mut(),
        transport_mem_id: None,
        transport_size: 0,
        activation_ptr: std::ptr::null_mut(),
        activation_mem_id: None,
        activation_size: 0,
        io_clock: std::ptr::null_mut(),
        io_position: std::ptr::null_mut(),
        format_configured: false,
    });
    state.node_info.params = state.node_params.as_mut_ptr();
    state.node_info.n_params = state.node_params.len() as u32;
    state.node_props.items = state.node_prop_items.as_ptr();
    state.node_info.props = (&mut state.node_props as *mut spa::sys::spa_dict).cast();
    state.port_props.items = state.port_prop_items.as_ptr();
    state.port_info.props = (&mut state.port_props as *mut spa::sys::spa_dict).cast();
    state.port_info.params = state.port_params.as_mut_ptr();
    state.port_info.n_params = state.port_params.len() as u32;

    let factory_name = CString::new("client-node").expect("valid CString");
    let state_ptr: *mut PipewireBridgeClientNodeState = &mut *state;
    let client_node = unsafe {
        let object = pw_core_create_object_raw(
            core.as_raw_ptr(),
            factory_name.as_ptr(),
            pw::sys::PW_TYPE_INTERFACE_ClientNode.as_ptr().cast(),
            pw::sys::PW_VERSION_CLIENT_NODE,
            &(*props_raw).dict,
            0,
        );
        pw::sys::pw_properties_free(props_raw);
        object.cast::<pw::sys::pw_client_node>()
    };
    if client_node.is_null() {
        anyhow::bail!("Failed to create PipeWire client-node bridge input object");
    }
    state.client_node = client_node;

    let mut port_param_ptrs = [
        state.enum_format_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state.meta_param_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state.io_buffers_param_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state.format_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state.buffers_param_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state.latency_param_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state.tag_param_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
    ];

    let client_node_events = pw::sys::pw_client_node_events {
        version: pw::sys::PW_VERSION_CLIENT_NODE_EVENTS,
        transport: Some(pipewire_bridge_client_node_transport),
        set_param: Some(pipewire_bridge_client_node_set_param),
        set_io: Some(pipewire_bridge_client_node_set_io),
        event: Some(pipewire_bridge_client_node_event),
        command: Some(pipewire_bridge_client_node_command),
        add_port: Some(pipewire_bridge_client_node_add_port),
        remove_port: Some(pipewire_bridge_client_node_remove_port),
        port_set_param: Some(pipewire_bridge_client_node_port_set_param),
        port_use_buffers: Some(pipewire_bridge_client_node_port_use_buffers),
        port_set_io: Some(pipewire_bridge_client_node_port_set_io),
        set_activation: Some(pipewire_bridge_client_node_set_activation),
        port_set_mix_info: Some(pipewire_bridge_client_node_port_set_mix_info),
    };
    let core_events = pw::sys::pw_core_events {
        version: pw::sys::PW_VERSION_CORE_EVENTS,
        info: Some(pipewire_bridge_client_node_core_info),
        done: Some(pipewire_bridge_client_node_core_done),
        ping: Some(pipewire_bridge_client_node_core_ping),
        error: Some(pipewire_bridge_client_node_core_error),
        remove_id: Some(pipewire_bridge_client_node_core_remove_id),
        bound_id: Some(pipewire_bridge_client_node_core_bound_id),
        add_mem: Some(pipewire_bridge_client_node_core_add_mem),
        remove_mem: Some(pipewire_bridge_client_node_core_remove_mem),
        bound_props: Some(pipewire_bridge_client_node_core_bound_props),
    };
    let proxy_events = pw::sys::pw_proxy_events {
        version: pw::sys::PW_VERSION_PROXY_EVENTS,
        destroy: Some(pipewire_bridge_client_node_proxy_destroy),
        bound: Some(pipewire_bridge_client_node_proxy_bound),
        removed: Some(pipewire_bridge_client_node_proxy_removed),
        done: Some(pipewire_bridge_client_node_proxy_done),
        error: Some(pipewire_bridge_client_node_proxy_error),
        bound_props: Some(pipewire_bridge_client_node_proxy_bound_props),
    };
    let node_events = pw::sys::pw_node_events {
        version: pw::sys::PW_VERSION_NODE_EVENTS,
        info: Some(pipewire_bridge_client_node_node_info),
        param: Some(pipewire_bridge_client_node_node_param),
    };

    let add_listener_res = unsafe {
        pw_client_node_add_listener_raw(
            client_node,
            &mut state.hook,
            &client_node_events,
            state_ptr.cast(),
        )
    };
    if add_listener_res < 0 {
        unsafe { pw::sys::pw_proxy_destroy(client_node.cast()) };
        anyhow::bail!("Failed to add PipeWire client-node listener: {}", add_listener_res);
    }
    let core_listener_res = unsafe {
        pw_core_add_listener_raw(
            core.as_raw_ptr(),
            &mut state.core_hook,
            &core_events,
            state_ptr.cast(),
        )
    };
    if core_listener_res < 0 {
        unsafe { pw::sys::pw_proxy_destroy(client_node.cast()) };
        anyhow::bail!(
            "Failed to add PipeWire core listener for client-node bridge: {}",
            core_listener_res
        );
    }
    unsafe {
        pw::sys::pw_proxy_add_listener(
            client_node.cast(),
            &mut state.proxy_hook,
            &proxy_events,
            state_ptr.cast(),
        );
    }

    let pw_node = unsafe {
        pw_client_node_get_node_raw(client_node, pw::sys::PW_VERSION_NODE, 0)
    };
    if !pw_node.is_null() {
        let node_listener_res = unsafe {
            pw_node_add_listener_raw(pw_node, &mut state.node_hook, &node_events, state_ptr.cast())
        };
        log::info!(
            "PipeWire bridge client-node node_add_listener: node={} result={} pw_node={:p}",
            config.node_name,
            node_listener_res,
            pw_node
        );
    }
    log::info!(
        "Publishing PipeWire bridge client-node sink: node={} description={} channels={} rate={}Hz latency={} iec958.codecs={} audio.position={} resample.disable=true client_node={:p} pw_node={:p}",
        config.node_name,
        config.node_description,
        config.channels,
        config.sample_rate_hz,
        requested_latency,
        TRUEHD_ONLY_IEC958_CODECS_PROP,
        IEC958_AUDIO_POSITION_PROP,
        client_node,
        pw_node
    );

    let mut node_param_ptrs = [
        state.props_param_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state.enum_format_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state.format_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state
            .enum_port_config_bytes
            .as_ptr()
            .cast::<spa::sys::spa_pod>(),
        state.port_config_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state.latency_param_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state
            .process_latency_param_bytes
            .as_ptr()
            .cast::<spa::sys::spa_pod>(),
        state.tag_param_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
    ];
    let update_res = unsafe {
        pw_client_node_update_raw(
            client_node,
            pw::sys::PW_CLIENT_NODE_UPDATE_INFO | pw::sys::PW_CLIENT_NODE_UPDATE_PARAMS,
            node_param_ptrs.len() as u32,
            node_param_ptrs.as_mut_ptr(),
            &state.node_info,
        )
    };
    log::info!(
        "PipeWire bridge client-node update: node={} result={}",
        config.node_name,
        update_res
    );

    let port_update_res = unsafe {
        pw_client_node_port_update_raw(
            client_node,
            spa::sys::SPA_DIRECTION_INPUT,
            0,
            pw::sys::PW_CLIENT_NODE_PORT_UPDATE_PARAMS | pw::sys::PW_CLIENT_NODE_PORT_UPDATE_INFO,
            port_param_ptrs.len() as u32,
            port_param_ptrs.as_mut_ptr(),
            &state.port_info,
        )
    };
    log::info!(
        "PipeWire bridge client-node port_update: node={} result={}",
        config.node_name,
        port_update_res
    );

    let active_res = unsafe { pw_client_node_set_active_raw(client_node, true) };
    log::info!(
        "PipeWire bridge client-node set_active: node={} result={}",
        config.node_name,
        active_res
    );

    while !state.stop.load(Ordering::Relaxed)
        && !sys::ShutdownHandle::is_requested()
        && !sys::ShutdownHandle::is_restart_from_config_requested()
    {
        let _ = mainloop.loop_().iterate(Duration::from_millis(100));
    }

    unsafe {
        let _ = pw_client_node_set_active_raw(client_node, false);
        pipewire_bridge_client_node_cleanup_mapped_mems(&mut state);
        pw::sys::pw_proxy_destroy(client_node.cast());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn pipewire_bridge_client_node_refresh_configured_state(
    state: &mut PipewireBridgeClientNodeState,
) -> i32 {
    state.node_info.change_mask = (spa::sys::SPA_NODE_CHANGE_MASK_FLAGS
        | spa::sys::SPA_NODE_CHANGE_MASK_PROPS
        | spa::sys::SPA_NODE_CHANGE_MASK_PARAMS) as u64;
    state.node_info.flags = if state.format_configured {
        spa::sys::SPA_NODE_FLAG_RT as u64
    } else {
        (spa::sys::SPA_NODE_FLAG_RT | spa::sys::SPA_NODE_FLAG_NEED_CONFIGURE) as u64
    };
    state.port_info.change_mask = (spa::sys::SPA_PORT_CHANGE_MASK_FLAGS
        | spa::sys::SPA_PORT_CHANGE_MASK_PROPS
        | spa::sys::SPA_PORT_CHANGE_MASK_PARAMS) as u64;

    let mut node_param_ptrs = [
        state.props_param_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state.enum_format_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state.format_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state.enum_port_config_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state.port_config_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state.latency_param_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state
            .process_latency_param_bytes
            .as_ptr()
            .cast::<spa::sys::spa_pod>(),
        state.tag_param_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
    ];
    let node_res = unsafe {
        pw_client_node_update_raw(
            state.client_node,
            pw::sys::PW_CLIENT_NODE_UPDATE_INFO | pw::sys::PW_CLIENT_NODE_UPDATE_PARAMS,
            node_param_ptrs.len() as u32,
            node_param_ptrs.as_mut_ptr(),
            &state.node_info,
        )
    };

    let mut port_param_ptrs = [
        state.enum_format_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state.meta_param_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state.io_buffers_param_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state.format_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state.buffers_param_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state.latency_param_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state.tag_param_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
    ];
    let port_res = unsafe {
        pw_client_node_port_update_raw(
            state.client_node,
            spa::sys::SPA_DIRECTION_INPUT,
            0,
            pw::sys::PW_CLIENT_NODE_PORT_UPDATE_INFO | pw::sys::PW_CLIENT_NODE_PORT_UPDATE_PARAMS,
            port_param_ptrs.len() as u32,
            port_param_ptrs.as_mut_ptr(),
            &state.port_info,
        )
    };
    log::info!(
        "PipeWire bridge client-node refresh configured state: node={} configured={} update_res={} port_update_res={}",
        state.config.node_name,
        state.format_configured,
        node_res,
        port_res
    );
    if node_res < 0 { node_res } else { port_res }
}

#[cfg(target_os = "linux")]
fn pipewire_bridge_client_node_find_mem(
    state: &PipewireBridgeClientNodeState,
    mem_id: u32,
) -> Option<&PipewireBridgeMappedMem> {
    state.mapped_mems.iter().find(|mem| mem.id == mem_id)
}

#[cfg(target_os = "linux")]
fn pipewire_bridge_client_node_map_mem_slice(
    state: &PipewireBridgeClientNodeState,
    mem_id: u32,
    offset: u32,
    size: u32,
) -> Option<*mut c_void> {
    let mem = pipewire_bridge_client_node_find_mem(state, mem_id)?;
    let end = offset as usize + size as usize;
    if end > mem.size {
        return None;
    }
    Some(unsafe { (mem.ptr.cast::<u8>()).add(offset as usize).cast::<c_void>() })
}

#[cfg(target_os = "linux")]
fn pipewire_bridge_client_node_cleanup_mapped_mems(state: &mut PipewireBridgeClientNodeState) {
    for mem in state.mapped_mems.drain(..) {
        if !mem.ptr.is_null() && mem.size > 0 {
            unsafe {
                libc::munmap(mem.ptr, mem.size);
            }
        }
        if mem.fd >= 0 {
            unsafe {
                libc::close(mem.fd);
            }
        }
    }
    state.transport_ptr = std::ptr::null_mut();
    state.activation_ptr = std::ptr::null_mut();
    state.io_clock = std::ptr::null_mut();
    state.io_position = std::ptr::null_mut();
}

#[cfg(target_os = "linux")]
fn pipewire_bridge_client_node_mark_activation_ready(state: &mut PipewireBridgeClientNodeState) {
    const PW_VERSION_NODE_ACTIVATION: u32 = 1;
    const PW_NODE_ACTIVATION_FINISHED: u32 = 3;
    const PW_NODE_ACTIVATION_INACTIVE: u32 = 4;

    if state.activation_ptr.is_null()
        || state.activation_size < std::mem::size_of::<PipewireBridgePwNodeActivation>() as u32
    {
        return;
    }

    let activation =
        unsafe { &mut *(state.activation_ptr.cast::<PipewireBridgePwNodeActivation>()) };

    unsafe {
        std::ptr::write_volatile(&mut activation.client_version, PW_VERSION_NODE_ACTIVATION);
        std::ptr::write_volatile(&mut activation.command, 0);
        std::ptr::write_volatile(&mut activation.status, PW_NODE_ACTIVATION_FINISHED);
        std::ptr::write_volatile(&mut activation.state[0].pending, 0);
        std::ptr::write_volatile(&mut activation.state[1].pending, 0);
        std::ptr::write_volatile(&mut activation.state[0].status, PW_NODE_ACTIVATION_INACTIVE);
        std::ptr::write_volatile(&mut activation.state[1].status, PW_NODE_ACTIVATION_FINISHED);
    }

    log::info!(
        "PipeWire bridge client-node activation ready: node={} activation={:p} client_version={} server_version={} status={} state0={} state1={}",
        state.config.node_name,
        state.activation_ptr,
        activation.client_version,
        activation.server_version,
        activation.status,
        activation.state[0].status,
        activation.state[1].status
    );
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_core_info(
    _data: *mut c_void,
    _info: *const pw::sys::pw_core_info,
) {
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_core_done(
    _data: *mut c_void,
    _id: u32,
    _seq: i32,
) {
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_core_ping(
    _data: *mut c_void,
    _id: u32,
    _seq: i32,
) {
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_core_error(
    data: *mut c_void,
    id: u32,
    seq: i32,
    res: i32,
    message: *const i8,
) {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    let message = if message.is_null() {
        "<null>"
    } else {
        unsafe { CStr::from_ptr(message) }.to_str().unwrap_or("<invalid utf8>")
    };
    log::warn!(
        "PipeWire bridge client-node core error: node={} id={} seq={} res={} message={}",
        state.config.node_name,
        id,
        seq,
        res,
        message
    );
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_core_remove_id(
    _data: *mut c_void,
    _id: u32,
) {
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_core_bound_id(
    _data: *mut c_void,
    _id: u32,
    _global_id: u32,
) {
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_core_add_mem(
    data: *mut c_void,
    id: u32,
    mem_type: u32,
    fd: i32,
    flags: u32,
) {
    let state = unsafe { &mut *(data as *mut PipewireBridgeClientNodeState) };
    let mut stat = MaybeUninit::<libc::stat>::zeroed();
    let stat_res = unsafe { libc::fstat(fd, stat.as_mut_ptr()) };
    if stat_res < 0 {
        log::warn!(
            "PipeWire bridge client-node core add_mem: node={} id={} type={} fd={} flags={} fstat_errno={}",
            state.config.node_name,
            id,
            mem_type,
            fd,
            flags,
            std::io::Error::last_os_error()
        );
        return;
    }
    let stat = unsafe { stat.assume_init() };
    let size = stat.st_size.max(0) as usize;
    if size == 0 {
        log::warn!(
            "PipeWire bridge client-node core add_mem: node={} id={} type={} fd={} flags={} size=0",
            state.config.node_name,
            id,
            mem_type,
            fd,
            flags
        );
        return;
    }
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED,
            fd,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        log::warn!(
            "PipeWire bridge client-node core add_mem: node={} id={} type={} fd={} flags={} mmap_errno={}",
            state.config.node_name,
            id,
            mem_type,
            fd,
            flags,
            std::io::Error::last_os_error()
        );
        return;
    }
    if let Some(existing_idx) = state.mapped_mems.iter().position(|mem| mem.id == id) {
        let existing = state.mapped_mems.remove(existing_idx);
        if !existing.ptr.is_null() && existing.size > 0 {
            unsafe {
                libc::munmap(existing.ptr, existing.size);
            }
        }
        if existing.fd >= 0 {
            unsafe {
                libc::close(existing.fd);
            }
        }
    }
    state.mapped_mems.push(PipewireBridgeMappedMem {
        id,
        mem_type,
        flags,
        fd,
        ptr,
        size,
    });
    log::info!(
        "PipeWire bridge client-node core add_mem: node={} id={} type={} fd={} flags={} size={} ptr={:p}",
        state.config.node_name,
        id,
        mem_type,
        fd,
        flags,
        size,
        ptr
    );
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_core_remove_mem(
    data: *mut c_void,
    id: u32,
) {
    let state = unsafe { &mut *(data as *mut PipewireBridgeClientNodeState) };
    if let Some(existing_idx) = state.mapped_mems.iter().position(|mem| mem.id == id) {
        let mem = state.mapped_mems.remove(existing_idx);
        if !mem.ptr.is_null() && mem.size > 0 {
            unsafe {
                libc::munmap(mem.ptr, mem.size);
            }
        }
        if mem.fd >= 0 {
            unsafe {
                libc::close(mem.fd);
            }
        }
    }
    if state.transport_mem_id == Some(id) {
        state.transport_mem_id = None;
        state.transport_ptr = std::ptr::null_mut();
        state.transport_size = 0;
    }
    if state.activation_mem_id == Some(id) {
        state.activation_mem_id = None;
        state.activation_ptr = std::ptr::null_mut();
        state.activation_size = 0;
    }
    log::info!(
        "PipeWire bridge client-node core remove_mem: node={} id={}",
        state.config.node_name,
        id
    );
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_core_bound_props(
    _data: *mut c_void,
    _id: u32,
    _global_id: u32,
    _props: *const spa::sys::spa_dict,
) {
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_transport(
    data: *mut c_void,
    readfd: i32,
    writefd: i32,
    mem_id: u32,
    offset: u32,
    size: u32,
) -> i32 {
    let state = unsafe { &mut *(data as *mut PipewireBridgeClientNodeState) };
    state.transport_mem_id = Some(mem_id);
    state.transport_size = size;
    state.transport_ptr =
        pipewire_bridge_client_node_map_mem_slice(state, mem_id, offset, size).unwrap_or_else(
            || {
                log::warn!(
                    "PipeWire bridge client-node transport map unresolved: node={} mem_id={} offset={} size={}",
                    state.config.node_name,
                    mem_id,
                    offset,
                    size
                );
                std::ptr::null_mut()
            },
        );
    log::info!(
        "PipeWire bridge client-node transport: node={} readfd={} writefd={} mem_id={} offset={} size={} ptr={:p}",
        state.config.node_name,
        readfd,
        writefd,
        mem_id,
        offset,
        size,
        state.transport_ptr
    );
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_set_param(
    data: *mut c_void,
    id: u32,
    flags: u32,
    param: *const spa::sys::spa_pod,
) -> i32 {
    let state = unsafe { &mut *(data as *mut PipewireBridgeClientNodeState) };
    log::info!(
        "PipeWire bridge client-node set_param: node={} id={} flags={} param_null={}",
        state.config.node_name,
        id,
        flags,
        param.is_null()
    );
    match id {
        x if x == spa::sys::SPA_PARAM_Format || x == spa::sys::SPA_PARAM_PortConfig => {
            if x == spa::sys::SPA_PARAM_Format {
                if let Some(bytes) = clone_spa_pod_bytes(param) {
                    state.format_bytes = bytes;
                }
            } else if let Some(bytes) = clone_spa_pod_bytes(param) {
                state.port_config_bytes = bytes;
            }
            state.format_configured = true;
            let refresh_res = pipewire_bridge_client_node_refresh_configured_state(state);
            log::info!(
                "PipeWire bridge client-node accepted node-level config: node={} id={} configured={} refresh_res={}",
                state.config.node_name,
                id,
                state.format_configured,
                refresh_res
            );
            refresh_res
        }
        _ => 0,
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_set_io(
    data: *mut c_void,
    id: u32,
    mem_id: u32,
    offset: u32,
    size: u32,
) -> i32 {
    let state = unsafe { &mut *(data as *mut PipewireBridgeClientNodeState) };
    let ptr = pipewire_bridge_client_node_map_mem_slice(state, mem_id, offset, size).unwrap_or_else(
        || {
            log::warn!(
                "PipeWire bridge client-node set_io unresolved mem: node={} id={} mem_id={} offset={} size={}",
                state.config.node_name,
                id,
                mem_id,
                offset,
                size
            );
            std::ptr::null_mut()
        },
    );
    match id {
        x if x == spa::sys::SPA_IO_Clock => {
            state.io_clock = ptr.cast::<spa::sys::spa_io_clock>();
        }
        x if x == spa::sys::SPA_IO_Position => {
            state.io_position = ptr.cast::<spa::sys::spa_io_position>();
        }
        _ => {}
    }
    pipewire_bridge_client_node_mark_activation_ready(state);
    log::info!(
        "PipeWire bridge client-node set_io: node={} id={} mem_id={} offset={} size={} ptr={:p}",
        state.config.node_name,
        id,
        mem_id,
        offset,
        size,
        ptr
    );
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_event(
    data: *mut c_void,
    event: *const spa::sys::spa_event,
) -> i32 {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::info!(
        "PipeWire bridge client-node event: node={} event={:p}",
        state.config.node_name,
        event
    );
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_command(
    data: *mut c_void,
    command: *const spa::sys::spa_command,
) -> i32 {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::info!(
        "PipeWire bridge client-node command: node={} command={:p}",
        state.config.node_name,
        command
    );
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_add_port(
    data: *mut c_void,
    direction: spa::sys::spa_direction,
    port_id: u32,
    props: *const spa::sys::spa_dict,
) -> i32 {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::info!(
        "PipeWire bridge client-node add_port: node={} direction={:?} port_id={} props={:p}",
        state.config.node_name,
        direction,
        port_id,
        props
    );
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_remove_port(
    data: *mut c_void,
    direction: spa::sys::spa_direction,
    port_id: u32,
) -> i32 {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::info!(
        "PipeWire bridge client-node remove_port: node={} direction={:?} port_id={}",
        state.config.node_name,
        direction,
        port_id
    );
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_port_set_param(
    data: *mut c_void,
    direction: spa::sys::spa_direction,
    port_id: u32,
    id: u32,
    flags: u32,
    param: *const spa::sys::spa_pod,
) -> i32 {
    let state = unsafe { &mut *(data as *mut PipewireBridgeClientNodeState) };
    log::info!(
        "PipeWire bridge client-node port_set_param: node={} direction={:?} port_id={} id={} flags={} param_null={}",
        state.config.node_name,
        direction,
        port_id,
        id,
        flags,
        param.is_null()
    );
    if direction != spa::sys::SPA_DIRECTION_INPUT || port_id != 0 {
        return 0;
    }
    if id != spa::sys::SPA_PARAM_Format {
        return 0;
    }
    if let Some(bytes) = clone_spa_pod_bytes(param) {
        state.format_bytes = bytes;
    }
    state.format_configured = true;
    let refresh_res = pipewire_bridge_client_node_refresh_configured_state(state);
    log::info!(
        "PipeWire bridge client-node accepted port format: node={} port_id={} configured={} refresh_res={}",
        state.config.node_name,
        port_id,
        state.format_configured,
        refresh_res
    );
    refresh_res
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_port_use_buffers(
    data: *mut c_void,
    direction: spa::sys::spa_direction,
    port_id: u32,
    mix_id: u32,
    flags: u32,
    n_buffers: u32,
    buffers: *mut pw::sys::pw_client_node_buffer,
) -> i32 {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::info!(
        "PipeWire bridge client-node port_use_buffers: node={} direction={:?} port_id={} mix_id={} flags={} n_buffers={} buffers={:p}",
        state.config.node_name,
        direction,
        port_id,
        mix_id,
        flags,
        n_buffers,
        buffers
    );
    if !buffers.is_null() && n_buffers > 0 {
        for idx in 0..(n_buffers as usize).min(8) {
            let client_buf = unsafe { &*buffers.add(idx) };
            let spa_buf = client_buf.buffer;
            if spa_buf.is_null() {
                log::info!(
                    "PipeWire bridge client-node port_use_buffers[{}]: spa_buffer=NULL",
                    idx
                );
                continue;
            }
            let spa_buf_ref = unsafe { &*spa_buf };
            let datas_ptr = spa_buf_ref.datas;
            let n_datas = spa_buf_ref.n_datas;
            log::info!(
                "PipeWire bridge client-node port_use_buffers[{}]: spa_buffer={:p} n_datas={} datas={:p}",
                idx,
                spa_buf,
                n_datas,
                datas_ptr
            );
            if !datas_ptr.is_null() && n_datas > 0 {
                let data0 = unsafe { &*datas_ptr };
                log::info!(
                    "PipeWire bridge client-node port_use_buffers[{}].data0: type={} flags={} fd={} maxsize={} mapoffset={} data={:p} chunk={:p}",
                    idx,
                    data0.type_,
                    data0.flags,
                    data0.fd,
                    data0.maxsize,
                    data0.mapoffset,
                    data0.data,
                    data0.chunk
                );
            }
        }
    }
    if !state.client_node.is_null() && !buffers.is_null() && n_buffers > 0 {
        let mut spa_buffers: Vec<*mut spa::sys::spa_buffer> = (0..n_buffers as usize)
            .map(|idx| unsafe { (*buffers.add(idx)).buffer })
            .collect();
        let res = unsafe {
            pw_client_node_port_buffers_raw(
                state.client_node,
                direction,
                port_id,
                mix_id,
                n_buffers,
                spa_buffers.as_mut_ptr(),
            )
        };
        log::info!(
            "PipeWire bridge client-node port_buffers: node={} direction={:?} port_id={} mix_id={} n_buffers={} result={}",
            state.config.node_name,
            direction,
            port_id,
            mix_id,
            n_buffers,
            res
        );
    }
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_port_set_io(
    data: *mut c_void,
    direction: spa::sys::spa_direction,
    port_id: u32,
    mix_id: u32,
    id: u32,
    mem_id: u32,
    offset: u32,
    size: u32,
) -> i32 {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::info!(
        "PipeWire bridge client-node port_set_io: node={} direction={:?} port_id={} mix_id={} id={} mem_id={} offset={} size={}",
        state.config.node_name,
        direction,
        port_id,
        mix_id,
        id,
        mem_id,
        offset,
        size
    );
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_set_activation(
    data: *mut c_void,
    node_id: u32,
    signalfd: i32,
    mem_id: u32,
    offset: u32,
    size: u32,
) -> i32 {
    let state = unsafe { &mut *(data as *mut PipewireBridgeClientNodeState) };
    state.activation_mem_id = Some(mem_id);
    state.activation_size = size;
    state.activation_ptr =
        pipewire_bridge_client_node_map_mem_slice(state, mem_id, offset, size).unwrap_or_else(
            || {
                log::warn!(
                    "PipeWire bridge client-node set_activation unresolved mem: node={} mem_id={} offset={} size={}",
                    state.config.node_name,
                    mem_id,
                    offset,
                    size
                );
                std::ptr::null_mut()
            },
        );
    pipewire_bridge_client_node_mark_activation_ready(state);
    log::info!(
        "PipeWire bridge client-node set_activation: node={} peer_node_id={} signalfd={} mem_id={} offset={} size={} ptr={:p}",
        state.config.node_name,
        node_id,
        signalfd,
        mem_id,
        offset,
        size,
        state.activation_ptr
    );
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_port_set_mix_info(
    data: *mut c_void,
    direction: spa::sys::spa_direction,
    port_id: u32,
    mix_id: u32,
    peer_id: u32,
    props: *const spa::sys::spa_dict,
) -> i32 {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::info!(
        "PipeWire bridge client-node port_set_mix_info: node={} direction={:?} port_id={} mix_id={} peer_id={} props={:p}",
        state.config.node_name,
        direction,
        port_id,
        mix_id,
        peer_id,
        props
    );
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_proxy_destroy(data: *mut c_void) {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::info!(
        "PipeWire bridge client-node proxy destroy: node={}",
        state.config.node_name
    );
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_proxy_bound(data: *mut c_void, global_id: u32) {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::info!(
        "PipeWire bridge client-node proxy bound: node={} global_id={}",
        state.config.node_name,
        global_id
    );
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_proxy_removed(data: *mut c_void) {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::info!(
        "PipeWire bridge client-node proxy removed: node={}",
        state.config.node_name
    );
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_proxy_done(data: *mut c_void, seq: i32) {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::info!(
        "PipeWire bridge client-node proxy done: node={} seq={}",
        state.config.node_name,
        seq
    );
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_proxy_error(
    data: *mut c_void,
    seq: i32,
    res: i32,
    message: *const std::os::raw::c_char,
) {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    let message = if message.is_null() {
        "<null>".to_string()
    } else {
        unsafe { CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned()
    };
    log::error!(
        "PipeWire bridge client-node proxy error: node={} seq={} res={} message={}",
        state.config.node_name,
        seq,
        res,
        message
    );
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_proxy_bound_props(
    data: *mut c_void,
    global_id: u32,
    props: *const spa::sys::spa_dict,
) {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::info!(
        "PipeWire bridge client-node proxy bound_props: node={} global_id={} props={:p}",
        state.config.node_name,
        global_id,
        props
    );
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_adapter_proxy_destroy(data: *mut c_void) {
    let state = unsafe { &*(data as *mut PipewireBridgeAdapterState) };
    log::info!(
        "PipeWire bridge adapter proxy destroy: node={}",
        state.config.node_name
    );
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_adapter_proxy_bound(data: *mut c_void, global_id: u32) {
    let state = unsafe { &*(data as *mut PipewireBridgeAdapterState) };
    state.bound.set(true);
    state.global_id.set(global_id);
    log::info!(
        "PipeWire bridge adapter proxy bound: node={} global_id={}",
        state.config.node_name,
        global_id
    );
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_adapter_proxy_removed(data: *mut c_void) {
    let state = unsafe { &*(data as *mut PipewireBridgeAdapterState) };
    state.removed.set(true);
    log::info!(
        "PipeWire bridge adapter proxy removed: node={}",
        state.config.node_name
    );
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_adapter_proxy_done(data: *mut c_void, seq: i32) {
    let state = unsafe { &*(data as *mut PipewireBridgeAdapterState) };
    log::info!(
        "PipeWire bridge adapter proxy done: node={} seq={}",
        state.config.node_name,
        seq
    );
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_adapter_proxy_error(
    data: *mut c_void,
    seq: i32,
    res: i32,
    message: *const std::os::raw::c_char,
) {
    let state = unsafe { &*(data as *mut PipewireBridgeAdapterState) };
    state.errored.set(true);
    let message = if message.is_null() {
        "<null>".to_string()
    } else {
        unsafe { CStr::from_ptr(message) }
            .to_string_lossy()
            .into_owned()
    };
    log::error!(
        "PipeWire bridge adapter proxy error: node={} seq={} res={} message={}",
        state.config.node_name,
        seq,
        res,
        message
    );
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_adapter_proxy_bound_props(
    data: *mut c_void,
    global_id: u32,
    props: *const spa::sys::spa_dict,
) {
    let state = unsafe { &*(data as *mut PipewireBridgeAdapterState) };
    if !props.is_null() {
        let props_ref = unsafe { &*props };
        let items = props_ref.items;
        for idx in 0..props_ref.n_items {
            let item = unsafe { &*items.add(idx as usize) };
            if item.key.is_null() || item.value.is_null() {
                continue;
            }
            let key = unsafe { CStr::from_ptr(item.key) }.to_string_lossy();
            let value = unsafe { CStr::from_ptr(item.value) }.to_string_lossy().into_owned();
            if key.as_ref() == "object.serial" {
                *state.object_serial.borrow_mut() = Some(value);
            } else if key.as_ref() == "node.name" {
                *state.node_name.borrow_mut() = Some(value);
            }
        }
    }
    log::info!(
        "PipeWire bridge adapter proxy bound_props: node={} global_id={} props={:p} object.serial={:?} node.name={:?}",
        state.config.node_name,
        global_id,
        props,
        state.object_serial.borrow().as_deref(),
        state.node_name.borrow().as_deref()
    );
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_node_info(
    data: *mut c_void,
    info: *const pw::sys::pw_node_info,
) {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    if info.is_null() {
        log::info!(
            "PipeWire bridge client-node node info: node={} info=<null>",
            state.config.node_name
        );
        return;
    }
    let info = unsafe { &*info };
    let error = if info.error.is_null() {
        None
    } else {
        Some(
            unsafe { CStr::from_ptr(info.error) }
                .to_string_lossy()
                .into_owned(),
        )
    };
    log::info!(
        "PipeWire bridge client-node node info: node={} id={} state={} change_mask={} n_input_ports={} n_output_ports={} error={}",
        state.config.node_name,
        info.id,
        info.state,
        info.change_mask,
        info.n_input_ports,
        info.n_output_ports,
        error.unwrap_or_else(|| "<none>".to_string())
    );
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_client_node_node_param(
    data: *mut c_void,
    seq: i32,
    id: u32,
    index: u32,
    next: u32,
    param: *const spa::sys::spa_pod,
) {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::info!(
        "PipeWire bridge client-node node param: node={} seq={} id={} index={} next={} param_null={}",
        state.config.node_name,
        seq,
        id,
        index,
        next,
        param.is_null()
    );
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_filter_state_changed(
    data: *mut c_void,
    old: pw::sys::pw_filter_state,
    new: pw::sys::pw_filter_state,
    error: *const std::os::raw::c_char,
) {
    let user_data = unsafe { &*(data as *mut PipewireBridgeFilterUserData) };
    let error_text = if error.is_null() {
        None
    } else {
        unsafe { CStr::from_ptr(error) }
            .to_str()
            .ok()
            .map(ToOwned::to_owned)
    };
    log::info!(
        "PipeWire bridge filter state changed: {:?} -> {:?}{}",
        old,
        new,
        error_text
            .as_deref()
            .map(|e| format!(" ({e})"))
            .unwrap_or_default()
    );
    if new == pw::sys::pw_filter_state_PW_FILTER_STATE_STREAMING {
        log::info!("PipeWire bridge filter is now STREAMING");
    }
    if new == pw::sys::pw_filter_state_PW_FILTER_STATE_ERROR {
        user_data.input_control.set_input_error(Some(format!(
            "PipeWire bridge filter entered error state on {}{}",
            user_data.config.node_name,
            error_text
                .as_deref()
                .map(|e| format!(": {e}"))
                .unwrap_or_default()
        )));
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_filter_param_changed(
    data: *mut c_void,
    _port_data: *mut c_void,
    id: u32,
    param: *const spa::sys::spa_pod,
) {
    let user_data = unsafe { &*(data as *mut PipewireBridgeFilterUserData) };
    if param.is_null() || id != pw::spa::param::ParamType::Format.as_raw() {
        return;
    }
    let param = unsafe { Pod::from_raw(param) };
    let Ok((media_type, media_subtype)) = pw::spa::param::format_utils::parse_format(param) else {
        return;
    };
    if media_type != pw::spa::param::format::MediaType::Audio {
        return;
    }
    log::info!(
        "PipeWire bridge filter format negotiated: subtype={:?}",
        media_subtype
    );
    let mut metrics = user_data.metrics.borrow_mut();
    metrics.last_log_at = Instant::now();
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_filter_process(
    data: *mut c_void,
    _position: *mut spa::sys::spa_io_position,
) {
    let user_data = unsafe { &*(data as *mut PipewireBridgeFilterUserData) };
    if user_data.stop.load(Ordering::Relaxed) {
        return;
    }
    let port = user_data.input_port.get();
    if port.is_null() {
        return;
    }

    let mut metrics = user_data.metrics.borrow_mut();
    metrics.process_calls_since_log += 1;

    let buffer = unsafe { pw::sys::pw_filter_dequeue_buffer(port) };
    if buffer.is_null() {
        log_pipewire_bridge_idle_metrics(&mut metrics);
        return;
    }

    let spa_buffer = unsafe { (*buffer).buffer };
    if spa_buffer.is_null()
        || unsafe { (*spa_buffer).n_datas == 0 || (*spa_buffer).datas.is_null() }
    {
        let _ = unsafe { pw::sys::pw_filter_queue_buffer(port, buffer) };
        return;
    }

    let data0 = unsafe { (*spa_buffer).datas };
    let chunk = unsafe { (*data0).chunk };
    if chunk.is_null() || unsafe { (*chunk).size == 0 || (*data0).data.is_null() } {
        let _ = unsafe { pw::sys::pw_filter_queue_buffer(port, buffer) };
        return;
    }

    let byte_len = unsafe { (*chunk).size as usize };
    let max_len = unsafe { (*data0).maxsize as usize };
    if byte_len == 0 || byte_len > max_len {
        let _ = unsafe { pw::sys::pw_filter_queue_buffer(port, buffer) };
        return;
    }

    let bytes = unsafe { std::slice::from_raw_parts((*data0).data as *const u8, byte_len) };
    process_pipewire_bridge_bytes(user_data, &mut metrics, bytes);
    let _ = unsafe { pw::sys::pw_filter_queue_buffer(port, buffer) };
}

#[cfg(target_os = "linux")]
fn log_pipewire_bridge_idle_metrics(metrics: &mut BridgeCaptureUserData) {
    metrics.empty_polls_since_log += 1;
    let now = Instant::now();
    if now.duration_since(metrics.last_log_at) >= LIVE_BRIDGE_LOG_INTERVAL {
        log::info!(
            "PipeWire bridge ingest idle: process_calls={} empty_polls={} rate={}Hz channels={}",
            metrics.process_calls_since_log,
            metrics.empty_polls_since_log,
            metrics.rate_hz,
            metrics.channels
        );
        metrics.last_log_at = now;
        metrics.process_calls_since_log = 0;
        metrics.empty_polls_since_log = 0;
    }
}

#[cfg(target_os = "linux")]
fn process_pipewire_bridge_bytes(
    user_data: &PipewireBridgeFilterUserData,
    metrics: &mut BridgeCaptureUserData,
    chunk: &[u8],
) {
    let has_spdif_sync = chunk.windows(4).any(|w| {
        u16::from_le_bytes([w[0], w[1]]) == 0xF872
            && u16::from_le_bytes([w[2], w[3]]) == 0x4E1F
    });
    metrics.bytes_since_log += chunk.len();
    metrics.buffers_since_log += 1;
    if has_spdif_sync {
        metrics.sync_buffers_since_log += 1;
    }
    let (packet_count, frame_count) = user_data.ingest.borrow_mut().process_chunk(chunk);
    metrics.packets_since_log += packet_count;
    metrics.frames_since_log += frame_count;
    let now = Instant::now();
    if now.duration_since(metrics.last_log_at) >= LIVE_BRIDGE_LOG_INTERVAL {
        log::info!(
            "PipeWire bridge ingest: process_calls={} buffers={} bytes={} sync_buffers={} packets={} frames={} empty_polls={} rate={}Hz channels={}",
            metrics.process_calls_since_log,
            metrics.buffers_since_log,
            metrics.bytes_since_log,
            metrics.sync_buffers_since_log,
            metrics.packets_since_log,
            metrics.frames_since_log,
            metrics.empty_polls_since_log,
            metrics.rate_hz,
            metrics.channels
        );
        if metrics.buffers_since_log > 0 && metrics.sync_buffers_since_log == 0 {
            log::warn!("PipeWire bridge ingest has audio buffers but no IEC61937 sync words yet");
        }
        metrics.last_log_at = now;
        metrics.process_calls_since_log = 0;
        metrics.bytes_since_log = 0;
        metrics.buffers_since_log = 0;
        metrics.sync_buffers_since_log = 0;
        metrics.packets_since_log = 0;
        metrics.frames_since_log = 0;
        metrics.empty_polls_since_log = 0;
    }
}

#[cfg(target_os = "linux")]
static PIPEWIRE_BRIDGE_EXPORTED_NODE_METHODS: spa::sys::spa_node_methods =
    spa::sys::spa_node_methods {
        version: spa::sys::SPA_VERSION_NODE_METHODS,
        add_listener: Some(pipewire_bridge_exported_node_add_listener),
        set_callbacks: Some(pipewire_bridge_exported_node_set_callbacks),
        sync: Some(pipewire_bridge_exported_node_sync),
        enum_params: Some(pipewire_bridge_exported_node_enum_params),
        set_param: Some(pipewire_bridge_exported_node_set_param),
        set_io: Some(pipewire_bridge_exported_node_set_io),
        send_command: Some(pipewire_bridge_exported_node_send_command),
        add_port: None,
        remove_port: None,
        port_enum_params: Some(pipewire_bridge_exported_node_port_enum_params),
        port_set_param: Some(pipewire_bridge_exported_node_port_set_param),
        port_use_buffers: Some(pipewire_bridge_exported_node_port_use_buffers),
        port_set_io: Some(pipewire_bridge_exported_node_port_set_io),
        port_reuse_buffer: None,
        process: Some(pipewire_bridge_exported_node_process),
    };

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_exported_node_add_listener(
    object: *mut c_void,
    listener: *mut spa::sys::spa_hook,
    events: *const spa::sys::spa_node_events,
    data: *mut c_void,
) -> i32 {
    let node = unsafe { &mut *(object as *mut PipewireBridgeExportNode) };
    log::info!(
        "PipeWire bridge exported node add_listener: node={} listener={:p} events={:p} data={:p}",
        node.config.node_name,
        listener,
        events,
        data
    );
    unsafe {
        spa::sys::spa_hook_list_append(
            &mut node.hooks,
            listener,
            events.cast(),
            data.cast(),
        );
    }
    if !events.is_null() {
        unsafe {
            if let Some(cb) = (*events).info {
                cb(data, &node.node_info);
            }
            if let Some(cb) = (*events).port_info {
                cb(data, spa::sys::SPA_DIRECTION_INPUT, 0, &node.port_info);
                cb(
                    data,
                    spa::sys::SPA_DIRECTION_OUTPUT,
                    0,
                    &node.monitor_port_info,
                );
            }
        }
    }
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_exported_node_set_callbacks(
    _object: *mut c_void,
    _callbacks: *const spa::sys::spa_node_callbacks,
    _data: *mut c_void,
) -> i32 {
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_exported_node_sync(_object: *mut c_void, _seq: i32) -> i32 {
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_exported_node_enum_params(
    object: *mut c_void,
    seq: i32,
    id: u32,
    start: u32,
    max: u32,
    _filter: *const spa::sys::spa_pod,
) -> i32 {
    let node = unsafe { &mut *(object as *mut PipewireBridgeExportNode) };
    log::info!(
        "PipeWire bridge exported node enum_params: node={} seq={} id={} start={} max={}",
        node.config.node_name,
        seq,
        id,
        start,
        max
    );
    let mut count = 0u32;

    for index in start.. {
        let param_ptr = match id {
            x if x == spa::sys::SPA_PARAM_Props => {
                if index == 0 {
                    node.props_param_bytes.as_ptr().cast_mut().cast()
                } else {
                    break;
                }
            }
            x if x == spa::sys::SPA_PARAM_EnumFormat => {
                if index == 0 {
                    node.enum_format_bytes.as_ptr().cast_mut().cast()
                } else {
                    break;
                }
            }
            x if x == spa::sys::SPA_PARAM_Format => {
                if index == 0 {
                    node.format_bytes.as_ptr().cast_mut().cast()
                } else {
                    break;
                }
            }
            x if x == spa::sys::SPA_PARAM_EnumPortConfig => {
                if index == 0 {
                    node.enum_port_config_bytes.as_ptr().cast_mut().cast()
                } else {
                    break;
                }
            }
            x if x == spa::sys::SPA_PARAM_PortConfig => {
                if index == 0 {
                    node.port_config_bytes.as_ptr().cast_mut().cast()
                } else {
                    break;
                }
            }
            x if x == spa::sys::SPA_PARAM_Latency => {
                if index == 0 {
                    node.latency_param_bytes.as_ptr().cast_mut().cast()
                } else {
                    break;
                }
            }
            x if x == spa::sys::SPA_PARAM_ProcessLatency => {
                if index == 0 {
                    node.process_latency_param_bytes.as_ptr().cast_mut().cast()
                } else {
                    break;
                }
            }
            x if x == spa::sys::SPA_PARAM_Tag => {
                if index == 0 {
                    node.tag_param_bytes.as_ptr().cast_mut().cast()
                } else {
                    break;
                }
            }
            _ => return -libc::ENOENT,
        };

        let result = spa::sys::spa_result_node_params {
            id,
            index,
            next: index + 1,
            param: param_ptr,
        };
        emit_pipewire_bridge_node_result(node, seq, &result);
        count += 1;
        if count >= max.max(1) {
            break;
        }
    }
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_exported_node_set_param(
    object: *mut c_void,
    id: u32,
    flags: u32,
    param: *const spa::sys::spa_pod,
) -> i32 {
    let node = unsafe { &mut *(object as *mut PipewireBridgeExportNode) };
    log::info!(
        "PipeWire bridge exported node set_param: node={} id={} flags={} param_null={}",
        node.config.node_name,
        id,
        flags,
        param.is_null()
    );
    match id {
        x if x == spa::sys::SPA_PARAM_Format || x == spa::sys::SPA_PARAM_PortConfig => {
            if x == spa::sys::SPA_PARAM_Format {
                if let Some(bytes) = clone_spa_pod_bytes(param) {
                    node.format_bytes = bytes;
                }
            } else if x == spa::sys::SPA_PARAM_PortConfig {
                if let Some(bytes) = clone_spa_pod_bytes(param) {
                    node.port_config_bytes = bytes;
                }
            }
            node.update_port_params_for_format(true);
            emit_pipewire_bridge_node_info(node);
            emit_pipewire_bridge_node_port_info(node);
            log::info!(
                "PipeWire bridge exported node accepted node-level config: node={} id={} format_configured={}",
                node.config.node_name,
                id,
                node.format_configured
            );
            0
        }
        _ => 0,
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_exported_node_set_io(
    object: *mut c_void,
    id: u32,
    data: *mut c_void,
    size: usize,
) -> i32 {
    let node = unsafe { &mut *(object as *mut PipewireBridgeExportNode) };
    log::info!(
        "PipeWire bridge exported node set_io: node={} id={} data={:p} size={}",
        node.config.node_name,
        id,
        data,
        size
    );
    match id {
        x if x == spa::sys::SPA_IO_Clock => {
            if !data.is_null() && size < std::mem::size_of::<spa::sys::spa_io_clock>() {
                return -libc::ENOSPC;
            }
            node.io_clock = data.cast();
            0
        }
        x if x == spa::sys::SPA_IO_Position => {
            if !data.is_null() && size < std::mem::size_of::<spa::sys::spa_io_position>() {
                return -libc::ENOSPC;
            }
            node.io_position = data.cast();
            0
        }
        _ => 0,
    }
}

#[cfg(target_os = "linux")]
fn pipewire_bridge_node_command_name(command: *const spa::sys::spa_command) -> &'static str {
    if command.is_null() {
        return "null";
    }
    let command = unsafe { &*command };
    if command.body.body.type_ != spa::sys::SPA_TYPE_COMMAND_Node {
        return "other";
    }
    match command.body.body.id {
        x if x == spa::sys::SPA_NODE_COMMAND_Suspend => "Suspend",
        x if x == spa::sys::SPA_NODE_COMMAND_Pause => "Pause",
        x if x == spa::sys::SPA_NODE_COMMAND_Start => "Start",
        x if x == spa::sys::SPA_NODE_COMMAND_Enable => "Enable",
        x if x == spa::sys::SPA_NODE_COMMAND_Disable => "Disable",
        x if x == spa::sys::SPA_NODE_COMMAND_Flush => "Flush",
        x if x == spa::sys::SPA_NODE_COMMAND_Drain => "Drain",
        x if x == spa::sys::SPA_NODE_COMMAND_Marker => "Marker",
        x if x == spa::sys::SPA_NODE_COMMAND_ParamBegin => "ParamBegin",
        x if x == spa::sys::SPA_NODE_COMMAND_ParamEnd => "ParamEnd",
        x if x == spa::sys::SPA_NODE_COMMAND_RequestProcess => "RequestProcess",
        x if x == spa::sys::SPA_NODE_COMMAND_User => "User",
        _ => "Unknown",
    }
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_exported_node_send_command(
    object: *mut c_void,
    command: *const spa::sys::spa_command,
) -> i32 {
    let node = unsafe { &mut *(object as *mut PipewireBridgeExportNode) };
    let command_type = if command.is_null() {
        u32::MAX
    } else {
        unsafe { (*command).body.body.type_ }
    };
    let command_id = if command.is_null() {
        u32::MAX
    } else {
        unsafe { (*command).body.body.id }
    };
    match command_id {
        x if x == spa::sys::SPA_NODE_COMMAND_Start => {
            node.started = true;
            node.suspended = false;
        }
        x if x == spa::sys::SPA_NODE_COMMAND_Pause => {
            node.started = false;
        }
        x if x == spa::sys::SPA_NODE_COMMAND_Suspend => {
            node.started = false;
            node.suspended = true;
            node.format_configured = false;
            node.update_port_params_for_format(false);
            emit_pipewire_bridge_node_info(node);
            emit_pipewire_bridge_node_port_info(node);
        }
        _ => {}
    }
    log::info!(
        "PipeWire bridge exported node send_command: node={} command={:p} type={} id={} name={} started={} suspended={}",
        node.config.node_name,
        command,
        command_type,
        command_id,
        pipewire_bridge_node_command_name(command),
        node.started,
        node.suspended
    );
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_exported_node_port_enum_params(
    object: *mut c_void,
    seq: i32,
    direction: spa::sys::spa_direction,
    port_id: u32,
    id: u32,
    start: u32,
    max: u32,
    _filter: *const spa::sys::spa_pod,
) -> i32 {
    if port_id != 0 {
        return -libc::EINVAL;
    }
    let node = unsafe { &mut *(object as *mut PipewireBridgeExportNode) };
    log::info!(
        "PipeWire bridge exported node port_enum_params: node={} direction={:?} port_id={} seq={} id={} start={} max={} configured={}",
        node.config.node_name,
        direction,
        port_id,
        seq,
        id,
        start,
        max,
        node.format_configured
    );
    let mut count = 0u32;

    for index in start.. {
        let param_ptr = match id {
            x if x == spa::sys::SPA_PARAM_EnumFormat => {
                if index == 0 {
                    node.enum_format_bytes.as_ptr().cast_mut().cast()
                } else {
                    break;
                }
            }
            x if x == spa::sys::SPA_PARAM_Meta => {
                if index == 0 {
                    node.meta_param_bytes.as_ptr().cast_mut().cast()
                } else {
                    break;
                }
            }
            x if x == spa::sys::SPA_PARAM_IO => {
                if index == 0 {
                    node.io_buffers_param_bytes.as_ptr().cast_mut().cast()
                } else {
                    break;
                }
            }
            x if x == spa::sys::SPA_PARAM_Format => {
                if index == 0 {
                    node.format_bytes.as_ptr().cast_mut().cast()
                } else {
                    break;
                }
            }
            x if x == spa::sys::SPA_PARAM_Buffers => {
                if index == 0 {
                    node.buffers_param_bytes.as_ptr().cast_mut().cast()
                } else {
                    break;
                }
            }
            x if x == spa::sys::SPA_PARAM_Latency => {
                if index == 0 {
                    node.latency_param_bytes.as_ptr().cast_mut().cast()
                } else {
                    break;
                }
            }
            x if x == spa::sys::SPA_PARAM_Tag => {
                if index == 0 {
                    if direction == spa::sys::SPA_DIRECTION_INPUT {
                        node.tag_param_bytes.as_ptr().cast_mut().cast()
                    } else {
                        node.monitor_tag_param_bytes.as_ptr().cast_mut().cast()
                    }
                } else {
                    break;
                }
            }
            _ => return -libc::ENOENT,
        };

        let result = spa::sys::spa_result_node_params {
            id,
            index,
            next: index + 1,
            param: param_ptr,
        };
        emit_pipewire_bridge_node_result(node, seq, &result);
        count += 1;
        if count >= max.max(1) {
            break;
        }
    }
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_exported_node_port_set_param(
    object: *mut c_void,
    direction: spa::sys::spa_direction,
    port_id: u32,
    id: u32,
    _flags: u32,
    _param: *const spa::sys::spa_pod,
) -> i32 {
    if port_id != 0 {
        return -libc::EINVAL;
    }
    let node = unsafe { &mut *(object as *mut PipewireBridgeExportNode) };
    log::info!(
        "PipeWire bridge exported node port_set_param: node={} direction={:?} port_id={} id={} flags={} param_null={}",
        node.config.node_name,
        direction,
        port_id,
        id,
        _flags,
        _param.is_null()
    );
    if direction == spa::sys::SPA_DIRECTION_OUTPUT {
        return 0;
    }
    if id != spa::sys::SPA_PARAM_Format {
        return -libc::ENOENT;
    }
    if let Some(bytes) = clone_spa_pod_bytes(_param) {
        node.format_bytes = bytes;
    }
    node.update_port_params_for_format(true);
    emit_pipewire_bridge_node_info(node);
    emit_pipewire_bridge_node_port_info(node);
    log::info!(
        "PipeWire bridge exported node format configured: node={} rate={}Hz channels={}",
        node.config.node_name,
        node.config.sample_rate_hz,
        node.config.channels
    );
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_exported_node_port_use_buffers(
    object: *mut c_void,
    direction: spa::sys::spa_direction,
    port_id: u32,
    flags: u32,
    buffers: *mut *mut spa::sys::spa_buffer,
    n_buffers: u32,
) -> i32 {
    if port_id != 0 {
        return -libc::EINVAL;
    }
    let node = unsafe { &mut *(object as *mut PipewireBridgeExportNode) };
    log::info!(
        "PipeWire bridge exported node port_use_buffers: node={} direction={:?} port_id={} flags={} n_buffers={} buffers_null={}",
        node.config.node_name,
        direction,
        port_id,
        flags,
        n_buffers,
        buffers.is_null()
    );
    if direction == spa::sys::SPA_DIRECTION_OUTPUT {
        return 0;
    }
    if n_buffers as usize > PIPEWIRE_BRIDGE_NODE_MAX_BUFFERS {
        return -libc::ENOSPC;
    }
    if buffers.is_null() {
        node.n_buffers = 0;
        for slot in &mut node.owned_buffer_data {
            *slot = None;
        }
        return 0;
    }
    let port_bytes_per_frame = (node.config.channels as usize) * std::mem::size_of::<u16>();
    let nominal_frames = node.config.sample_rate_hz.div_ceil(100) as usize;
    let nominal_size = (port_bytes_per_frame * nominal_frames).max(1024);
    for index in 0..(n_buffers as usize) {
        let buffer = unsafe { *buffers.add(index) };
        node.buffers[index] = buffer;
        if buffer.is_null() {
            continue;
        }
        let spa_buffer = unsafe { &mut *buffer };
        if spa_buffer.n_datas == 0 || spa_buffer.datas.is_null() {
            continue;
        }
        let data0 = unsafe { &mut *spa_buffer.datas };
        if flags & spa::sys::SPA_NODE_BUFFERS_FLAG_ALLOC != 0 {
            let requested_size = if data0.maxsize == 0 {
                nominal_size
            } else {
                (data0.maxsize as usize).max(nominal_size)
            };
            let storage = node.owned_buffer_data[index].get_or_insert_with(|| vec![0u8; requested_size]);
            if storage.len() != requested_size {
                storage.resize(requested_size, 0);
            }
            data0.type_ = spa::sys::SPA_DATA_MemPtr;
            data0.flags = spa::sys::SPA_DATA_FLAG_READWRITE | spa::sys::SPA_DATA_FLAG_MAPPABLE;
            data0.fd = -1;
            data0.mapoffset = 0;
            data0.maxsize = storage.len() as u32;
            data0.data = storage.as_mut_ptr().cast();
            if let Some(chunk) = unsafe { data0.chunk.as_mut() } {
                chunk.offset = 0;
                chunk.size = 0;
                chunk.stride = port_bytes_per_frame as i32;
                chunk.flags = spa::sys::SPA_CHUNK_FLAG_EMPTY as i32;
            }
        }
    }
    node.n_buffers = n_buffers;
    log::info!(
        "PipeWire bridge exported node buffers configured: node={} buffers={} alloc={} nominal_size={}",
        node.config.node_name,
        node.n_buffers,
        (flags & spa::sys::SPA_NODE_BUFFERS_FLAG_ALLOC) != 0,
        nominal_size
    );
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_exported_node_port_set_io(
    object: *mut c_void,
    direction: spa::sys::spa_direction,
    port_id: u32,
    id: u32,
    data: *mut c_void,
    size: usize,
) -> i32 {
    if port_id != 0 {
        return -libc::EINVAL;
    }
    let node = unsafe { &mut *(object as *mut PipewireBridgeExportNode) };
    log::info!(
        "PipeWire bridge exported node port_set_io: node={} direction={:?} port_id={} id={} data={:p} size={}",
        node.config.node_name,
        direction,
        port_id,
        id,
        data,
        size
    );
    if direction == spa::sys::SPA_DIRECTION_OUTPUT {
        return 0;
    }
    if id != spa::sys::SPA_IO_Buffers {
        return -libc::ENOENT;
    }
    if !data.is_null() && size < std::mem::size_of::<spa::sys::spa_io_buffers>() {
        return -libc::ENOSPC;
    }
    node.io_buffers = data.cast();
    0
}

#[cfg(target_os = "linux")]
unsafe extern "C" fn pipewire_bridge_exported_node_process(object: *mut c_void) -> i32 {
    let node = unsafe { &mut *(object as *mut PipewireBridgeExportNode) };
    if node.stop.load(Ordering::Relaxed) {
        return spa::sys::SPA_STATUS_NEED_DATA as i32;
    }
    let Some(io_buffers) = (unsafe { node.io_buffers.as_mut() }) else {
        return spa::sys::SPA_STATUS_NEED_DATA as i32;
    };

    if io_buffers.status as u32 != spa::sys::SPA_STATUS_HAVE_DATA {
        return spa::sys::SPA_STATUS_NEED_DATA as i32;
    }
    if io_buffers.buffer_id >= node.n_buffers {
        io_buffers.status = spa::sys::SPA_STATUS_NEED_DATA as i32;
        return spa::sys::SPA_STATUS_NEED_DATA as i32;
    }
    let buffer = node.buffers[io_buffers.buffer_id as usize];
    if buffer.is_null() {
        io_buffers.status = spa::sys::SPA_STATUS_NEED_DATA as i32;
        return spa::sys::SPA_STATUS_NEED_DATA as i32;
    }
    let spa_buffer = unsafe { &*buffer };
    if spa_buffer.n_datas == 0 || spa_buffer.datas.is_null() {
        io_buffers.status = spa::sys::SPA_STATUS_NEED_DATA as i32;
        return spa::sys::SPA_STATUS_NEED_DATA as i32;
    }
    let data0 = unsafe { &*spa_buffer.datas };
    let Some(chunk) = (unsafe { spa_data_chunk_slice(data0) }) else {
        io_buffers.status = spa::sys::SPA_STATUS_NEED_DATA as i32;
        return spa::sys::SPA_STATUS_NEED_DATA as i32;
    };

    let mut metrics = node.metrics.borrow_mut();
    metrics.process_calls_since_log += 1;
    if !metrics.first_process_logged || metrics.process_calls_since_log <= 8 {
        let (position_state, clock_rate_num, clock_rate_denom, clock_position, clock_duration) =
            if let Some(position) = unsafe { node.io_position.as_ref() } {
                (
                    position.state,
                    position.clock.rate.num,
                    position.clock.rate.denom,
                    position.clock.position,
                    position.clock.duration,
                )
            } else {
                (u32::MAX, 0, 0, 0, 0)
            };
        log::info!(
            "PipeWire bridge exported node process: node={} io_buffers={:p} status={} buffer_id={} n_buffers={} started={} suspended={} pos_state={} clock_rate={}/{} clock_position={} clock_duration={}",
            node.config.node_name,
            node.io_buffers,
            io_buffers.status,
            io_buffers.buffer_id,
            node.n_buffers,
            node.started,
            node.suspended,
            position_state,
            clock_rate_num,
            clock_rate_denom,
            clock_position,
            clock_duration
        );
        metrics.first_process_logged = true;
    }
    process_pipewire_bridge_exported_bytes(node, &mut metrics, chunk);
    io_buffers.status = spa::sys::SPA_STATUS_NEED_DATA as i32;
    spa::sys::SPA_STATUS_NEED_DATA as i32
}

#[cfg(target_os = "linux")]
unsafe fn spa_data_chunk_slice(data: &spa::sys::spa_data) -> Option<&[u8]> {
    let chunk = unsafe { data.chunk.as_ref() }?;
    let base = data.data.cast::<u8>();
    if base.is_null() {
        return None;
    }
    let offset = chunk.offset as usize;
    let size = chunk.size as usize;
    let maxsize = data.maxsize as usize;
    if offset > maxsize || size > maxsize.saturating_sub(offset) {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(base.add(offset), size) })
}

#[cfg(target_os = "linux")]
fn process_pipewire_bridge_exported_bytes(
    node: &PipewireBridgeExportNode,
    metrics: &mut BridgeCaptureUserData,
    chunk: &[u8],
) {
    process_pipewire_bridge_chunk_metrics(metrics, chunk, || {
        node.ingest.borrow_mut().process_chunk(chunk)
    });
}

#[cfg(target_os = "linux")]
fn process_pipewire_bridge_chunk_metrics<F>(
    metrics: &mut BridgeCaptureUserData,
    chunk: &[u8],
    process_chunk: F,
) where
    F: FnOnce() -> (usize, usize),
{
    let has_spdif_sync = chunk.windows(4).any(|w| {
        u16::from_le_bytes([w[0], w[1]]) == 0xF872
            && u16::from_le_bytes([w[2], w[3]]) == 0x4E1F
    });
    metrics.bytes_since_log += chunk.len();
    metrics.buffers_since_log += 1;
    if has_spdif_sync {
        metrics.sync_buffers_since_log += 1;
    }
    let (packet_count, frame_count) = process_chunk();
    metrics.packets_since_log += packet_count;
    metrics.frames_since_log += frame_count;
    let now = Instant::now();
    if now.duration_since(metrics.last_log_at) >= LIVE_BRIDGE_LOG_INTERVAL {
        log::info!(
            "PipeWire bridge ingest: process_calls={} buffers={} bytes={} sync_buffers={} packets={} frames={} empty_polls={} rate={}Hz channels={}",
            metrics.process_calls_since_log,
            metrics.buffers_since_log,
            metrics.bytes_since_log,
            metrics.sync_buffers_since_log,
            metrics.packets_since_log,
            metrics.frames_since_log,
            metrics.empty_polls_since_log,
            metrics.rate_hz,
            metrics.channels
        );
        if metrics.buffers_since_log > 0 && metrics.sync_buffers_since_log == 0 {
            log::warn!("PipeWire bridge ingest has audio buffers but no IEC61937 sync words yet");
        }
        metrics.last_log_at = now;
        metrics.process_calls_since_log = 0;
        metrics.bytes_since_log = 0;
        metrics.buffers_since_log = 0;
        metrics.sync_buffers_since_log = 0;
        metrics.packets_since_log = 0;
        metrics.frames_since_log = 0;
        metrics.empty_polls_since_log = 0;
    }
}

#[cfg(target_os = "linux")]
fn clone_spa_pod_bytes(param: *const spa::sys::spa_pod) -> Option<Vec<u8>> {
    if param.is_null() {
        return None;
    }
    let pod = unsafe { &*param };
    let total_size = std::mem::size_of::<spa::sys::spa_pod>() + pod.size as usize;
    Some(unsafe { std::slice::from_raw_parts(param.cast::<u8>(), total_size) }.to_vec())
}

#[cfg(target_os = "linux")]
fn emit_pipewire_bridge_node_info(node: &mut PipewireBridgeExportNode) {
    unsafe {
        for_each_pipewire_bridge_node_listener(&mut node.hooks, |events, data| {
            if let Some(cb) = events.info {
                cb(data, &node.node_info);
            }
        });
    }
}

#[cfg(target_os = "linux")]
fn emit_pipewire_bridge_node_port_info(node: &mut PipewireBridgeExportNode) {
    unsafe {
        for_each_pipewire_bridge_node_listener(&mut node.hooks, |events, data| {
            if let Some(cb) = events.port_info {
                cb(data, spa::sys::SPA_DIRECTION_INPUT, 0, &node.port_info);
                cb(
                    data,
                    spa::sys::SPA_DIRECTION_OUTPUT,
                    0,
                    &node.monitor_port_info,
                );
            }
        });
    }
}

#[cfg(target_os = "linux")]
fn emit_pipewire_bridge_node_result(
    node: &mut PipewireBridgeExportNode,
    seq: i32,
    result: &spa::sys::spa_result_node_params,
) {
    unsafe {
        for_each_pipewire_bridge_node_listener(&mut node.hooks, |events, data| {
            if let Some(cb) = events.result {
                cb(
                    data,
                    seq,
                    0,
                    spa::sys::SPA_RESULT_TYPE_NODE_PARAMS,
                    (result as *const spa::sys::spa_result_node_params).cast(),
                );
            }
        });
    }
}

#[cfg(target_os = "linux")]
unsafe fn for_each_pipewire_bridge_node_listener<F>(
    hooks: &mut spa::sys::spa_hook_list,
    mut f: F,
) where
    F: FnMut(&spa::sys::spa_node_events, *mut c_void),
{
    let head = &mut hooks.list as *mut spa::sys::spa_list;
    let mut cursor = unsafe { (*head).next };
    while cursor != head {
        let hook = cursor.cast::<spa::sys::spa_hook>();
        let funcs = unsafe { (*hook).cb.funcs.cast::<spa::sys::spa_node_events>() };
        if !funcs.is_null() {
            f(unsafe { &*funcs }, unsafe { (*hook).cb.data });
        }
        cursor = unsafe { (*cursor).next };
    }
}

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
fn build_pipewire_bridge_stream_properties(
    config: &PipewireBridgeInputConfig,
    requested_latency: &str,
) -> pw::properties::PropertiesBox {
    let mut props = pw::properties::PropertiesBox::new();
    let requested_rate = format!("1/{}", config.sample_rate_hz);
    props.insert(*pw::keys::MEDIA_TYPE, "Audio");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Playback");
    props.insert(*pw::keys::MEDIA_ROLE, "Movie");
    props.insert("media.class", "Audio/Sink");
    props.insert("node.virtual", "true");
    props.insert("node.name", config.node_name.clone());
    props.insert("node.description", config.node_description.clone());
    props.insert("media.name", config.node_description.clone());
    props.insert("audio.channels", config.channels.to_string());
    props.insert("audio.position", IEC958_AUDIO_POSITION_PROP);
    props.insert("iec958.codecs", TRUEHD_ONLY_IEC958_CODECS_PROP);
    props.insert("resample.disable", "true");
    props.insert("node.latency", requested_latency);
    props.insert("node.rate", requested_rate);
    props.insert("node.lock-rate", "true");
    props.insert("node.force-rate", config.sample_rate_hz.to_string());
    props
}

#[cfg(target_os = "linux")]
fn build_pipewire_bridge_adapter_properties(
    config: &PipewireBridgeInputConfig,
    requested_latency: &str,
) -> pw::properties::PropertiesBox {
    let mut props = pw::properties::PropertiesBox::new();
    props.insert("factory.name", "support.null-audio-sink");
    props.insert(*pw::keys::MEDIA_TYPE, "Audio");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Playback");
    props.insert(*pw::keys::MEDIA_ROLE, "Movie");
    props.insert("media.class", "Audio/Sink");
    props.insert("object.linger", "false");
    props.insert("node.virtual", "true");
    props.insert("node.name", config.node_name.clone());
    props.insert("node.description", config.node_description.clone());
    props.insert("media.name", config.node_description.clone());
    props.insert("audio.channels", config.channels.to_string());
    props.insert("audio.position", IEC958_AUDIO_POSITION_PROP);
    props.insert("iec958.codecs", TRUEHD_ONLY_IEC958_CODECS_PROP);
    props.insert("resample.disable", "true");
    props.insert("node.latency", requested_latency);
    props
}

#[cfg(target_os = "linux")]
fn build_pipewire_bridge_capture_stream_properties(
    config: &PipewireBridgeInputConfig,
    target_object: &str,
) -> pw::properties::PropertiesBox {
    let mut props = pw::properties::PropertiesBox::new();
    props.insert(*pw::keys::MEDIA_TYPE, "Audio");
    props.insert(*pw::keys::MEDIA_CATEGORY, "Capture");
    props.insert(*pw::keys::MEDIA_ROLE, "Movie");
    props.insert("target.object", target_object);
    props.insert("node.target", target_object);
    props.insert(*pw::keys::STREAM_CAPTURE_SINK, "true");
    props.insert(*pw::keys::STREAM_MONITOR, "true");
    props.insert("node.name", format!("{}.monitor.capture", config.node_name));
    props.insert(
        "node.description",
        format!("{} Monitor Capture", config.node_description),
    );
    props.insert("media.name", format!("{} Monitor Capture", config.node_description));
    props.insert("audio.channels", config.channels.to_string());
    props.insert("audio.position", IEC958_AUDIO_POSITION_PROP);
    props.insert("iec958.codecs", TRUEHD_ONLY_IEC958_CODECS_PROP);
    props.insert("resample.disable", "true");
    props
}

#[cfg(target_os = "linux")]
fn build_pipewire_bridge_buffers_pod(config: &PipewireBridgeInputConfig) -> Result<Vec<u8>> {
    let port_bytes_per_frame = (config.channels as usize) * std::mem::size_of::<u16>();
    let nominal_frames = config.sample_rate_hz.div_ceil(100);
    let nominal_size = (port_bytes_per_frame * nominal_frames as usize).max(1024);
    let obj = object! {
        spa::utils::SpaTypes::ObjectParamBuffers,
        spa::param::ParamType::Buffers,
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_BUFFERS_buffers), Int, 8i32),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_BUFFERS_blocks), Int, 1i32),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_BUFFERS_size), Int, nominal_size as i32),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_BUFFERS_stride), Int, port_bytes_per_frame as i32),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_BUFFERS_align), Int, 16i32),
        property!(
            RawSpaPodKey(spa::sys::SPA_PARAM_BUFFERS_dataType),
            pw::spa::pod::Value::Int(spa::sys::SPA_DATA_MemPtr as i32)
        ),
        property!(
            RawSpaPodKey(spa::sys::SPA_PARAM_BUFFERS_metaType),
            pw::spa::pod::Value::Int(1i32 << (spa::sys::SPA_META_Header as i32))
        ),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge buffer pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

#[cfg(target_os = "linux")]
fn build_pipewire_bridge_io_buffers_pod() -> Result<Vec<u8>> {
    let obj = object! {
        spa::utils::SpaTypes::ObjectParamIO,
        spa::param::ParamType::IO,
        property!(
            RawSpaPodKey(spa::sys::SPA_PARAM_IO_id),
            pw::spa::pod::Value::Id(pw::spa::utils::Id(spa::sys::SPA_IO_Buffers))
        ),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_IO_size), Int, std::mem::size_of::<spa::sys::spa_io_buffers>() as i32),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge IO pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

#[cfg(target_os = "linux")]
fn build_pipewire_bridge_props_pod() -> Result<Vec<u8>> {
    let obj = object! {
        spa::utils::SpaTypes::ObjectParamProps,
        spa::param::ParamType::Props,
        property!(RawSpaPodKey(spa::sys::SPA_PROP_mute), Bool, false),
        property!(RawSpaPodKey(spa::sys::SPA_PROP_volume), Float, 1.0f32),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge props pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

#[cfg(target_os = "linux")]
fn build_pipewire_bridge_meta_pod() -> Result<Vec<u8>> {
    let obj = object! {
        spa::utils::SpaTypes::ObjectParamMeta,
        spa::param::ParamType::Meta,
        property!(
            RawSpaPodKey(spa::sys::SPA_PARAM_META_type),
            pw::spa::pod::Value::Id(pw::spa::utils::Id(spa::sys::SPA_META_Header))
        ),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_META_size), Int, 32i32),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge meta pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

#[cfg(target_os = "linux")]
fn build_pipewire_bridge_process_latency_pod() -> Result<Vec<u8>> {
    let obj = object! {
        spa::utils::SpaTypes::ObjectParamProcessLatency,
        spa::param::ParamType::ProcessLatency,
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_PROCESS_LATENCY_quantum), Float, 0.0f32),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_PROCESS_LATENCY_rate), Int, 0i32),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_PROCESS_LATENCY_ns), Long, 0i64),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge process latency pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

#[cfg(target_os = "linux")]
fn build_pipewire_bridge_tag_pod(direction: spa::sys::spa_direction) -> Result<Vec<u8>> {
    let obj = object! {
        pw::spa::utils::SpaTypes::from_raw(spa::sys::SPA_TYPE_OBJECT_ParamTag),
        RawSpaPodKey(spa::sys::SPA_PARAM_Tag),
        property!(
            RawSpaPodKey(spa::sys::SPA_PARAM_TAG_direction),
            pw::spa::pod::Value::Id(pw::spa::utils::Id(direction))
        ),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge tag pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

#[cfg(target_os = "linux")]
fn build_pipewire_bridge_latency_pod() -> Result<Vec<u8>> {
    let obj = object! {
        spa::utils::SpaTypes::ObjectParamLatency,
        spa::param::ParamType::Latency,
        property!(
            RawSpaPodKey(spa::sys::SPA_PARAM_LATENCY_direction),
            pw::spa::pod::Value::Id(pw::spa::utils::Id(spa::sys::SPA_DIRECTION_INPUT))
        ),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_LATENCY_minQuantum), Float, 0.0f32),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_LATENCY_maxQuantum), Float, 0.0f32),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_LATENCY_minRate), Int, 0i32),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_LATENCY_maxRate), Int, 0i32),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_LATENCY_minNs), Long, 0i64),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_LATENCY_maxNs), Long, 0i64),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge latency pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

#[cfg(target_os = "linux")]
fn build_pipewire_bridge_enum_port_config_pod() -> Result<Vec<u8>> {
    let obj = object! {
        spa::utils::SpaTypes::ObjectParamPortConfig,
        spa::param::ParamType::EnumPortConfig,
        property!(
            RawSpaPodKey(spa::sys::SPA_PARAM_PORT_CONFIG_direction),
            pw::spa::pod::Value::Id(pw::spa::utils::Id(spa::sys::SPA_DIRECTION_INPUT))
        ),
        property!(
            RawSpaPodKey(spa::sys::SPA_PARAM_PORT_CONFIG_mode),
            pw::spa::pod::Value::Id(pw::spa::utils::Id(
                spa::sys::SPA_PARAM_PORT_CONFIG_MODE_none
            ))
        ),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_PORT_CONFIG_monitor), Bool, false),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_PORT_CONFIG_control), Bool, false),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge enum port config pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

#[cfg(target_os = "linux")]
fn build_pipewire_bridge_port_config_pod() -> Result<Vec<u8>> {
    let obj = object! {
        spa::utils::SpaTypes::ObjectParamPortConfig,
        spa::param::ParamType::PortConfig,
        property!(
            RawSpaPodKey(spa::sys::SPA_PARAM_PORT_CONFIG_direction),
            pw::spa::pod::Value::Id(pw::spa::utils::Id(spa::sys::SPA_DIRECTION_INPUT))
        ),
        property!(
            RawSpaPodKey(spa::sys::SPA_PARAM_PORT_CONFIG_mode),
            pw::spa::pod::Value::Id(pw::spa::utils::Id(
                spa::sys::SPA_PARAM_PORT_CONFIG_MODE_none
            ))
        ),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_PORT_CONFIG_monitor), Bool, false),
        property!(RawSpaPodKey(spa::sys::SPA_PARAM_PORT_CONFIG_control), Bool, false),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge port config pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

#[cfg(target_os = "linux")]
fn build_pipewire_bridge_format_pod(
    config: &PipewireBridgeInputConfig,
    param_type: spa::param::ParamType,
) -> Result<Vec<u8>> {
    let obj = object! {
        spa::utils::SpaTypes::ObjectParamFormat,
        param_type,
        property!(spa::param::format::FormatProperties::MediaType, Id, spa::param::format::MediaType::Audio),
        property!(spa::param::format::FormatProperties::MediaSubtype, Id, spa::param::format::MediaSubtype::Iec958),
        property!(spa::param::format::FormatProperties::AudioFormat, Id, spa::param::audio::AudioFormat::Encoded),
        property!(spa::param::format::FormatProperties::AudioRate, Int, config.sample_rate_hz as i32),
        property!(spa::param::format::FormatProperties::AudioChannels, Int, config.channels as i32),
        property!(
            spa::param::format::FormatProperties::AudioIec958Codec,
            pw::spa::pod::Value::Id(pw::spa::utils::Id(
                spa::sys::SPA_AUDIO_IEC958_CODEC_TRUEHD
            ))
        ),
    };
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(obj),
    )
    .map_err(|e| anyhow!("Failed to serialize PipeWire bridge input format pod: {e:?}"))?
    .0
    .into_inner();
    Ok(values)
}

#[cfg(target_os = "linux")]
fn spa_param_info(id: u32, flags: u32) -> spa::sys::spa_param_info {
    spa::sys::spa_param_info {
        id,
        flags,
        user: 0,
        seq: 0,
        padding: [0; 4],
    }
}

#[cfg(target_os = "linux")]
fn process_live_bridge_chunk(
    tx: &mpsc::SyncSender<Result<DecoderMessage>>,
    bridge: &mut FormatBridgeBox,
    spdif_parser: &mut SpdifParser,
    chunk: &[u8],
    strict_mode: bool,
) -> (usize, usize) {
    let mut packet_count = 0usize;
    let mut frame_count = 0usize;
    spdif_parser.push_bytes(chunk);
    while let Some(packet) = spdif_parser.get_next_packet() {
        packet_count += 1;
        let decode_started_at = Instant::now();
        let result = bridge.push_packet(
            packet.payload.as_slice().into(),
            RInputTransport::Iec61937,
            packet.data_type,
        );
        let decode_time_ms = decode_started_at.elapsed().as_secs_f32() * 1000.0;
        if result.frames.is_empty() || !result.error_message.is_empty() || result.did_reset {
            log::info!(
                "PipeWire bridge packet: data_type=0x{:02X} payload_bytes={} frames={} reset={} error={}",
                packet.data_type,
                packet.payload.len(),
                result.frames.len(),
                result.did_reset,
                result.error_message
            );
        }

        if result.did_reset {
            if strict_mode && !result.error_message.is_empty() {
                let _ = tx.try_send(Err(anyhow!("{}", result.error_message)));
                return (packet_count, frame_count);
            }
            if strict_mode {
                let _ = tx.try_send(Ok(DecoderMessage::FlushRequest(DecodedSource::Bridge)));
            }
        }

        let frame_count_in_packet = result.frames.len().max(1) as f32;
        let per_frame_decode_time_ms = decode_time_ms / frame_count_in_packet;
        for frame in result.frames {
            frame_count += 1;
            let send_result = tx.try_send(Ok(DecoderMessage::AudioData(DecodedAudioData {
                source: DecodedSource::Bridge,
                frame,
                decode_time_ms: per_frame_decode_time_ms,
                sent_at: Instant::now(),
            })));
            if send_result.is_err() {
                break;
            }
        }
    }
    (packet_count, frame_count)
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
