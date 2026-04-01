use crate::InputControl;
use crate::bridge::LiveBridgeIngestRuntime;
use crate::pipewire::PipewireBridgeStreamConfig;
use crate::pipewire_legacy::{
    BridgeCaptureUserData, clone_spa_pod_bytes, process_pipewire_bridge_chunk_metrics,
    spa_data_chunk_slice,
};
use crate::pipewire_pods::{
    build_pipewire_bridge_buffers_pod, build_pipewire_bridge_enum_port_config_pod,
    build_pipewire_bridge_format_pod, build_pipewire_bridge_io_buffers_pod,
    build_pipewire_bridge_latency_pod, build_pipewire_bridge_meta_pod,
    build_pipewire_bridge_port_config_pod, build_pipewire_bridge_process_latency_pod,
    build_pipewire_bridge_props_pod, build_pipewire_bridge_stream_properties,
    build_pipewire_bridge_tag_pod, spa_param_info,
};
use anyhow::{Result, anyhow};
use pipewire as pw;
use pw::spa;
use std::cell::RefCell;
use std::ffi::CString;
use std::os::raw::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

const PIPEWIRE_BRIDGE_NODE_MAX_BUFFERS: usize = 64;

#[allow(dead_code)]
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
    config: PipewireBridgeStreamConfig,
    stop: Arc<AtomicBool>,
    ingest: RefCell<LiveBridgeIngestRuntime>,
    metrics: RefCell<BridgeCaptureUserData>,
    format_configured: bool,
    started: bool,
    suspended: bool,
}

impl PipewireBridgeExportNode {
    fn new(
        input_control: Arc<InputControl>,
        config: PipewireBridgeStreamConfig,
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
            node_props: spa::sys::spa_dict { flags: 0, n_items: 7, items: std::ptr::null() },
            node_prop_items: [
                spa::sys::spa_dict_item { key: pw::keys::NODE_NAME.as_ptr().cast(), value: node_name_value.as_ptr() },
                spa::sys::spa_dict_item { key: pw::keys::NODE_DESCRIPTION.as_ptr().cast(), value: node_description_value.as_ptr() },
                spa::sys::spa_dict_item { key: pw::keys::MEDIA_NAME.as_ptr().cast(), value: media_name_value.as_ptr() },
                spa::sys::spa_dict_item { key: pw::keys::MEDIA_CLASS.as_ptr().cast(), value: b"Audio/Sink\0".as_ptr().cast() },
                spa::sys::spa_dict_item { key: pw::keys::MEDIA_TYPE.as_ptr().cast(), value: b"Audio\0".as_ptr().cast() },
                spa::sys::spa_dict_item { key: pw::keys::MEDIA_CATEGORY.as_ptr().cast(), value: b"Playback\0".as_ptr().cast() },
                spa::sys::spa_dict_item { key: pw::keys::MEDIA_ROLE.as_ptr().cast(), value: b"Movie\0".as_ptr().cast() },
            ],
            port_props: spa::sys::spa_dict { flags: 0, n_items: 5, items: std::ptr::null() },
            port_prop_items: [
                spa::sys::spa_dict_item { key: b"format.dsp\0".as_ptr().cast(), value: b"32 bit float mono audio\0".as_ptr().cast() },
                spa::sys::spa_dict_item { key: b"port.group\0".as_ptr().cast(), value: port_group_value.as_ptr() },
                spa::sys::spa_dict_item { key: pw::keys::PORT_NAME.as_ptr().cast(), value: port_name_value.as_ptr() },
                spa::sys::spa_dict_item { key: pw::keys::PORT_ALIAS.as_ptr().cast(), value: port_alias_value.as_ptr() },
                spa::sys::spa_dict_item { key: b"audio.channel\0".as_ptr().cast(), value: port_audio_channel_value.as_ptr() },
            ],
            monitor_port_props: spa::sys::spa_dict { flags: 0, n_items: 6, items: std::ptr::null() },
            monitor_port_prop_items: [
                spa::sys::spa_dict_item { key: b"format.dsp\0".as_ptr().cast(), value: b"32 bit float mono audio\0".as_ptr().cast() },
                spa::sys::spa_dict_item { key: b"port.monitor\0".as_ptr().cast(), value: b"true\0".as_ptr().cast() },
                spa::sys::spa_dict_item { key: b"port.group\0".as_ptr().cast(), value: monitor_port_group_value.as_ptr() },
                spa::sys::spa_dict_item { key: pw::keys::PORT_NAME.as_ptr().cast(), value: monitor_port_name_value.as_ptr() },
                spa::sys::spa_dict_item { key: pw::keys::PORT_ALIAS.as_ptr().cast(), value: monitor_port_alias_value.as_ptr() },
                spa::sys::spa_dict_item { key: b"audio.channel\0".as_ptr().cast(), value: monitor_port_audio_channel_value.as_ptr() },
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
            enum_format_bytes: build_pipewire_bridge_format_pod(config.sample_rate_hz, config.channels, spa::param::ParamType::EnumFormat)?,
            format_bytes: build_pipewire_bridge_format_pod(config.sample_rate_hz, config.channels, spa::param::ParamType::Format)?,
            props_param_bytes: build_pipewire_bridge_props_pod()?,
            meta_param_bytes: build_pipewire_bridge_meta_pod()?,
            latency_param_bytes: build_pipewire_bridge_latency_pod()?,
            process_latency_param_bytes: build_pipewire_bridge_process_latency_pod()?,
            tag_param_bytes: build_pipewire_bridge_tag_pod(spa::sys::SPA_DIRECTION_INPUT)?,
            monitor_tag_param_bytes: build_pipewire_bridge_tag_pod(spa::sys::SPA_DIRECTION_OUTPUT)?,
            buffers_param_bytes: build_pipewire_bridge_buffers_pod(config.channels, config.sample_rate_hz)?,
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
                callback_chunk_logs_remaining: 8,
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
        node.monitor_port_info.props = (&mut node.monitor_port_props as *mut spa::sys::spa_dict).cast();
        node.monitor_port_info.params = node.monitor_port_params.as_mut_ptr();
        node.monitor_port_info.n_params = node.monitor_port_params.len() as u32;
        unsafe { spa::sys::spa_hook_list_init(&mut node.hooks) };
        let node_ptr: *mut Self = &mut *node;
        node.iface.iface.type_ = spa::sys::SPA_TYPE_INTERFACE_Node.as_ptr().cast();
        node.iface.iface.version = spa::sys::SPA_VERSION_NODE;
        node.iface.iface.cb.funcs = (&PIPEWIRE_BRIDGE_EXPORTED_NODE_METHODS as *const spa::sys::spa_node_methods).cast();
        node.iface.iface.cb.data = node_ptr.cast();
        Ok(node)
    }

    fn update_port_params_for_format(&mut self, configured: bool) {
        self.format_configured = configured;
        self.node_info.change_mask = (spa::sys::SPA_NODE_CHANGE_MASK_FLAGS | spa::sys::SPA_NODE_CHANGE_MASK_PARAMS) as u64;
        self.node_info.flags = if configured { spa::sys::SPA_NODE_FLAG_RT as u64 } else { (spa::sys::SPA_NODE_FLAG_RT | spa::sys::SPA_NODE_FLAG_NEED_CONFIGURE) as u64 };
        self.port_info.change_mask = (spa::sys::SPA_PORT_CHANGE_MASK_PROPS | spa::sys::SPA_PORT_CHANGE_MASK_PARAMS) as u64;
        self.port_info.params = self.port_params.as_mut_ptr();
        self.port_info.n_params = self.port_params.len() as u32;
        self.monitor_port_info.change_mask = (spa::sys::SPA_PORT_CHANGE_MASK_PROPS | spa::sys::SPA_PORT_CHANGE_MASK_PARAMS) as u64;
        self.monitor_port_info.params = self.monitor_port_params.as_mut_ptr();
        self.monitor_port_info.n_params = self.monitor_port_params.len() as u32;
    }
}

pub fn run_pipewire_bridge_exported_node_backend(
    input_control: Arc<InputControl>,
    config: PipewireBridgeStreamConfig,
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
    let props = build_pipewire_bridge_stream_properties(
        &config.node_name,
        &config.node_description,
        config.channels,
        config.sample_rate_hz,
        &requested_latency,
    );
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
        "Publishing PipeWire bridge exported sink: node={} description={} channels={} rate={}Hz latency={} resample.disable=true",
        config.node_name,
        config.node_description,
        config.channels,
        config.sample_rate_hz,
        requested_latency
    );
    while !node.stop.load(Ordering::Relaxed)
        && !sys::ShutdownHandle::is_requested()
        && !sys::ShutdownHandle::is_restart_from_config_requested()
    {
        let _ = mainloop.loop_().iterate(Duration::from_millis(100));
    }
    unsafe { pw::sys::pw_proxy_destroy(proxy) };
    Ok(())
}

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

unsafe extern "C" fn pipewire_bridge_exported_node_add_listener(
    object: *mut c_void,
    listener: *mut spa::sys::spa_hook,
    events: *const spa::sys::spa_node_events,
    data: *mut c_void,
) -> i32 {
    let node = unsafe { &mut *(object as *mut PipewireBridgeExportNode) };
    unsafe { spa::sys::spa_hook_list_append(&mut node.hooks, listener, events.cast(), data.cast()) };
    if !events.is_null() {
        unsafe {
            if let Some(cb) = (*events).info { cb(data, &node.node_info); }
            if let Some(cb) = (*events).port_info {
                cb(data, spa::sys::SPA_DIRECTION_INPUT, 0, &node.port_info);
                cb(data, spa::sys::SPA_DIRECTION_OUTPUT, 0, &node.monitor_port_info);
            }
        }
    }
    0
}

unsafe extern "C" fn pipewire_bridge_exported_node_set_callbacks(
    _object: *mut c_void,
    _callbacks: *const spa::sys::spa_node_callbacks,
    _data: *mut c_void,
) -> i32 { 0 }

unsafe extern "C" fn pipewire_bridge_exported_node_sync(_object: *mut c_void, _seq: i32) -> i32 { 0 }

unsafe extern "C" fn pipewire_bridge_exported_node_enum_params(
    object: *mut c_void, seq: i32, id: u32, start: u32, max: u32, _filter: *const spa::sys::spa_pod,
) -> i32 {
    let node = unsafe { &mut *(object as *mut PipewireBridgeExportNode) };
    let mut count = 0u32;
    for index in start.. {
        let param_ptr = match id {
            x if x == spa::sys::SPA_PARAM_Props => if index == 0 { node.props_param_bytes.as_ptr().cast_mut().cast() } else { break },
            x if x == spa::sys::SPA_PARAM_EnumFormat => if index == 0 { node.enum_format_bytes.as_ptr().cast_mut().cast() } else { break },
            x if x == spa::sys::SPA_PARAM_Format => if index == 0 { node.format_bytes.as_ptr().cast_mut().cast() } else { break },
            x if x == spa::sys::SPA_PARAM_EnumPortConfig => if index == 0 { node.enum_port_config_bytes.as_ptr().cast_mut().cast() } else { break },
            x if x == spa::sys::SPA_PARAM_PortConfig => if index == 0 { node.port_config_bytes.as_ptr().cast_mut().cast() } else { break },
            x if x == spa::sys::SPA_PARAM_Latency => if index == 0 { node.latency_param_bytes.as_ptr().cast_mut().cast() } else { break },
            x if x == spa::sys::SPA_PARAM_ProcessLatency => if index == 0 { node.process_latency_param_bytes.as_ptr().cast_mut().cast() } else { break },
            x if x == spa::sys::SPA_PARAM_Tag => if index == 0 { node.tag_param_bytes.as_ptr().cast_mut().cast() } else { break },
            _ => return -libc::ENOENT,
        };
        let result = spa::sys::spa_result_node_params { id, index, next: index + 1, param: param_ptr };
        emit_pipewire_bridge_node_result(node, seq, &result);
        count += 1;
        if count >= max.max(1) { break; }
    }
    0
}

unsafe extern "C" fn pipewire_bridge_exported_node_set_param(
    object: *mut c_void, id: u32, _flags: u32, param: *const spa::sys::spa_pod,
) -> i32 {
    let node = unsafe { &mut *(object as *mut PipewireBridgeExportNode) };
    match id {
        x if x == spa::sys::SPA_PARAM_Format || x == spa::sys::SPA_PARAM_PortConfig => {
            if x == spa::sys::SPA_PARAM_Format {
                if let Some(bytes) = clone_spa_pod_bytes(param) { node.format_bytes = bytes; }
            } else if let Some(bytes) = clone_spa_pod_bytes(param) {
                node.port_config_bytes = bytes;
            }
            node.update_port_params_for_format(true);
            emit_pipewire_bridge_node_info(node);
            emit_pipewire_bridge_node_port_info(node);
            0
        }
        _ => 0,
    }
}

unsafe extern "C" fn pipewire_bridge_exported_node_set_io(
    object: *mut c_void, id: u32, data: *mut c_void, size: usize,
) -> i32 {
    let node = unsafe { &mut *(object as *mut PipewireBridgeExportNode) };
    match id {
        x if x == spa::sys::SPA_IO_Clock => {
            if !data.is_null() && size < std::mem::size_of::<spa::sys::spa_io_clock>() { return -libc::ENOSPC; }
            node.io_clock = data.cast();
            0
        }
        x if x == spa::sys::SPA_IO_Position => {
            if !data.is_null() && size < std::mem::size_of::<spa::sys::spa_io_position>() { return -libc::ENOSPC; }
            node.io_position = data.cast();
            0
        }
        _ => 0,
    }
}

unsafe extern "C" fn pipewire_bridge_exported_node_send_command(
    object: *mut c_void, command: *const spa::sys::spa_command,
) -> i32 {
    let node = unsafe { &mut *(object as *mut PipewireBridgeExportNode) };
    let command_id = if command.is_null() { u32::MAX } else { unsafe { (*command).body.body.id } };
    match command_id {
        x if x == spa::sys::SPA_NODE_COMMAND_Start => { node.started = true; node.suspended = false; }
        x if x == spa::sys::SPA_NODE_COMMAND_Pause => { node.started = false; }
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
    let _ = pipewire_bridge_node_command_name(command);
    0
}

unsafe extern "C" fn pipewire_bridge_exported_node_port_enum_params(
    object: *mut c_void, seq: i32, direction: spa::sys::spa_direction, port_id: u32, id: u32, start: u32, max: u32, _filter: *const spa::sys::spa_pod,
) -> i32 {
    if port_id != 0 { return -libc::EINVAL; }
    let node = unsafe { &mut *(object as *mut PipewireBridgeExportNode) };
    let mut count = 0u32;
    for index in start.. {
        let param_ptr = match id {
            x if x == spa::sys::SPA_PARAM_EnumFormat => if index == 0 { node.enum_format_bytes.as_ptr().cast_mut().cast() } else { break },
            x if x == spa::sys::SPA_PARAM_Meta => if index == 0 { node.meta_param_bytes.as_ptr().cast_mut().cast() } else { break },
            x if x == spa::sys::SPA_PARAM_IO => if index == 0 { node.io_buffers_param_bytes.as_ptr().cast_mut().cast() } else { break },
            x if x == spa::sys::SPA_PARAM_Format => if index == 0 { node.format_bytes.as_ptr().cast_mut().cast() } else { break },
            x if x == spa::sys::SPA_PARAM_Buffers => if index == 0 { node.buffers_param_bytes.as_ptr().cast_mut().cast() } else { break },
            x if x == spa::sys::SPA_PARAM_Latency => if index == 0 { node.latency_param_bytes.as_ptr().cast_mut().cast() } else { break },
            x if x == spa::sys::SPA_PARAM_Tag => if index == 0 {
                if direction == spa::sys::SPA_DIRECTION_INPUT { node.tag_param_bytes.as_ptr().cast_mut().cast() } else { node.monitor_tag_param_bytes.as_ptr().cast_mut().cast() }
            } else { break },
            _ => return -libc::ENOENT,
        };
        let result = spa::sys::spa_result_node_params { id, index, next: index + 1, param: param_ptr };
        emit_pipewire_bridge_node_result(node, seq, &result);
        count += 1;
        if count >= max.max(1) { break; }
    }
    0
}

unsafe extern "C" fn pipewire_bridge_exported_node_port_set_param(
    object: *mut c_void, direction: spa::sys::spa_direction, port_id: u32, id: u32, _flags: u32, param: *const spa::sys::spa_pod,
) -> i32 {
    if port_id != 0 { return -libc::EINVAL; }
    let node = unsafe { &mut *(object as *mut PipewireBridgeExportNode) };
    if direction == spa::sys::SPA_DIRECTION_OUTPUT { return 0; }
    if id != spa::sys::SPA_PARAM_Format { return -libc::ENOENT; }
    if let Some(bytes) = clone_spa_pod_bytes(param) { node.format_bytes = bytes; }
    node.update_port_params_for_format(true);
    emit_pipewire_bridge_node_info(node);
    emit_pipewire_bridge_node_port_info(node);
    0
}

unsafe extern "C" fn pipewire_bridge_exported_node_port_use_buffers(
    object: *mut c_void, direction: spa::sys::spa_direction, port_id: u32, flags: u32, buffers: *mut *mut spa::sys::spa_buffer, n_buffers: u32,
) -> i32 {
    if port_id != 0 { return -libc::EINVAL; }
    let node = unsafe { &mut *(object as *mut PipewireBridgeExportNode) };
    if direction == spa::sys::SPA_DIRECTION_OUTPUT { return 0; }
    if n_buffers as usize > PIPEWIRE_BRIDGE_NODE_MAX_BUFFERS { return -libc::ENOSPC; }
    if buffers.is_null() {
        node.n_buffers = 0;
        for slot in &mut node.owned_buffer_data { *slot = None; }
        return 0;
    }
    let port_bytes_per_frame = (node.config.channels as usize) * std::mem::size_of::<u16>();
    let nominal_frames = node.config.sample_rate_hz.div_ceil(100) as usize;
    let nominal_size = (port_bytes_per_frame * nominal_frames).max(1024);
    for index in 0..(n_buffers as usize) {
        let buffer = unsafe { *buffers.add(index) };
        node.buffers[index] = buffer;
        if buffer.is_null() { continue; }
        let spa_buffer = unsafe { &mut *buffer };
        if spa_buffer.n_datas == 0 || spa_buffer.datas.is_null() { continue; }
        let data0 = unsafe { &mut *spa_buffer.datas };
        if flags & spa::sys::SPA_NODE_BUFFERS_FLAG_ALLOC != 0 {
            let requested_size = if data0.maxsize == 0 { nominal_size } else { (data0.maxsize as usize).max(nominal_size) };
            let storage = node.owned_buffer_data[index].get_or_insert_with(|| vec![0u8; requested_size]);
            if storage.len() != requested_size { storage.resize(requested_size, 0); }
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
    0
}

unsafe extern "C" fn pipewire_bridge_exported_node_port_set_io(
    object: *mut c_void, direction: spa::sys::spa_direction, port_id: u32, id: u32, data: *mut c_void, size: usize,
) -> i32 {
    if port_id != 0 { return -libc::EINVAL; }
    let node = unsafe { &mut *(object as *mut PipewireBridgeExportNode) };
    if direction == spa::sys::SPA_DIRECTION_OUTPUT { return 0; }
    if id != spa::sys::SPA_IO_Buffers { return -libc::ENOENT; }
    if !data.is_null() && size < std::mem::size_of::<spa::sys::spa_io_buffers>() { return -libc::ENOSPC; }
    node.io_buffers = data.cast();
    0
}

unsafe extern "C" fn pipewire_bridge_exported_node_process(object: *mut c_void) -> i32 {
    let node = unsafe { &mut *(object as *mut PipewireBridgeExportNode) };
    if node.stop.load(Ordering::Relaxed) { return spa::sys::SPA_STATUS_NEED_DATA as i32; }
    let Some(io_buffers) = (unsafe { node.io_buffers.as_mut() }) else { return spa::sys::SPA_STATUS_NEED_DATA as i32; };
    if io_buffers.status as u32 != spa::sys::SPA_STATUS_HAVE_DATA { return spa::sys::SPA_STATUS_NEED_DATA as i32; }
    if io_buffers.buffer_id >= node.n_buffers { io_buffers.status = spa::sys::SPA_STATUS_NEED_DATA as i32; return spa::sys::SPA_STATUS_NEED_DATA as i32; }
    let buffer = node.buffers[io_buffers.buffer_id as usize];
    if buffer.is_null() { io_buffers.status = spa::sys::SPA_STATUS_NEED_DATA as i32; return spa::sys::SPA_STATUS_NEED_DATA as i32; }
    let spa_buffer = unsafe { &*buffer };
    if spa_buffer.n_datas == 0 || spa_buffer.datas.is_null() { io_buffers.status = spa::sys::SPA_STATUS_NEED_DATA as i32; return spa::sys::SPA_STATUS_NEED_DATA as i32; }
    let data0 = unsafe { &*spa_buffer.datas };
    let Some(chunk) = (unsafe { spa_data_chunk_slice(data0) }) else { io_buffers.status = spa::sys::SPA_STATUS_NEED_DATA as i32; return spa::sys::SPA_STATUS_NEED_DATA as i32; };
    let mut metrics = node.metrics.borrow_mut();
    metrics.process_calls_since_log += 1;
    if !metrics.first_process_logged { metrics.first_process_logged = true; }
    process_pipewire_bridge_chunk_metrics(&mut metrics, chunk, || node.ingest.borrow_mut().process_chunk(chunk));
    io_buffers.status = spa::sys::SPA_STATUS_NEED_DATA as i32;
    spa::sys::SPA_STATUS_NEED_DATA as i32
}

fn emit_pipewire_bridge_node_info(node: &mut PipewireBridgeExportNode) {
    unsafe {
        for_each_pipewire_bridge_node_listener(&mut node.hooks, |events, data| {
            if let Some(cb) = events.info { cb(data, &node.node_info); }
        });
    }
}

fn emit_pipewire_bridge_node_port_info(node: &mut PipewireBridgeExportNode) {
    unsafe {
        for_each_pipewire_bridge_node_listener(&mut node.hooks, |events, data| {
            if let Some(cb) = events.port_info {
                cb(data, spa::sys::SPA_DIRECTION_INPUT, 0, &node.port_info);
                cb(data, spa::sys::SPA_DIRECTION_OUTPUT, 0, &node.monitor_port_info);
            }
        });
    }
}

fn emit_pipewire_bridge_node_result(
    node: &mut PipewireBridgeExportNode, seq: i32, result: &spa::sys::spa_result_node_params,
) {
    unsafe {
        for_each_pipewire_bridge_node_listener(&mut node.hooks, |events, data| {
            if let Some(cb) = events.result {
                cb(data, seq, 0, spa::sys::SPA_RESULT_TYPE_NODE_PARAMS, (result as *const spa::sys::spa_result_node_params).cast());
            }
        });
    }
}

unsafe fn for_each_pipewire_bridge_node_listener<F>(
    hooks: &mut spa::sys::spa_hook_list, mut f: F,
) where F: FnMut(&spa::sys::spa_node_events, *mut c_void) {
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
