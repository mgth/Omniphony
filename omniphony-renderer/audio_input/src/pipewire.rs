use crate::InputControl;
use crate::pipewire_pods::{
    build_pipewire_bridge_buffers_pod, build_pipewire_bridge_format_pod,
    build_pipewire_bridge_stream_properties,
};
use anyhow::{Result, anyhow};
use pipewire as pw;
use pw::spa;
use pw::spa::pod::Pod;
use std::cell::RefCell;
use std::mem::MaybeUninit;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::time::{Duration, Instant};

const LIVE_BRIDGE_LOG_INTERVAL: Duration = Duration::from_secs(1);
const PW_STREAM_ACCUMULATE_CALLBACKS: usize = 4;
const PW_DRIVER_IDLE_TRIGGER_INTERVAL: Duration = Duration::from_millis(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipewireBridgeStreamConfig {
    pub node_name: String,
    pub node_description: String,
    pub channels: u16,
    pub sample_rate_hz: u32,
    pub target_latency_ms: u32,
}

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
    callback_chunk_logs_remaining: usize,
    accumulate_buf: Vec<u8>,
    accumulate_count: usize,
    last_idle_trigger: Instant,
    dynamic_trigger_interval: Option<Duration>,
    last_pw_time_log_at: Instant,
    output_rate_adjust: Arc<AtomicU32>,
}

#[derive(Default)]
struct PwDriverTriggerSchedule {
    next_trigger_at: Option<Instant>,
    pending_reason: Option<&'static str>,
    trigger_calls_since_log: usize,
    trigger_errors_since_log: usize,
}

fn current_pw_driver_trigger_interval(user_data: &BridgeCaptureUserData) -> Duration {
    user_data
        .dynamic_trigger_interval
        .unwrap_or(PW_DRIVER_IDLE_TRIGGER_INTERVAL)
}

fn current_direct_pw_driver_trigger_interval(input_control: &InputControl) -> Duration {
    let rate_hz = input_control.input_trigger_rate_hz().max(1) as u128;
    let quantum_frames = input_control.input_trigger_quantum_frames().max(1) as u128;
    let nanos = ((quantum_frames * 1_000_000_000u128) / rate_hz).max(500_000);
    Duration::from_nanos(nanos.min(u64::MAX as u128) as u64)
}

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

fn drain_direct_pw_stream_driver_trigger(
    stream: &pw::stream::Stream,
    pending: Option<&Arc<AtomicI64>>,
    next_trigger_at: &mut Option<Instant>,
    trigger_interval: Duration,
    log_prefix: &'static str,
) {
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

    let transport_frames = (time.size / user_data.channels.max(1) as u64).max(1);
    input_control
        .register_direct_trigger_quantum_frames(transport_frames.min(u32::MAX as u64) as u32);
    let quantum_ns = (transport_frames as u128 * time.rate.num as u128 * 1_000_000_000u128)
        / time.rate.denom as u128;
    let quantum_ns = quantum_ns.min(u64::MAX as u128) as u64;
    if quantum_ns == 0 {
        return;
    }

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

pub fn run_pipewire_bridge_input_stream<F>(
    input_control: Arc<InputControl>,
    config: PipewireBridgeStreamConfig,
    stop: Arc<AtomicBool>,
    process_chunk: F,
) -> Result<()>
where
    F: FnMut(&[u8]) -> (usize, usize) + 'static,
{
    pw::init();

    let log_prefix = "PipeWire bridge input";
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
    log::info!(
        "Publishing PipeWire bridge input sink: node={} description={} channels={} rate={}Hz latency={} resample.disable=true",
        config.node_name,
        config.node_description,
        config.channels,
        config.sample_rate_hz,
        requested_latency
    );

    let stream = pw::stream::StreamBox::new(&core, "omniphony-live-bridge-input", props)
        .map_err(|e| anyhow!("Failed to create PipeWire bridge input stream: {e:?}"))?;

    let stop_for_process = Arc::clone(&stop);
    let input_control_for_state = Arc::clone(&input_control);
    let config_for_state = config.clone();
    let input_control_for_process = Arc::clone(&input_control);
    let trigger_schedule = Rc::new(RefCell::new(PwDriverTriggerSchedule::default()));
    let trigger_schedule_for_state = Rc::clone(&trigger_schedule);
    let trigger_schedule_for_process = Rc::clone(&trigger_schedule);
    let process_chunk = RefCell::new(process_chunk);

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
            log::info!("{} state changed: {:?} -> {:?}", log_prefix, old, new);
            if new == pw::stream::StreamState::Streaming {
                log::info!("{} is now STREAMING — triggering initial driver cycle", log_prefix);
                schedule_pw_stream_driver_trigger(
                    &trigger_schedule_for_state,
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
                log::info!("{} format negotiated: subtype={:?}", log_prefix, media_subtype);
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
                log::debug!("{} first buffer layout: datas_len={}", log_prefix, datas.len());
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
            let (packet_count, frame_count) =
                if user_data.accumulate_count >= PW_STREAM_ACCUMULATE_CALLBACKS {
                    let result = process_chunk.borrow_mut()(&user_data.accumulate_buf);
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
    let buffers_values =
        build_pipewire_bridge_buffers_pod(config.channels, config.sample_rate_hz)?;
    let buffers_pod = Pod::from_bytes(&buffers_values)
        .ok_or_else(|| anyhow!("Invalid PipeWire buffers pod"))?;
    let mut params = [format_pod, buffers_pod];

    stream
        .connect(
            spa::utils::Direction::Input,
            None,
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
            let trigger_interval = current_direct_pw_driver_trigger_interval(input_control.as_ref());
            let _ = mainloop.loop_().iterate(next_direct_pw_stream_driver_timeout(
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
