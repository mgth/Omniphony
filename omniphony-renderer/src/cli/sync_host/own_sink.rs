//! The `own` source clock: orender's own PipeWire sink, clocked by orender's
//! own timer driver (spike S1, option c).
//!
//! - A `support.node.driver` node on `CLOCK_MONOTONIC` drives a private node
//!   group. The sink stream joins that group as a plain follower, so the
//!   player writing into it is paced by orender's clock, not by the DAC's,
//!   and sees an exact graph clock (`nsec = position·10⁹/rate`).
//! - The sink advertises a constant latency `L` once
//!   (`SPA_PARAM_Latency`), never a measurement.
//! - On an IEC 958 format at a carrier rate other than 48 kHz, the group's
//!   rate is forced to it (`node.force-rate`): otherwise a 192 kHz carrier
//!   runs at a quarter of real time.
//!
//! The realtime `process` callback only copies: the buffer's bytes into a
//! byte ring, and one [`CycleStamp`] per cycle (its graph time, the byte count
//! at its end, the format, the epoch) into a stamp ring. A pump thread turns
//! those into the [`ReaderMsg`]s the engine thread already consumes for the
//! pipe, with the exact transport position attached.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread;
use std::time::Duration;

use anyhow::{Result, anyhow};
use audio_input::pipewire_pods::{
    IEC958_AC3_CHANNELS, IEC958_AC3_RATE_HZ, IEC958_DTS_CHANNELS, IEC958_DTS_RATE_HZ,
    IEC958_DTSHD_CHANNELS, IEC958_DTSHD_RATE_HZ, build_pipewire_bridge_codec_format_pod,
    build_pipewire_bridge_format_pod, build_pipewire_bridge_iec958_quantum_buffers_pod,
    build_pipewire_bridge_latency_pod, build_pipewire_bridge_stream_properties,
};
use audio_rt::{Consumer, Producer, frame_ring};
use pipewire as pw;
use pw::spa;
use pw::spa::pod::Pod;

use super::reader::{END_OF_STREAM_S, ReaderMsg, ReaderStats, Transport};

/// `SPA_IO_CLOCK_FLAG_XRUN_RECOVER | SPA_IO_CLOCK_FLAG_DISCONT`
/// (`spa/node/io.h`): the driver's clock restarted. Spelled out because the
/// `libspa-sys` bindings come from the system's headers, and those of the
/// PipeWire releases still supported (Ubuntu 22.04/24.04, CI included)
/// predate both flags. The bits are ABI; an older server never sets them.
const CLOCK_RESTARTED: u32 = (1 << 1) | (1 << 4);

/// One graph cycle as the sink saw it.
#[derive(Debug, Clone, Copy, Default)]
struct CycleStamp {
    /// Graph time of the cycle's first frame (`spa_io_clock.nsec`).
    t_ns: u64,
    /// Bytes captured in this epoch up to the end of this cycle.
    bytes_end: u64,
    /// Negotiated carrier: frames per second and channels (2-byte samples).
    rate: u32,
    channels: u32,
    /// Bumped on every format change and discontinuity.
    epoch: u32,
}

/// Byte ring capacity: 4 s of the widest carrier (8 ch × 2 B × 192 kHz).
const BYTE_RING: usize = 4 * 8 * 2 * 192_000;
const STAMP_RING: usize = 4096;

/// What the realtime callback shares with the main loop.
#[derive(Default)]
struct Shared {
    rate: AtomicU32,
    channels: AtomicU32,
    epoch: AtomicU32,
    position: AtomicPtr<spa::sys::spa_io_position>,
    overflows: AtomicU64,
    ready: AtomicBool,
    /// Diagnostics: process calls, cycles without the clock, empty buffers.
    cycles: AtomicU64,
    no_clock: AtomicU64,
    empty: AtomicU64,
}

/// Configuration of the sink.
#[derive(Debug, Clone)]
pub struct OwnSinkConfig {
    pub node_name: String,
    pub description: String,
    /// Advertised end-to-end latency (ns).
    pub latency_ns: i64,
    /// Quantum requested for the group (frames at 48 kHz-family rates).
    pub quantum: u32,
    /// `priority.session`, if set.
    pub session_priority: Option<u32>,
}

/// Start the driver, the sink and the pump; returns the message stream the
/// engine thread consumes, as for the pipe.
pub fn spawn_own_sink(config: OwnSinkConfig) -> Result<(Receiver<ReaderMsg>, Arc<ReaderStats>)> {
    let (bytes_tx, bytes_rx) = frame_ring::<u8>(1, BYTE_RING);
    let (stamps_tx, stamps_rx) = frame_ring::<CycleStamp>(1, STAMP_RING);
    let shared = Arc::new(Shared::default());
    let (ready_tx, ready_rx) = sync_channel::<Result<()>>(1);
    {
        let shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("own-sink".into())
            .spawn(move || {
                if let Err(e) = run_sink(config, shared, bytes_tx, stamps_tx, &ready_tx) {
                    let _ = ready_tx.send(Err(e));
                }
            })?;
    }
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err(anyhow!("own sink did not start within 5 s")),
    }
    let (tx, rx) = sync_channel(1024);
    let stats = Arc::new(ReaderStats::default());
    {
        let stats = Arc::clone(&stats);
        thread::Builder::new()
            .name("own-sink-pump".into())
            .spawn(move || pump(bytes_rx, stamps_rx, tx, &stats))?;
    }
    Ok((rx, stats))
}

/// Main loop of the sink thread: driver node, follower stream, then wait.
fn run_sink(
    config: OwnSinkConfig,
    shared: Arc<Shared>,
    mut bytes_tx: Producer<u8>,
    mut stamps_tx: Producer<CycleStamp>,
    ready: &SyncSender<Result<()>>,
) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let group = format!("orender.own-clock.{}", std::process::id());

    // 1. The driver: PipeWire's own timer node on the monotonic clock.
    let driver_name = format!("{}-clock", config.node_name);
    let driver_props = pw::properties::properties! {
        "factory.name" => "support.node.driver",
        "node.name" => driver_name.as_str(),
        "node.description" => "orender own clock",
        "node.group" => group.as_str(),
        "priority.driver" => "0",
        "clock.id" => "monotonic",
        "node.freewheel" => "false",
    };
    let _driver = core
        .create_object::<pw::node::Node>("spa-node-factory", &driver_props)
        .map_err(|e| anyhow!("creating the own-clock driver: {e:?}"))?;

    // 2. The sink: today's properties, as a plain follower of that group.
    let latency = format!("{}/48000", config.quantum);
    let mut props = build_pipewire_bridge_stream_properties(
        &config.node_name,
        &config.description,
        8,
        IEC958_DTSHD_RATE_HZ,
        &latency,
    );
    props.insert("node.group", group.as_str());
    props.insert("node.always-process", "true");
    if let Some(p) = config.session_priority {
        props.insert("priority.session", p.to_string());
    }
    let stream = pw::stream::StreamBox::new(&core, "orender-own-sink", props)
        .map_err(|e| anyhow!("creating the own sink: {e:?}"))?;

    let param_shared = Arc::clone(&shared);
    let io_shared = Arc::clone(&shared);
    let process_shared = Arc::clone(&shared);
    let pending_force_rate: Cell<Option<u32>> = Cell::new(None);
    let pending = std::rc::Rc::new(pending_force_rate);
    let pending_for_param = std::rc::Rc::clone(&pending);
    let forced_for_param: Cell<Option<u32>> = Cell::new(None);
    let mut bytes_total: u64 = 0;
    let mut last_epoch = 0u32;

    let _listener = stream
        .add_local_listener_with_user_data(())
        .param_changed(move |_, _, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Ok((media_type, subtype)) = spa::param::format_utils::parse_format(param) else {
                return;
            };
            if media_type != spa::param::format::MediaType::Audio {
                return;
            }
            let mut info = spa::param::audio::AudioInfoRaw::new();
            if info.parse(param).is_err() {
                return;
            }
            let iec958 = subtype == spa::param::format::MediaSubtype::Iec958;
            // Setting `node.force-rate` renegotiates, which lands here again
            // with the same format: only a real change is a new epoch, and
            // the property is only written when its value changes.
            let rate_changed =
                param_shared.rate.swap(info.rate(), Ordering::Relaxed) != info.rate();
            let channels_changed = param_shared
                .channels
                .swap(info.channels(), Ordering::Relaxed)
                != info.channels();
            if !rate_changed && !channels_changed {
                return;
            }
            log::info!(
                "own sink: format {} {} Hz × {} ch",
                if iec958 { "IEC958" } else { "raw" },
                info.rate(),
                info.channels()
            );
            param_shared.epoch.fetch_add(1, Ordering::Release);
            let force = if iec958 && info.rate() != 48_000 {
                info.rate()
            } else {
                0
            };
            if forced_for_param.get() != Some(force) {
                forced_for_param.set(Some(force));
                pending_for_param.set(Some(force));
            }
        })
        .io_changed(move |_, _, id, area, size| {
            if id == spa::sys::SPA_IO_Position {
                let usable = !area.is_null()
                    && size as usize >= std::mem::size_of::<spa::sys::spa_io_position>();
                io_shared.position.store(
                    if usable {
                        area.cast()
                    } else {
                        std::ptr::null_mut()
                    },
                    Ordering::Release,
                );
            }
        })
        .process(move |stream, _| {
            process_shared.cycles.fetch_add(1, Ordering::Relaxed);
            let io = process_shared.position.load(Ordering::Acquire);
            // SAFETY: the area stays valid until io_changed withdraws it,
            // which PipeWire never does while a cycle is running.
            let (t_ns, discont) = if io.is_null() {
                process_shared.no_clock.fetch_add(1, Ordering::Relaxed);
                (0, false)
            } else {
                let clock = unsafe { &(*io).clock };
                let discont = clock.flags & CLOCK_RESTARTED != 0;
                (clock.nsec, discont)
            };
            if discont {
                process_shared.epoch.fetch_add(1, Ordering::Release);
            }
            let epoch = process_shared.epoch.load(Ordering::Acquire);
            if epoch != last_epoch {
                last_epoch = epoch;
                bytes_total = 0;
            }
            // A player may queue several buffers per cycle: take them all, or
            // the rest backs up and the player is held to a fraction of real
            // time.
            let mut got_any = false;
            while let Some(mut buffer) = stream.dequeue_buffer() {
                let datas = buffer.datas_mut();
                let Some(d) = datas.first_mut() else { continue };
                let size = d.chunk().size() as usize;
                let offset = d.chunk().offset() as usize;
                let Some(bytes) = d.data() else { continue };
                let end = (offset + size).min(bytes.len());
                if end <= offset || t_ns == 0 {
                    continue;
                }
                let pushed = bytes_tx.push(&bytes[offset..end]);
                if pushed < end - offset {
                    process_shared.overflows.fetch_add(1, Ordering::Relaxed);
                }
                bytes_total += pushed as u64;
                got_any = true;
            }
            if !got_any {
                process_shared.empty.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let _ = stamps_tx.push(&[CycleStamp {
                t_ns,
                bytes_end: bytes_total,
                rate: process_shared.rate.load(Ordering::Relaxed),
                channels: process_shared.channels.load(Ordering::Relaxed),
                epoch,
            }]);
        })
        .register()
        .map_err(|e| anyhow!("own sink listener: {e:?}"))?;

    // 3. Formats (IEC 958 only for now), full-quantum buffers, connect.
    let enum_fmt = spa::param::ParamType::EnumFormat;
    let pods: Vec<Vec<u8>> = vec![
        build_pipewire_bridge_iec958_quantum_buffers_pod()?,
        build_pipewire_bridge_format_pod(IEC958_DTSHD_RATE_HZ, 8, enum_fmt)?,
        build_pipewire_bridge_format_pod(IEC958_DTSHD_RATE_HZ, 2, enum_fmt)?,
        build_pipewire_bridge_codec_format_pod(
            spa::sys::SPA_AUDIO_IEC958_CODEC_DTSHD,
            IEC958_DTSHD_RATE_HZ,
            IEC958_DTSHD_CHANNELS,
            enum_fmt,
        )?,
        build_pipewire_bridge_codec_format_pod(
            spa::sys::SPA_AUDIO_IEC958_CODEC_AC3,
            IEC958_AC3_RATE_HZ,
            IEC958_AC3_CHANNELS,
            enum_fmt,
        )?,
        build_pipewire_bridge_codec_format_pod(
            spa::sys::SPA_AUDIO_IEC958_CODEC_DTS,
            IEC958_DTS_RATE_HZ,
            IEC958_DTS_CHANNELS,
            enum_fmt,
        )?,
    ];
    let mut params: Vec<&Pod> = pods
        .iter()
        .map(|b| Pod::from_bytes(b).ok_or_else(|| anyhow!("invalid pod")))
        .collect::<Result<_>>()?;
    stream
        .connect(
            spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|e| anyhow!("connecting the own sink: {e:?}"))?;

    // The latency players see: the configured target, once.
    let latency_pod = build_pipewire_bridge_latency_pod(config.latency_ns)?;
    if let Some(pod) = Pod::from_bytes(&latency_pod) {
        stream
            .update_params(&mut [pod])
            .map_err(|e| anyhow!("advertising the latency: {e:?}"))?;
    }
    shared.ready.store(true, Ordering::Relaxed);
    let _ = ready.send(Ok(()));
    log::info!(
        "own sink: {} in group {group}, advertising {:.1} ms",
        config.node_name,
        config.latency_ns as f64 / 1e6
    );

    let mut last_report = std::time::Instant::now();
    loop {
        mainloop.loop_().iterate(Duration::from_millis(50));
        if last_report.elapsed() >= Duration::from_secs(5) {
            last_report = std::time::Instant::now();
            log::debug!(
                "own sink: cycles={} empty={} no_clock={} overflows={} epoch={}",
                shared.cycles.load(Ordering::Relaxed),
                shared.empty.load(Ordering::Relaxed),
                shared.no_clock.load(Ordering::Relaxed),
                shared.overflows.load(Ordering::Relaxed),
                shared.epoch.load(Ordering::Relaxed),
            );
        }
        if let Some(rate) = pending.take() {
            set_force_rate(&stream, rate);
        }
        if sys::ShutdownHandle::is_requested() {
            break;
        }
    }
    Ok(())
}

/// `node.force-rate` on the stream (0 clears it). The crate has no wrapper
/// for `pw_stream_update_properties`.
fn set_force_rate(stream: &pw::stream::Stream, rate: u32) {
    let value = rate.to_string();
    let props = pw::properties::properties! { "node.force-rate" => value.as_str() };
    // SAFETY: valid stream and dict for the duration of the call.
    let r = unsafe {
        pw::sys::pw_stream_update_properties(stream.as_raw_ptr(), props.dict().as_raw_ptr())
    };
    log::info!("own sink: node.force-rate = {rate} ({r})");
}

/// Turn stamps and bytes into the engine thread's messages.
fn pump(
    mut bytes: Consumer<u8>,
    mut stamps: Consumer<CycleStamp>,
    tx: SyncSender<ReaderMsg>,
    stats: &ReaderStats,
) {
    let mut stamp = [CycleStamp::default()];
    let mut in_stream: Option<u32> = None;
    let mut last_bytes_end = 0u64;
    let mut last_data_at = std::time::Instant::now();
    loop {
        if !stamps.read_exact(&mut stamp) {
            if in_stream.is_some() && last_data_at.elapsed().as_secs_f64() >= END_OF_STREAM_S {
                in_stream = None;
                if tx.send(ReaderMsg::End).is_err() {
                    return;
                }
            }
            thread::sleep(Duration::from_millis(5));
            continue;
        }
        let s = stamp[0];
        if in_stream != Some(s.epoch) {
            if in_stream.is_some() && tx.send(ReaderMsg::End).is_err() {
                return;
            }
            in_stream = Some(s.epoch);
            last_bytes_end = 0;
            if tx.send(ReaderMsg::Start).is_err() {
                return;
            }
        }
        let n = s.bytes_end.saturating_sub(last_bytes_end) as usize;
        let mut chunk = vec![0u8; n];
        let got = bytes.read_up_to(&mut chunk);
        chunk.truncate(got);
        last_bytes_end = s.bytes_end;
        last_data_at = std::time::Instant::now();
        stats.bytes_read.fetch_add(got as u64, Ordering::Relaxed);
        let frames = n as f64 / (2.0 * s.channels.max(1) as f64);
        let t_end = s.t_ns as f64 * 1e-9 + frames / s.rate.max(1) as f64;
        let msg = ReaderMsg::Chunk {
            t: t_end,
            bytes: chunk,
            transport: Some(Transport {
                rate: s.rate,
                channels: s.channels,
                bytes_end: s.bytes_end,
            }),
        };
        if tx.send(msg).is_err() {
            return;
        }
    }
}
