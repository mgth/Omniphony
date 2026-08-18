use crate::pipewire_pods::{
    IEC958_AC3_CHANNELS, IEC958_AC3_RATE_HZ, IEC958_CODECS_PROP, IEC958_DTS_CHANNELS,
    IEC958_DTS_RATE_HZ, IEC958_DTSHD_CHANNELS, IEC958_DTSHD_RATE_HZ,
    build_pipewire_bridge_buffers_pod, build_pipewire_bridge_codec_format_pod,
    build_pipewire_bridge_format_pod, build_pipewire_bridge_raw_buffers_pod,
    build_pipewire_bridge_raw_format_pod, build_pipewire_bridge_stream_properties,
};
use crate::{InputClockMode, InputControl};
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
/// Channel count of the linear-PCM alternative. The renderer's live input map
/// is a fixed 7.1 layout, so PCM is only offered in that shape.
const RAW_FALLBACK_CHANNELS: u16 = 8;
/// Rate the linear-PCM alternative prefers. The encoded carrier's rate is not a
/// sensible default here: a PCM client is desktop audio, which runs at 48 kHz.
const RAW_DEFAULT_RATE_HZ: u32 = 48_000;
/// IEC 61937 carries its bursts in a 16-bit container.
const IEC958_BYTES_PER_SAMPLE: usize = std::mem::size_of::<u16>();

/// Which PipeWire client implementation carries the bridge input.
///
/// The choice follows the input clock mode: an upstream-clocked graph needs the
/// raw `pw_client_node` client (no DRIVER flag), everything else uses `pw_stream`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PipewireBridgeBackendKind {
    PwClientNode,
    PwStream,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipewireBridgeStreamConfig {
    pub node_name: String,
    pub node_description: String,
    pub channels: u16,
    pub sample_rate_hz: u32,
    pub target_latency_ms: u32,
    pub clock_mode: InputClockMode,
}

fn bridge_stream_uses_driver(clock_mode: InputClockMode) -> bool {
    !matches!(clock_mode, InputClockMode::Upstream)
}

struct BridgeCaptureUserData {
    rate_hz: u32,
    channels: u32,
    negotiated_iec958: bool,
    /// Width of one sample in the negotiated format. IEC 61937 rides a 16-bit
    /// container, linear PCM here is `f32`, and every byte-to-frame conversion
    /// on this path depends on which one is live — including the pacer drain,
    /// which starves the output ring if it is told twice the frames arrived.
    bytes_per_sample: usize,
    observed_transport_frames: u32,
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
    queued_packets_since_log: usize,
    empty_polls_since_log: usize,
    callback_chunk_logs_remaining: usize,
    accumulate_buf: Vec<u8>,
    accumulate_count: usize,
    last_idle_trigger: Instant,
    dynamic_trigger_interval: Option<Duration>,
    last_pw_time_log_at: Instant,
    output_rate_adjust: Arc<AtomicU32>,
    /// Timestamp of the previous IEC958 chunk arrival (diagnostic).
    last_iec958_chunk_at: Option<Instant>,
    /// Timestamp of the previous bridge-plugin decode flush (diagnostic).
    last_bridge_decode_at: Option<Instant>,
    /// Pre-decode source-clock cumulative time, in microseconds. Incremented
    /// each input callback by `frames_in_buffer / rate_hz × 1e6` — i.e. how
    /// much wall-time the just-arrived chunk represents at the S/PDIF source
    /// clock. Smooth by construction (the IEC61937 stuffing keeps the
    /// subframe rate constant across compressed bursts), so this exposes the
    /// source clock free of decoder batching artefacts.
    input_clock_us_cumulative: f64,
    /// Registry-handed diagnostic handles, each holding `f64` bits.
    /// Replaces the previous individual atomics; new metrics added to the
    /// registry appear automatically in the Studio diag plot.
    diag_iec958_chunk_bytes: Arc<std::sync::atomic::AtomicU64>,
    diag_iec958_chunk_dt_us: Arc<std::sync::atomic::AtomicU64>,
    diag_iec958_decode_packets: Arc<std::sync::atomic::AtomicU64>,
    diag_iec958_decode_dt_us: Arc<std::sync::atomic::AtomicU64>,
    /// Published mirror of `input_clock_us_cumulative` (f64::to_bits).
    diag_input_clock_us: Arc<std::sync::atomic::AtomicU64>,
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

    // For audio/raw, PipeWire reports interleaved samples in `pw_time.size`, so
    // dividing by channels yields the transport-frame quantum. For encoded
    // IEC958 streams, the callback payload is the authoritative transport domain:
    // `pw_time.size` can reflect a doubled sample-domain quantum while each
    // delivered chunk still contains the real transport frame count.
    let (transport_frames, transport_source) =
        if user_data.negotiated_iec958 && user_data.observed_transport_frames > 0 {
            (user_data.observed_transport_frames as u64, "observed_chunk")
        } else {
            (
                (time.size / user_data.channels.max(1) as u64).max(1),
                "pw_time",
            )
        };
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
            "{} pw_time: rate={}/{} size={} transport_frames={} source={} queued={} buffered={} queued_buffers={} avail_buffers={} delay={} quantum_ms={:.3} trigger_ms={:.3} rate_adjust={:.6} correction={:.4}",
            log_prefix,
            time.rate.num,
            time.rate.denom,
            time.size,
            transport_frames,
            transport_source,
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

/// Run the sink that carries the bridge input.
///
/// The node advertises both IEC 61937 and linear PCM, so the negotiated media
/// subtype decides which consumer receives a buffer: `process_chunk` gets the
/// encoded bursts to deframe, `process_pcm` gets interleaved `f32` frames.
/// A client that never enables passthrough lands on the PCM path instead of
/// silently feeding a deframer that will find no sync word.
pub fn run_pipewire_bridge_input_stream<F, P>(
    input_control: Arc<InputControl>,
    config: PipewireBridgeStreamConfig,
    stop: Arc<AtomicBool>,
    process_chunk: F,
    process_pcm: P,
) -> Result<()>
where
    F: FnMut(&[u8]) -> (usize, usize) + 'static,
    P: FnMut(&[u8], u32, u32) + 'static,
{
    pw::init();
    let use_driver = bridge_stream_uses_driver(config.clock_mode);

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
        "Publishing PipeWire bridge input sink: node={} description={} channels={} rate={}Hz latency={} codecs={} resample.disable=true",
        config.node_name,
        config.node_description,
        config.channels,
        config.sample_rate_hz,
        requested_latency,
        IEC958_CODECS_PROP
    );

    let stream = pw::stream::StreamBox::new(&core, "omniphony-live-bridge-input", props)
        .map_err(|e| anyhow!("Failed to create PipeWire bridge input stream: {e:?}"))?;

    let stop_for_process = Arc::clone(&stop);
    let input_control_for_state = Arc::clone(&input_control);
    let input_control_for_param = Arc::clone(&input_control);
    let config_for_state = config.clone();
    let input_control_for_process = Arc::clone(&input_control);
    let trigger_schedule = Rc::new(RefCell::new(PwDriverTriggerSchedule::default()));
    let trigger_schedule_for_state = Rc::clone(&trigger_schedule);
    let trigger_schedule_for_process = Rc::clone(&trigger_schedule);
    let process_chunk = RefCell::new(process_chunk);
    let process_pcm = RefCell::new(process_pcm);

    let _listener = stream
        .add_local_listener_with_user_data(BridgeCaptureUserData {
            rate_hz: config.sample_rate_hz,
            channels: config.channels as u32,
            negotiated_iec958: false,
            bytes_per_sample: IEC958_BYTES_PER_SAMPLE,
            observed_transport_frames: 0,
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
            queued_packets_since_log: 0,
            empty_polls_since_log: 0,
            callback_chunk_logs_remaining: 8,
            accumulate_buf: Vec::new(),
            accumulate_count: 0,
            last_idle_trigger: Instant::now(),
            dynamic_trigger_interval: None,
            last_pw_time_log_at: Instant::now(),
            output_rate_adjust: input_control.output_rate_adjust_atomic(),
            last_iec958_chunk_at: None,
            last_bridge_decode_at: None,
            input_clock_us_cumulative: 0.0,
            diag_iec958_chunk_bytes: {
                let diag = input_control.diag_registry();
                diag.register("iec958_chunk_bytes", "Chunk bytes", "iec958", "B")
            },
            diag_iec958_chunk_dt_us: {
                let diag = input_control.diag_registry();
                diag.register("iec958_chunk_dt_us", "Chunk dt", "iec958", "us")
            },
            diag_iec958_decode_packets: {
                let diag = input_control.diag_registry();
                diag.register("iec958_decode_packets", "SPDIF packets/call", "iec958", "")
            },
            diag_iec958_decode_dt_us: {
                let diag = input_control.diag_registry();
                diag.register("iec958_decode_dt_us", "SPDIF parser dt", "iec958", "us")
            },
            diag_input_clock_us: {
                // Reuse the atomic owned by InputControl so the audio_output PI
                // and the diag registry observe the exact same value. Reg path
                // is register_external (idempotent re-bind) instead of register.
                let diag = input_control.diag_registry();
                let shared = input_control.input_clock_us_atomic();
                diag.register_external(
                    "input_clock_us",
                    "Source-clock cumulative",
                    "iec958",
                    "us",
                    Arc::clone(&shared),
                );
                shared
            },
        })
        .state_changed(move |_stream, _user_data, old, new| {
            log::info!("{} state changed: {:?} -> {:?}", log_prefix, old, new);
            if new == pw::stream::StreamState::Streaming {
                if use_driver {
                    log::info!("{} is now STREAMING — triggering initial driver cycle", log_prefix);
                    schedule_pw_stream_driver_trigger(
                        &trigger_schedule_for_state,
                        Duration::ZERO,
                        "state_changed_streaming",
                    );
                } else {
                    log::info!(
                        "{} is now STREAMING — upstream clock mode active (no DRIVER trigger)",
                        log_prefix
                    );
                }
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

            let is_iec958 =
                media_subtype == pw::spa::param::format::MediaSubtype::Iec958;
            user_data.negotiated_iec958 = is_iec958;
            // Byte-to-frame conversions below this point follow the negotiated
            // format, not the encoded container: keeping the 16-bit width for a
            // 32-bit PCM stream doubles every frame count derived from a chunk.
            user_data.bytes_per_sample = if is_iec958 {
                IEC958_BYTES_PER_SAMPLE
            } else {
                std::mem::size_of::<f32>()
            };
            // `spa_format_audio_raw_parse` parses by property key (AudioRate /
            // AudioChannels), not by subtype — so it also extracts a usable
            // `rate` and `channels` from an IEC958 format pod. We need that
            // because we offer two IEC958 alternatives (2 ch and 8 ch) and
            // PipeWire picks one at runtime; without re-reading the
            // negotiated channel count, `user_data.channels` stays on the
            // config default (2) even when 8-channel streams arrive,
            // breaking byte→frame conversions that depend on stride.
            let mut format = pw::spa::param::audio::AudioInfoRaw::new();
            let parsed = format.parse(param).is_ok();
            if parsed {
                if format.rate() != 0 {
                    user_data.rate_hz = format.rate();
                    // The driver derives its trigger interval from this rate
                    // (quantum_frames / rate). Codecs ride different carriers —
                    // AC-3 at 48 kHz, TrueHD at the 4x one — so a rate captured
                    // once at connect makes the interval wrong by that ratio
                    // for every other codec, and the sink then drains the
                    // client far too fast: "Audio device underrun detected".
                    if use_driver {
                        input_control_for_param.register_direct_trigger_target(user_data.rate_hz);
                    }
                }
                if format.channels() != 0 {
                    user_data.channels = format.channels();
                }
            }
            if is_iec958 {
                log::info!(
                    "{} format negotiated: subtype=iec958 rate={}Hz channels={} (parsed={})",
                    log_prefix,
                    user_data.rate_hz,
                    user_data.channels,
                    parsed
                );
            } else if media_subtype == pw::spa::param::format::MediaSubtype::Raw {
                log::info!(
                    "{} format negotiated: subtype=raw rate={}Hz channels={} format={:?}",
                    log_prefix,
                    user_data.rate_hz,
                    user_data.channels,
                    format.format()
                );
            } else {
                log::info!(
                    "{} format negotiated: subtype={:?} rate={}Hz channels={}",
                    log_prefix,
                    media_subtype,
                    user_data.rate_hz,
                    user_data.channels
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
                if use_driver
                    && now.duration_since(user_data.last_idle_trigger)
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
                if use_driver
                    && now.duration_since(user_data.last_idle_trigger)
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
            let chunk_stride = data.chunk().stride().max(0) as usize;
            let byte_len = data.chunk().size() as usize;
            if user_data.negotiated_iec958 && byte_len > 0 {
                let bytes_per_transport_frame = chunk_stride.max(
                    user_data.channels as usize * user_data.bytes_per_sample,
                );
                let observed_transport_frames = (byte_len / bytes_per_transport_frame).max(1);
                user_data.observed_transport_frames =
                    observed_transport_frames.min(u32::MAX as usize) as u32;
            }
            let Some(bytes) = data.data() else {
                user_data.data_missing_since_log += 1;
                let now = Instant::now();
                if use_driver
                    && now.duration_since(user_data.last_idle_trigger)
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
                if use_driver
                    && now.duration_since(user_data.last_idle_trigger)
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
                if use_driver
                    && now.duration_since(user_data.last_idle_trigger)
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
                if use_driver
                    && now.duration_since(user_data.last_idle_trigger)
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
                    / (user_data.channels as f64 * user_data.bytes_per_sample as f64)
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
            // DIAG iec958-chain: per-chunk arrival trace. Publishes the chunk
            // size and inter-chunk interval to atomics so the Studio plot can
            // show whether the 1 Hz sawtooth already exists in the PipeWire
            // capture path. Kept as a log line too, for offline grep.
            let now_chunk = Instant::now();
            let dt_chunk_us = user_data
                .last_iec958_chunk_at
                .map(|prev| now_chunk.saturating_duration_since(prev).as_micros() as u64)
                .unwrap_or(0);
            user_data.last_iec958_chunk_at = Some(now_chunk);
            user_data
                .diag_iec958_chunk_bytes
                .store((byte_len as f64).to_bits(), Ordering::Relaxed);
            user_data
                .diag_iec958_chunk_dt_us
                .store((dt_chunk_us as f64).to_bits(), Ordering::Relaxed);
            // Pre-decode source-clock cumulative: each input callback delivers
            // `frames` S/PDIF subframes; convert to wall-time-at-source-clock
            // and accumulate. Smooth by construction (S/PDIF stuffing keeps
            // the subframe rate constant during compressed bursts).
            if user_data.channels > 0 && user_data.rate_hz > 0 {
                let frames_this_chunk = byte_len as f64
                    / (user_data.channels as f64 * user_data.bytes_per_sample as f64);
                let delta_us =
                    frames_this_chunk / user_data.rate_hz as f64 * 1_000_000.0;
                user_data.input_clock_us_cumulative += delta_us;
                user_data
                    .diag_input_clock_us
                    .store(user_data.input_clock_us_cumulative.to_bits(), Ordering::Relaxed);
            }
            // Pacer drain: for each IEC958 chunk that just arrived, drain a
            // proportional duration of rendered audio from pacer_fifo into
            // the ring buffer. Strict 1:1 between input-chunk duration and
            // ring-write duration → the ring sees a smooth stream regardless
            // of the decoder's burst pattern. Underrun → zero-fill the ring
            // (counted via the diag atomic). During pre-roll → also zero-fill
            // until pacer_fifo is primed.
            if user_data.channels > 0 && user_data.rate_hz > 0 {
                if let Some(pacer) = input_control_for_process.output_pacer() {
                    if pacer.enabled.load(Ordering::Relaxed) {
                        let in_subframes = byte_len as u64
                            / (user_data.channels as u64 * user_data.bytes_per_sample as u64);
                        let drain_samples = (in_subframes
                            .saturating_mul(pacer.out_sample_rate as u64)
                            .saturating_mul(pacer.out_channels as u64)
                            / (user_data.rate_hz as u64).max(1))
                            as usize;
                        // Single writer here (PipeWire input thread), so the
                        // diag read-modify-writes inside `drain` are race-free.
                        pacer.drain(drain_samples);
                    }
                }
            }
            user_data.bytes_since_log += byte_len;
            user_data.buffers_since_log += 1;
            if has_spdif_sync {
                user_data.sync_buffers_since_log += 1;
            }
            // Linear PCM was negotiated: hand the frames straight to the PCM
            // consumer. Deframing does not apply, and the accumulation window
            // below exists only to give the deframer whole bursts, so PCM must
            // not go through it — buffering four callbacks would add latency
            // for no benefit.
            if !user_data.negotiated_iec958 {
                process_pcm.borrow_mut()(chunk, user_data.channels, user_data.rate_hz);
                // FIXME(pcm-clock): this path has no proper clock yet.
                //
                // Every other trigger on this node fires on an *absent* buffer,
                // as an idle keepalive. Scheduling one here, after a buffer that
                // did arrive, makes the node pull as fast as the interval allows
                // and the input then arrives in bursts — measured at 0.005x then
                // 7.7x real time in consecutive seconds, which is the chopping.
                // Removing it is worse still: delivery collapses to 0.005x and
                // stays there, so something has to keep the node scheduled.
                // The encoded path gets away with the same heartbeat because its
                // producer only ever has a burst ready at the source rate; a PCM
                // client always has a full quantum queued, so the heartbeat
                // drains it faster than real time. Read `delivered_ratio` in the
                // stats line above: it must sit at 1.0, and today it does not.
                if use_driver {
                    schedule_pw_stream_driver_trigger(
                        &trigger_schedule_for_process,
                        current_pw_driver_trigger_interval(user_data),
                        "pcm_frames",
                    );
                }
                // The encoded path's periodic stats sit past this return, so
                // without a line of its own the PCM path reports nothing at all
                // — and a starving input is invisible precisely when it matters.
                // `delivered_ratio` is the figure to read: 1.0 means the sink is
                // pulling frames at exactly the rate the format declares.
                let now = Instant::now();
                if now.duration_since(user_data.last_log_at) >= LIVE_BRIDGE_LOG_INTERVAL {
                    let elapsed = now.duration_since(user_data.last_log_at).as_secs_f64();
                    let bytes_per_frame =
                        (user_data.channels as usize * user_data.bytes_per_sample).max(1);
                    let frames = user_data.bytes_since_log as f64 / bytes_per_frame as f64;
                    let frames_per_s = frames / elapsed.max(f64::MIN_POSITIVE);
                    log::debug!(
                        "{} pcm ingest: buffers={} frames/s={:.0} delivered_ratio={:.3} rate={}Hz channels={}",
                        log_prefix,
                        user_data.buffers_since_log,
                        frames_per_s,
                        frames_per_s / user_data.rate_hz.max(1) as f64,
                        user_data.rate_hz,
                        user_data.channels
                    );
                    user_data.last_log_at = now;
                    user_data.bytes_since_log = 0;
                    user_data.buffers_since_log = 0;
                }
                return;
            }
            user_data.accumulate_buf.extend_from_slice(chunk);
            user_data.accumulate_count += 1;
            let (packet_count, queued_count) =
                if user_data.accumulate_count >= PW_STREAM_ACCUMULATE_CALLBACKS {
                    let input_bytes = user_data.accumulate_buf.len();
                    let result = process_chunk.borrow_mut()(&user_data.accumulate_buf);
                    user_data.accumulate_buf.clear();
                    user_data.accumulate_count = 0;
                    // DIAG iec958-chain: per-bridge-decode trace. Publishes
                    // the decoded-packet count and inter-decode interval to
                    // atomics so the Studio plot can show whether the 1 Hz
                    // burst originates in the plugin. Kept as log too.
                    let now_decode = Instant::now();
                    let dt_decode_us = user_data
                        .last_bridge_decode_at
                        .map(|prev| {
                            now_decode.saturating_duration_since(prev).as_micros() as u64
                        })
                        .unwrap_or(0);
                    user_data.last_bridge_decode_at = Some(now_decode);
                    user_data
                        .diag_iec958_decode_packets
                        .store((result.0 as f64).to_bits(), Ordering::Relaxed);
                    user_data
                        .diag_iec958_decode_dt_us
                        .store((dt_decode_us as f64).to_bits(), Ordering::Relaxed);
                    let _ = input_bytes; // Was only used in the removed log line.
                    result
                } else {
                    (0, 0)
                };
            user_data.packets_since_log += packet_count;
            user_data.queued_packets_since_log += queued_count;
            let now = Instant::now();
            if now.duration_since(user_data.last_log_at) >= LIVE_BRIDGE_LOG_INTERVAL {
                log::debug!(
                    "{} ingest: buffers={} bytes={} sync_buffers={} packets={} queued={}",
                    log_prefix,
                    user_data.buffers_since_log,
                    user_data.bytes_since_log,
                    user_data.sync_buffers_since_log,
                    user_data.packets_since_log,
                    user_data.queued_packets_since_log
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
                user_data.queued_packets_since_log = 0;
                user_data.empty_polls_since_log = 0;
            }
            if use_driver {
                schedule_pw_stream_driver_trigger(
                    &trigger_schedule_for_process,
                    current_pw_driver_trigger_interval(user_data),
                    "post_process",
                );
            }
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

    // Advertise both formats with matching buffer pods.
    let format_2ch_bytes = build_pipewire_bridge_format_pod(
        config.sample_rate_hz,
        2,
        spa::param::ParamType::EnumFormat,
    )?;
    let format_8ch_bytes = build_pipewire_bridge_format_pod(
        config.sample_rate_hz,
        8,
        spa::param::ParamType::EnumFormat,
    )?;
    let format_2ch = Pod::from_bytes(&format_2ch_bytes)
        .ok_or_else(|| anyhow!("Invalid PipeWire 2ch format pod"))?;
    let format_8ch = Pod::from_bytes(&format_8ch_bytes)
        .ok_or_else(|| anyhow!("Invalid PipeWire 8ch format pod"))?;
    // AC-3 and the DTS core ride their own 48 kHz carrier, so each needs a
    // format of its own: the channel count cannot express them, and the two
    // pods above are both on the 4x carrier. DTS-HD shares that 4x carrier but
    // still needs stating, since the codec is not derivable from the channels.
    let format_ac3_bytes = build_pipewire_bridge_codec_format_pod(
        spa::sys::SPA_AUDIO_IEC958_CODEC_AC3,
        IEC958_AC3_RATE_HZ,
        IEC958_AC3_CHANNELS,
        spa::param::ParamType::EnumFormat,
    )?;
    let buffers_ac3_bytes =
        build_pipewire_bridge_buffers_pod(IEC958_AC3_CHANNELS, IEC958_AC3_RATE_HZ)?;
    let format_dtshd_bytes = build_pipewire_bridge_codec_format_pod(
        spa::sys::SPA_AUDIO_IEC958_CODEC_DTSHD,
        IEC958_DTSHD_RATE_HZ,
        IEC958_DTSHD_CHANNELS,
        spa::param::ParamType::EnumFormat,
    )?;
    let buffers_dtshd_bytes =
        build_pipewire_bridge_buffers_pod(IEC958_DTSHD_CHANNELS, IEC958_DTSHD_RATE_HZ)?;
    let format_dts_bytes = build_pipewire_bridge_codec_format_pod(
        spa::sys::SPA_AUDIO_IEC958_CODEC_DTS,
        IEC958_DTS_RATE_HZ,
        IEC958_DTS_CHANNELS,
        spa::param::ParamType::EnumFormat,
    )?;
    let buffers_dts_bytes =
        build_pipewire_bridge_buffers_pod(IEC958_DTS_CHANNELS, IEC958_DTS_RATE_HZ)?;
    let buffers_2ch_bytes = build_pipewire_bridge_buffers_pod(2, config.sample_rate_hz)?;
    let buffers_8ch_bytes = build_pipewire_bridge_buffers_pod(8, config.sample_rate_hz)?;
    let buffers_2ch = Pod::from_bytes(&buffers_2ch_bytes)
        .ok_or_else(|| anyhow!("Invalid PipeWire 2ch buffers pod"))?;
    let buffers_8ch = Pod::from_bytes(&buffers_8ch_bytes)
        .ok_or_else(|| anyhow!("Invalid PipeWire 8ch buffers pod"))?;
    // Linear-PCM alternative, offered last so PipeWire keeps preferring the
    // encoded formats for a passthrough client. It exists so the node is
    // discoverable at all: clients that build their device list from
    // EnumFormat drop a sink that advertises encoded formats only.
    let raw_format_bytes = build_pipewire_bridge_raw_format_pod(
        RAW_DEFAULT_RATE_HZ,
        RAW_FALLBACK_CHANNELS,
        spa::param::ParamType::EnumFormat,
    )?;
    let raw_buffers_bytes =
        build_pipewire_bridge_raw_buffers_pod(RAW_FALLBACK_CHANNELS, RAW_DEFAULT_RATE_HZ)?;
    let raw_format = Pod::from_bytes(&raw_format_bytes)
        .ok_or_else(|| anyhow!("Invalid PipeWire raw format pod"))?;
    let raw_buffers = Pod::from_bytes(&raw_buffers_bytes)
        .ok_or_else(|| anyhow!("Invalid PipeWire raw buffers pod"))?;
    let format_ac3 = Pod::from_bytes(&format_ac3_bytes)
        .ok_or_else(|| anyhow!("Invalid PipeWire AC-3 format pod"))?;
    let buffers_ac3 = Pod::from_bytes(&buffers_ac3_bytes)
        .ok_or_else(|| anyhow!("Invalid PipeWire AC-3 buffers pod"))?;
    let format_dtshd = Pod::from_bytes(&format_dtshd_bytes)
        .ok_or_else(|| anyhow!("Invalid PipeWire DTS-HD format pod"))?;
    let buffers_dtshd = Pod::from_bytes(&buffers_dtshd_bytes)
        .ok_or_else(|| anyhow!("Invalid PipeWire DTS-HD buffers pod"))?;
    let format_dts = Pod::from_bytes(&format_dts_bytes)
        .ok_or_else(|| anyhow!("Invalid PipeWire DTS format pod"))?;
    let buffers_dts = Pod::from_bytes(&buffers_dts_bytes)
        .ok_or_else(|| anyhow!("Invalid PipeWire DTS buffers pod"))?;
    let mut params = [
        format_8ch,
        buffers_8ch,
        format_2ch,
        buffers_2ch,
        format_dtshd,
        buffers_dtshd,
        format_ac3,
        buffers_ac3,
        format_dts,
        buffers_dts,
        raw_format,
        raw_buffers,
    ];

    let mut stream_flags =
        pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS;
    if use_driver {
        stream_flags |= pw::stream::StreamFlags::DRIVER;
    }

    stream
        .connect(
            spa::utils::Direction::Input,
            None,
            stream_flags,
            &mut params,
        )
        .map_err(|e| anyhow!("Failed to connect PipeWire bridge input stream: {e:?}"))?;
    log::info!(
        "{} sink connected: node={} node_id={}",
        log_prefix,
        config.node_name,
        stream.node_id()
    );

    if use_driver {
        input_control.register_direct_trigger_target(config.sample_rate_hz);
        log::info!(
            "{} registered direct trigger target: capture_rate={}Hz",
            log_prefix,
            config.sample_rate_hz
        );
    } else {
        log::info!(
            "{} using upstream clock mode: no DRIVER scheduling, waiting for graph-driven callbacks",
            log_prefix
        );
    }
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
        } else if use_driver {
            let _ = mainloop
                .loop_()
                .iterate(next_pw_stream_driver_timeout(&trigger_schedule));
            drain_scheduled_pw_stream_trigger(&stream, &trigger_schedule, log_prefix);
        } else {
            let _ = mainloop.loop_().iterate(Duration::from_millis(50));
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
