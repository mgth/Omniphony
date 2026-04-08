use crate::InputControl;
use crate::bridge::LiveBridgeIngestRuntime;
use crate::pipewire::PipewireBridgeStreamConfig;
use crate::pipewire_legacy::clone_spa_pod_bytes;
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
use std::ffi::{CStr, CString};
use std::mem::MaybeUninit;
use std::os::fd::RawFd;
use std::os::raw::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const TRUEHD_ONLY_IEC958_CODECS_PROP: &str = "[ \"TRUEHD\" ]";
const IEC958_AUDIO_POSITION_PROP: &str = "[ FL FR C LFE SL SR RL RR ]";

#[allow(dead_code)]
struct PipewireBridgeClientNodeState {
    hook: spa::sys::spa_hook,
    core_hook: spa::sys::spa_hook,
    proxy_hook: spa::sys::spa_hook,
    node_hook: spa::sys::spa_hook,
    client_node: *mut pw::sys::pw_client_node,
    input_control: Arc<InputControl>,
    config: PipewireBridgeStreamConfig,
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

#[allow(dead_code)]
struct PipewireBridgeMappedMem {
    id: u32,
    mem_type: u32,
    flags: u32,
    fd: RawFd,
    ptr: *mut c_void,
    size: usize,
}

#[repr(C)]
struct PipewireBridgePwNodeActivationState {
    status: u32,
    pending: i32,
}

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

pub fn run_pipewire_bridge_client_node_backend(
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
    let node_name_value = CString::new(config.node_name.clone()).expect("valid CString");
    let node_description_value =
        CString::new(config.node_description.clone()).expect("valid CString");
    let media_name_value = CString::new(config.node_description.clone()).expect("valid CString");
    let port_group_value = CString::new("stream.0").expect("valid CString");
    let port_name_value = CString::new("playback_FL").expect("valid CString");
    let port_alias_value =
        CString::new(format!("{}:playback_FL", config.node_description)).expect("valid CString");
    let port_audio_channel_value = CString::new("FL").expect("valid CString");
    let props = build_pipewire_bridge_stream_properties(
        &config.node_name,
        &config.node_description,
        config.channels,
        config.sample_rate_hz,
        &requested_latency,
    );
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
            spa_param_info(
                spa::sys::SPA_PARAM_Props,
                spa::sys::SPA_PARAM_INFO_READWRITE,
            ),
            spa_param_info(
                spa::sys::SPA_PARAM_EnumFormat,
                spa::sys::SPA_PARAM_INFO_READ,
            ),
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
            spa_param_info(
                spa::sys::SPA_PARAM_EnumFormat,
                spa::sys::SPA_PARAM_INFO_READ,
            ),
            spa_param_info(spa::sys::SPA_PARAM_Meta, spa::sys::SPA_PARAM_INFO_READ),
            spa_param_info(spa::sys::SPA_PARAM_IO, spa::sys::SPA_PARAM_INFO_READ),
            spa_param_info(spa::sys::SPA_PARAM_Format, spa::sys::SPA_PARAM_INFO_WRITE),
            spa_param_info(spa::sys::SPA_PARAM_Buffers, 0),
            spa_param_info(
                spa::sys::SPA_PARAM_Latency,
                spa::sys::SPA_PARAM_INFO_READWRITE,
            ),
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
            config.sample_rate_hz,
            config.channels,
            spa::param::ParamType::EnumFormat,
        )?,
        format_bytes: build_pipewire_bridge_format_pod(
            config.sample_rate_hz,
            config.channels,
            spa::param::ParamType::Format,
        )?,
        props_param_bytes: build_pipewire_bridge_props_pod()?,
        meta_param_bytes: build_pipewire_bridge_meta_pod()?,
        latency_param_bytes: build_pipewire_bridge_latency_pod()?,
        process_latency_param_bytes: build_pipewire_bridge_process_latency_pod()?,
        tag_param_bytes: build_pipewire_bridge_tag_pod(spa::sys::SPA_DIRECTION_INPUT)?,
        buffers_param_bytes: build_pipewire_bridge_buffers_pod(
            config.channels,
            config.sample_rate_hz,
        )?,
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
        state
            .io_buffers_param_bytes
            .as_ptr()
            .cast::<spa::sys::spa_pod>(),
        state.format_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state
            .buffers_param_bytes
            .as_ptr()
            .cast::<spa::sys::spa_pod>(),
        state
            .latency_param_bytes
            .as_ptr()
            .cast::<spa::sys::spa_pod>(),
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
        anyhow::bail!(
            "Failed to add PipeWire client-node listener: {}",
            add_listener_res
        );
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

    let pw_node = unsafe { pw_client_node_get_node_raw(client_node, pw::sys::PW_VERSION_NODE, 0) };
    if !pw_node.is_null() {
        let node_listener_res = unsafe {
            pw_node_add_listener_raw(
                pw_node,
                &mut state.node_hook,
                &node_events,
                state_ptr.cast(),
            )
        };
        log::debug!(
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
        state
            .latency_param_bytes
            .as_ptr()
            .cast::<spa::sys::spa_pod>(),
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
    log::debug!(
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
    log::debug!(
        "PipeWire bridge client-node port_update: node={} result={}",
        config.node_name,
        port_update_res
    );

    let active_res = unsafe { pw_client_node_set_active_raw(client_node, true) };
    log::debug!(
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
    let methods = unsafe { *(*iface).cb.funcs.cast::<pw::sys::pw_core_methods>() };
    let Some(create_object) = methods.create_object else {
        return std::ptr::null_mut();
    };
    unsafe {
        create_object(
            (*iface).cb.data,
            factory_name,
            type_name,
            version,
            props,
            user_data_size,
        )
    }
}

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
    let methods = unsafe { *(*iface).cb.funcs.cast::<pw::sys::pw_core_methods>() };
    let Some(add_listener) = methods.add_listener else {
        return -libc::ENOTSUP;
    };
    unsafe { add_listener((*iface).cb.data, listener, events, data) }
}

unsafe fn pw_client_node_add_listener_raw(
    client_node: *mut pw::sys::pw_client_node,
    listener: *mut spa::sys::spa_hook,
    events: *const pw::sys::pw_client_node_events,
    data: *mut c_void,
) -> i32 {
    let iface = client_node.cast::<spa::sys::spa_interface>();
    let methods = unsafe { *(*iface).cb.funcs.cast::<pw::sys::pw_client_node_methods>() };
    let Some(add_listener) = methods.add_listener else {
        return -libc::ENOTSUP;
    };
    unsafe { add_listener((*iface).cb.data, listener, events, data) }
}

unsafe fn pw_client_node_get_node_raw(
    client_node: *mut pw::sys::pw_client_node,
    version: u32,
    user_data_size: usize,
) -> *mut pw::sys::pw_node {
    let iface = client_node.cast::<spa::sys::spa_interface>();
    let methods = unsafe { *(*iface).cb.funcs.cast::<pw::sys::pw_client_node_methods>() };
    let Some(get_node) = methods.get_node else {
        return std::ptr::null_mut();
    };
    unsafe { get_node((*iface).cb.data, version, user_data_size) }
}

unsafe fn pw_client_node_update_raw(
    client_node: *mut pw::sys::pw_client_node,
    change_mask: u32,
    n_params: u32,
    params: *mut *const spa::sys::spa_pod,
    info: *const spa::sys::spa_node_info,
) -> i32 {
    let iface = client_node.cast::<spa::sys::spa_interface>();
    let methods = unsafe { *(*iface).cb.funcs.cast::<pw::sys::pw_client_node_methods>() };
    let Some(update) = methods.update else {
        return -libc::ENOTSUP;
    };
    unsafe { update((*iface).cb.data, change_mask, n_params, params, info) }
}

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
    let methods = unsafe { *(*iface).cb.funcs.cast::<pw::sys::pw_client_node_methods>() };
    let Some(port_update) = methods.port_update else {
        return -libc::ENOTSUP;
    };
    unsafe {
        port_update(
            (*iface).cb.data,
            direction,
            port_id,
            change_mask,
            n_params,
            params,
            info,
        )
    }
}

unsafe fn pw_client_node_set_active_raw(
    client_node: *mut pw::sys::pw_client_node,
    active: bool,
) -> i32 {
    let iface = client_node.cast::<spa::sys::spa_interface>();
    let methods = unsafe { *(*iface).cb.funcs.cast::<pw::sys::pw_client_node_methods>() };
    let Some(set_active) = methods.set_active else {
        return -libc::ENOTSUP;
    };
    unsafe { set_active((*iface).cb.data, active) }
}

unsafe fn pw_client_node_port_buffers_raw(
    client_node: *mut pw::sys::pw_client_node,
    direction: spa::sys::spa_direction,
    port_id: u32,
    mix_id: u32,
    n_buffers: u32,
    buffers: *mut *mut spa::sys::spa_buffer,
) -> i32 {
    let iface = client_node.cast::<spa::sys::spa_interface>();
    let methods = unsafe { *(*iface).cb.funcs.cast::<pw::sys::pw_client_node_methods>() };
    let Some(port_buffers) = methods.port_buffers else {
        return -libc::ENOTSUP;
    };
    unsafe {
        port_buffers(
            (*iface).cb.data,
            direction,
            port_id,
            mix_id,
            n_buffers,
            buffers,
        )
    }
}

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
        state
            .enum_port_config_bytes
            .as_ptr()
            .cast::<spa::sys::spa_pod>(),
        state.port_config_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state
            .latency_param_bytes
            .as_ptr()
            .cast::<spa::sys::spa_pod>(),
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
        state
            .io_buffers_param_bytes
            .as_ptr()
            .cast::<spa::sys::spa_pod>(),
        state.format_bytes.as_ptr().cast::<spa::sys::spa_pod>(),
        state
            .buffers_param_bytes
            .as_ptr()
            .cast::<spa::sys::spa_pod>(),
        state
            .latency_param_bytes
            .as_ptr()
            .cast::<spa::sys::spa_pod>(),
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
    log::debug!(
        "PipeWire bridge client-node refresh configured state: node={} configured={} update_res={} port_update_res={}",
        state.config.node_name,
        state.format_configured,
        node_res,
        port_res
    );
    if node_res < 0 { node_res } else { port_res }
}

fn pipewire_bridge_client_node_find_mem(
    state: &PipewireBridgeClientNodeState,
    mem_id: u32,
) -> Option<&PipewireBridgeMappedMem> {
    state.mapped_mems.iter().find(|mem| mem.id == mem_id)
}

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

fn pipewire_bridge_client_node_cleanup_mapped_mems(state: &mut PipewireBridgeClientNodeState) {
    for mem in state.mapped_mems.drain(..) {
        if !mem.ptr.is_null() && mem.size > 0 {
            unsafe { libc::munmap(mem.ptr, mem.size) };
        }
        if mem.fd >= 0 {
            unsafe { libc::close(mem.fd) };
        }
    }
    state.transport_ptr = std::ptr::null_mut();
    state.activation_ptr = std::ptr::null_mut();
    state.io_clock = std::ptr::null_mut();
    state.io_position = std::ptr::null_mut();
}

fn pipewire_bridge_client_node_mark_activation_ready(state: &mut PipewireBridgeClientNodeState) {
    const PW_VERSION_NODE_ACTIVATION: u32 = 1;
    const PW_NODE_ACTIVATION_FINISHED: u32 = 3;
    const PW_NODE_ACTIVATION_INACTIVE: u32 = 4;

    if state.activation_ptr.is_null()
        || state.activation_size < std::mem::size_of::<PipewireBridgePwNodeActivation>() as u32
    {
        return;
    }

    let activation = unsafe {
        &mut *(state
            .activation_ptr
            .cast::<PipewireBridgePwNodeActivation>())
    };

    unsafe {
        std::ptr::write_volatile(&mut activation.client_version, PW_VERSION_NODE_ACTIVATION);
        std::ptr::write_volatile(&mut activation.command, 0);
        std::ptr::write_volatile(&mut activation.status, PW_NODE_ACTIVATION_FINISHED);
        std::ptr::write_volatile(&mut activation.state[0].pending, 0);
        std::ptr::write_volatile(&mut activation.state[1].pending, 0);
        std::ptr::write_volatile(&mut activation.state[0].status, PW_NODE_ACTIVATION_INACTIVE);
        std::ptr::write_volatile(&mut activation.state[1].status, PW_NODE_ACTIVATION_FINISHED);
    }

    log::debug!(
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

unsafe extern "C" fn pipewire_bridge_client_node_core_info(
    _data: *mut c_void,
    _info: *const pw::sys::pw_core_info,
) {
}

unsafe extern "C" fn pipewire_bridge_client_node_core_done(
    _data: *mut c_void,
    _id: u32,
    _seq: i32,
) {
}

unsafe extern "C" fn pipewire_bridge_client_node_core_ping(
    _data: *mut c_void,
    _id: u32,
    _seq: i32,
) {
}

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
        unsafe { CStr::from_ptr(message) }
            .to_str()
            .unwrap_or("<invalid utf8>")
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

unsafe extern "C" fn pipewire_bridge_client_node_core_remove_id(_data: *mut c_void, _id: u32) {}

unsafe extern "C" fn pipewire_bridge_client_node_core_bound_id(
    _data: *mut c_void,
    _id: u32,
    _global_id: u32,
) {
}

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
            unsafe { libc::munmap(existing.ptr, existing.size) };
        }
        if existing.fd >= 0 {
            unsafe { libc::close(existing.fd) };
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
    log::debug!(
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

unsafe extern "C" fn pipewire_bridge_client_node_core_remove_mem(data: *mut c_void, id: u32) {
    let state = unsafe { &mut *(data as *mut PipewireBridgeClientNodeState) };
    if let Some(existing_idx) = state.mapped_mems.iter().position(|mem| mem.id == id) {
        let mem = state.mapped_mems.remove(existing_idx);
        if !mem.ptr.is_null() && mem.size > 0 {
            unsafe { libc::munmap(mem.ptr, mem.size) };
        }
        if mem.fd >= 0 {
            unsafe { libc::close(mem.fd) };
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
    log::debug!(
        "PipeWire bridge client-node core remove_mem: node={} id={}",
        state.config.node_name,
        id
    );
}

unsafe extern "C" fn pipewire_bridge_client_node_core_bound_props(
    _data: *mut c_void,
    _id: u32,
    _global_id: u32,
    _props: *const spa::sys::spa_dict,
) {
}

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
    log::trace!(
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

unsafe extern "C" fn pipewire_bridge_client_node_set_param(
    data: *mut c_void,
    id: u32,
    flags: u32,
    param: *const spa::sys::spa_pod,
) -> i32 {
    let state = unsafe { &mut *(data as *mut PipewireBridgeClientNodeState) };
    log::trace!(
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
            log::debug!(
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
    log::trace!(
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

unsafe extern "C" fn pipewire_bridge_client_node_event(
    data: *mut c_void,
    event: *const spa::sys::spa_event,
) -> i32 {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::trace!(
        "PipeWire bridge client-node event: node={} event={:p}",
        state.config.node_name,
        event
    );
    0
}

unsafe extern "C" fn pipewire_bridge_client_node_command(
    data: *mut c_void,
    command: *const spa::sys::spa_command,
) -> i32 {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::trace!(
        "PipeWire bridge client-node command: node={} command={:p}",
        state.config.node_name,
        command
    );
    0
}

unsafe extern "C" fn pipewire_bridge_client_node_add_port(
    data: *mut c_void,
    direction: spa::sys::spa_direction,
    port_id: u32,
    props: *const spa::sys::spa_dict,
) -> i32 {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::trace!(
        "PipeWire bridge client-node add_port: node={} direction={:?} port_id={} props={:p}",
        state.config.node_name,
        direction,
        port_id,
        props
    );
    0
}

unsafe extern "C" fn pipewire_bridge_client_node_remove_port(
    data: *mut c_void,
    direction: spa::sys::spa_direction,
    port_id: u32,
) -> i32 {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::trace!(
        "PipeWire bridge client-node remove_port: node={} direction={:?} port_id={}",
        state.config.node_name,
        direction,
        port_id
    );
    0
}

unsafe extern "C" fn pipewire_bridge_client_node_port_set_param(
    data: *mut c_void,
    direction: spa::sys::spa_direction,
    port_id: u32,
    id: u32,
    flags: u32,
    param: *const spa::sys::spa_pod,
) -> i32 {
    let state = unsafe { &mut *(data as *mut PipewireBridgeClientNodeState) };
    log::trace!(
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
    log::debug!(
        "PipeWire bridge client-node accepted port format: node={} port_id={} configured={} refresh_res={}",
        state.config.node_name,
        port_id,
        state.format_configured,
        refresh_res
    );
    refresh_res
}

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
    log::trace!(
        "PipeWire bridge client-node port_use_buffers: node={} direction={:?} port_id={} mix_id={} flags={} n_buffers={} buffers={:p}",
        state.config.node_name,
        direction,
        port_id,
        mix_id,
        flags,
        n_buffers,
        buffers
    );
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
        log::trace!(
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
    log::trace!(
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
    log::debug!(
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

unsafe extern "C" fn pipewire_bridge_client_node_port_set_mix_info(
    data: *mut c_void,
    direction: spa::sys::spa_direction,
    port_id: u32,
    mix_id: u32,
    peer_id: u32,
    props: *const spa::sys::spa_dict,
) -> i32 {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::trace!(
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

unsafe extern "C" fn pipewire_bridge_client_node_proxy_destroy(data: *mut c_void) {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::debug!(
        "PipeWire bridge client-node proxy destroy: node={}",
        state.config.node_name
    );
}

unsafe extern "C" fn pipewire_bridge_client_node_proxy_bound(data: *mut c_void, global_id: u32) {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::debug!(
        "PipeWire bridge client-node proxy bound: node={} global_id={}",
        state.config.node_name,
        global_id
    );
}

unsafe extern "C" fn pipewire_bridge_client_node_proxy_removed(data: *mut c_void) {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::debug!(
        "PipeWire bridge client-node proxy removed: node={}",
        state.config.node_name
    );
}

unsafe extern "C" fn pipewire_bridge_client_node_proxy_done(data: *mut c_void, seq: i32) {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::trace!(
        "PipeWire bridge client-node proxy done: node={} seq={}",
        state.config.node_name,
        seq
    );
}

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

unsafe extern "C" fn pipewire_bridge_client_node_proxy_bound_props(
    data: *mut c_void,
    global_id: u32,
    props: *const spa::sys::spa_dict,
) {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::trace!(
        "PipeWire bridge client-node proxy bound_props: node={} global_id={} props={:p}",
        state.config.node_name,
        global_id,
        props
    );
}

unsafe extern "C" fn pipewire_bridge_client_node_node_info(
    data: *mut c_void,
    info: *const pw::sys::pw_node_info,
) {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    if info.is_null() {
        log::debug!(
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
    log::debug!(
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

unsafe extern "C" fn pipewire_bridge_client_node_node_param(
    data: *mut c_void,
    seq: i32,
    id: u32,
    index: u32,
    next: u32,
    param: *const spa::sys::spa_pod,
) {
    let state = unsafe { &*(data as *mut PipewireBridgeClientNodeState) };
    log::trace!(
        "PipeWire bridge client-node node param: node={} seq={} id={} index={} next={} param_null={}",
        state.config.node_name,
        seq,
        id,
        index,
        next,
        param.is_null()
    );
}
