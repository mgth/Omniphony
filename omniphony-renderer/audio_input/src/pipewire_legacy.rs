use crate::InputControl;
use crate::bridge::LiveBridgeIngestRuntime;
use crate::pipewire::PipewireBridgeStreamConfig;
use crate::pipewire_pods::{
    build_pipewire_bridge_adapter_properties, build_pipewire_bridge_capture_stream_properties,
    build_pipewire_bridge_format_pod, build_pipewire_bridge_stream_properties,
};
use anyhow::{Result, anyhow};
use pipewire as pw;
use pw::spa;
use pw::spa::pod::Pod;
use std::cell::{Cell, RefCell};
use std::ffi::{CStr, CString};
use std::os::raw::c_void;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

const LIVE_BRIDGE_LOG_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipewireBridgeBackendKind {
    PwAdapter,
    PwClientNode,
    PwExportedNode,
    PwStream,
    PwFilter,
}

pub struct BridgeCaptureUserData {
    pub rate_hz: u32,
    pub channels: u32,
    pub last_log_at: Instant,
    pub add_buffer_calls_since_log: usize,
    pub remove_buffer_calls_since_log: usize,
    pub drained_calls_since_log: usize,
    pub io_changed_calls_since_log: usize,
    pub first_process_logged: bool,
    pub first_buffer_layout_logged: bool,
    pub process_calls_since_log: usize,
    pub datas_empty_since_log: usize,
    pub data_missing_since_log: usize,
    pub zero_size_chunks_since_log: usize,
    pub oversized_chunks_since_log: usize,
    pub bytes_since_log: usize,
    pub buffers_since_log: usize,
    pub sync_buffers_since_log: usize,
    pub packets_since_log: usize,
    pub frames_since_log: usize,
    pub empty_polls_since_log: usize,
    pub callback_chunk_logs_remaining: usize,
    pub accumulate_buf: Vec<u8>,
    pub accumulate_count: usize,
    pub last_idle_trigger: Instant,
    pub dynamic_trigger_interval: Option<Duration>,
    pub last_pw_time_log_at: Instant,
    pub output_rate_adjust: Arc<AtomicU32>,
}

#[derive(Default)]
pub struct PwDriverTriggerSchedule {
    pub next_trigger_at: Option<Instant>,
    pub pending_reason: Option<&'static str>,
    pub trigger_calls_since_log: usize,
    pub trigger_errors_since_log: usize,
}

struct PipewireBridgeFilterUserData {
    input_control: Arc<InputControl>,
    config: PipewireBridgeStreamConfig,
    stop: Arc<AtomicBool>,
    ingest: RefCell<LiveBridgeIngestRuntime>,
    input_port: Cell<*mut c_void>,
    metrics: RefCell<BridgeCaptureUserData>,
}

struct PipewireBridgeAdapterState {
    hook: spa::sys::spa_hook,
    config: PipewireBridgeStreamConfig,
    bound: Cell<bool>,
    removed: Cell<bool>,
    errored: Cell<bool>,
    global_id: Cell<u32>,
    object_serial: RefCell<Option<String>>,
    node_name: RefCell<Option<String>>,
}

fn new_bridge_capture_metrics(rate_hz: u32, channels: u32) -> BridgeCaptureUserData {
    BridgeCaptureUserData {
        rate_hz,
        channels,
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
    }
}

pub fn run_pipewire_bridge_filter_backend(
    input_control: Arc<InputControl>,
    config: PipewireBridgeStreamConfig,
    stop: Arc<AtomicBool>,
    ingest: LiveBridgeIngestRuntime,
) -> Result<()> {
    pw::init();

    let mainloop = pw::main_loop::MainLoopRc::new(None)
        .map_err(|e| anyhow!("Failed to create PipeWire main loop: {e:?}"))?;

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

    let user_data = Box::new(PipewireBridgeFilterUserData {
        input_control,
        config: config.clone(),
        stop,
        ingest: RefCell::new(ingest),
        input_port: Cell::new(std::ptr::null_mut()),
        metrics: RefCell::new(new_bridge_capture_metrics(
            config.sample_rate_hz,
            config.channels as u32,
        )),
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
            "Publishing PipeWire bridge filter sink: node={} description={} channels={} rate={}Hz latency={} resample.disable=true",
            config.node_name,
            config.node_description,
            config.channels,
            config.sample_rate_hz,
            requested_latency
        );

        let format_values = build_pipewire_bridge_format_pod(
            config.sample_rate_hz,
            config.channels,
            spa::param::ParamType::EnumFormat,
        )?;
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

pub fn run_pipewire_bridge_adapter_backend<F>(
    input_control: Arc<InputControl>,
    config: PipewireBridgeStreamConfig,
    stop: Arc<AtomicBool>,
    ingest: LiveBridgeIngestRuntime,
    run_capture: F,
) -> Result<()>
where
    F: FnOnce(
        &pw::main_loop::MainLoopRc,
        &pw::core::CoreRc,
        Arc<AtomicBool>,
        Arc<InputControl>,
        PipewireBridgeStreamConfig,
        LiveBridgeIngestRuntime,
        pw::properties::PropertiesBox,
    ) -> Result<()>,
{
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

    let adapter_props = build_pipewire_bridge_adapter_properties(
        &config.node_name,
        &config.node_description,
        config.channels,
        &requested_latency,
    );
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
        "Publishing PipeWire bridge adapter sink: node={} description={} channels={} rate={}Hz latency={} factory.name=support.null-audio-sink resample.disable=true",
        config.node_name,
        config.node_description,
        config.channels,
        config.sample_rate_hz,
        requested_latency
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

    let capture_props = build_pipewire_bridge_capture_stream_properties(
        &config.node_name,
        &config.node_description,
        config.channels,
        &capture_target,
    );
    let capture_result = run_capture(
        &mainloop,
        &core,
        stop,
        input_control,
        config,
        ingest,
        capture_props,
    );

    unsafe {
        pw::sys::pw_proxy_destroy(adapter_proxy);
    }
    drop(adapter_state);
    capture_result
}

fn wait_for_pipewire_bridge_adapter_target(
    mainloop: &pw::main_loop::MainLoopRc,
    core: &pw::core::CoreRc,
    target_global_id: u32,
    config: &PipewireBridgeStreamConfig,
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

pub fn log_pipewire_bridge_idle_metrics(metrics: &mut BridgeCaptureUserData) {
    metrics.empty_polls_since_log += 1;
    let now = Instant::now();
    if now.duration_since(metrics.last_log_at) >= LIVE_BRIDGE_LOG_INTERVAL {
        log::debug!(
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

pub fn process_pipewire_bridge_chunk_metrics<F>(
    metrics: &mut BridgeCaptureUserData,
    chunk: &[u8],
    process_chunk: F,
) where
    F: FnOnce() -> (usize, usize),
{
    let has_spdif_sync = chunk.windows(4).any(|w| {
        u16::from_le_bytes([w[0], w[1]]) == 0xF872 && u16::from_le_bytes([w[2], w[3]]) == 0x4E1F
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
        log::debug!(
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
            log::debug!("PipeWire bridge ingest has audio buffers but no IEC61937 sync words yet");
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

fn process_pipewire_bridge_bytes(
    user_data: &PipewireBridgeFilterUserData,
    metrics: &mut BridgeCaptureUserData,
    chunk: &[u8],
) {
    process_pipewire_bridge_chunk_metrics(metrics, chunk, || {
        user_data.ingest.borrow_mut().process_chunk(chunk)
    });
}

pub unsafe fn spa_data_chunk_slice(data: &spa::sys::spa_data) -> Option<&[u8]> {
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

pub fn clone_spa_pod_bytes(param: *const spa::sys::spa_pod) -> Option<Vec<u8>> {
    if param.is_null() {
        return None;
    }
    let pod = unsafe { &*param };
    let total_size = std::mem::size_of::<spa::sys::spa_pod>() + pod.size as usize;
    Some(unsafe { std::slice::from_raw_parts(param.cast::<u8>(), total_size) }.to_vec())
}

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
        unsafe { std::ffi::CStr::from_ptr(error) }
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

unsafe extern "C" fn pipewire_bridge_adapter_proxy_destroy(data: *mut c_void) {
    let state = unsafe { &*(data as *mut PipewireBridgeAdapterState) };
    log::info!(
        "PipeWire bridge adapter proxy destroy: node={}",
        state.config.node_name
    );
}

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

unsafe extern "C" fn pipewire_bridge_adapter_proxy_removed(data: *mut c_void) {
    let state = unsafe { &*(data as *mut PipewireBridgeAdapterState) };
    state.removed.set(true);
    log::info!(
        "PipeWire bridge adapter proxy removed: node={}",
        state.config.node_name
    );
}

unsafe extern "C" fn pipewire_bridge_adapter_proxy_done(data: *mut c_void, seq: i32) {
    let state = unsafe { &*(data as *mut PipewireBridgeAdapterState) };
    log::info!(
        "PipeWire bridge adapter proxy done: node={} seq={}",
        state.config.node_name,
        seq
    );
}

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
            let value = unsafe { CStr::from_ptr(item.value) }
                .to_string_lossy()
                .into_owned();
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
