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
//! - Beside the IEC 958 formats the sink takes linear PCM: audio on the fixed
//!   7.1 map, in which a burst carrier is recognised and taken as its words
//!   ([`carrier`](super::carrier)); or, when the config asks for a carrier
//!   sink, a carrier and nothing else, on as many channels as it says.
//!
//! The realtime `process` callback only copies: the buffer's bytes into a
//! byte ring, and one [`CycleStamp`] per cycle (its graph time, the byte count
//! at its end, the format, the epoch) into a stamp ring. A pump thread turns
//! those into the [`ReaderMsg`]s the engine thread already consumes for the
//! pipe, with the exact transport position attached.

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::thread;
use std::time::Duration;

use anyhow::{Result, anyhow};
use audio_input::pipewire_pods::{
    IEC958_AC3_CHANNELS, IEC958_CONTENT_RATES_HZ, IEC958_DTS_CHANNELS, IEC958_DTSHD_CHANNELS,
    IEC958_DTSHD_RATE_HZ, build_pipewire_bridge_carrier_format_pod_at,
    build_pipewire_bridge_carrier_properties, build_pipewire_bridge_codec_format_pod,
    build_pipewire_bridge_codec_format_pod_at_rates, build_pipewire_bridge_format_pod,
    build_pipewire_bridge_latency_pod, build_pipewire_bridge_quantum_buffers_pod,
    build_pipewire_bridge_raw_format_pod_at, build_pipewire_bridge_stream_properties,
};
use audio_rt::{Consumer, Producer, frame_ring};
use pipewire as pw;
use pw::spa;
use pw::spa::pod::Pod;

use super::carrier::{CarrierGate, Take};
use super::reader::{Capture, CarrierFormat, END_OF_STREAM_S, ReaderMsg, ReaderStats, Transport};

/// `SPA_IO_CLOCK_FLAG_XRUN_RECOVER | SPA_IO_CLOCK_FLAG_DISCONT`
/// (`spa/node/io.h`): the driver's clock restarted. Spelled out because the
/// `libspa-sys` bindings come from the system's headers, and those of the
/// PipeWire releases still supported (Ubuntu 22.04/24.04, CI included)
/// predate both flags. The bits are ABI; an older server never sets them.
const CLOCK_RESTARTED: u32 = (1 << 1) | (1 << 4);

/// How long the carrier goes on being counted while the player gives it
/// nothing: as long as a silence does not end the stream.
const IDLE_COUNT_NS: u64 = (END_OF_STREAM_S * 1e9) as u64;

/// The carrier's time between the cycles that bring data.
///
/// A player that misses a cycle (it was late, it was starved of the CPU)
/// leaves a hole on the carrier: the graph skips the cycle, or runs it with
/// nothing from the player, and what the player sends next comes that much
/// later. The hole is carrier time like any other. Left out of the count,
/// it read as the source having lost that time: a latency error of a whole
/// cycle, steered out over tens of seconds or realigned on.
#[derive(Debug, Default)]
struct CarrierGaps {
    /// Where the last cycle that brought data ended (graph time, ns).
    data_until_ns: Option<u64>,
}

impl CarrierGaps {
    /// A cycle starting at `t_ns` and `cycle_ns` long brought data: the
    /// carrier time since the last one that did (ns). Nothing before the
    /// first, and nothing for a silence of [`IDLE_COUNT_NS`] or more (a
    /// pause: the stream ends, and the next one starts its own count).
    fn data(&mut self, t_ns: u64, cycle_ns: u64) -> u64 {
        let gap = self
            .data_until_ns
            .map_or(0, |until| t_ns.saturating_sub(until));
        self.data_until_ns = Some(t_ns + cycle_ns);
        // (Cycles sit on the driver's grid: anything under half of one is
        // the grid's own rounding.)
        if gap * 2 < cycle_ns || gap >= IDLE_COUNT_NS {
            0
        } else {
            gap
        }
    }
}

/// One graph cycle as the sink saw it.
#[derive(Debug, Clone, Copy, Default)]
struct CycleStamp {
    /// Graph time of the cycle's first frame (`spa_io_clock.nsec`).
    t_ns: u64,
    /// The carrier's position in this epoch at the end of this cycle
    /// (bytes): what was captured, and what the time without data stood
    /// for ([`CarrierGaps`]).
    bytes_end: u64,
    /// Bytes captured in this epoch up to the end of this cycle.
    data_end: u64,
    /// Negotiated carrier: frames per second, channels and sample format.
    rate: u32,
    channels: u32,
    format: CarrierFormat,
    /// Bumped on every format change and discontinuity.
    epoch: u32,
}

use super::pcm::{PCM_CHANNELS, PCM_RATE_HZ};

/// Byte ring capacity: 4 s of the widest IEC 958 carrier (8 ch × 2 B ×
/// 192 kHz), which is 8 s of the sink's linear PCM.
const BYTE_RING: usize = 4 * 8 * 2 * 192_000;
const STAMP_RING: usize = 4096;

/// The byte ring of a sink: [`BYTE_RING`], or 4 s of the carrier it takes on
/// its linear PCM when that is more.
fn byte_ring(carrier_channels: Option<u32>) -> usize {
    let carrier = carrier_channels.unwrap_or(0) as usize;
    BYTE_RING.max(4 * carrier * std::mem::size_of::<f32>() * PCM_RATE_HZ as usize)
}

/// What the realtime callback shares with the main loop.
#[derive(Default)]
struct Shared {
    rate: AtomicU32,
    channels: AtomicU32,
    /// The negotiated format is linear PCM, not IEC 958.
    pcm: AtomicBool,
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
    /// A carrier sink: its linear PCM is this many unpositioned channels that
    /// carry bursts, and never audio. `None` is the sink that takes audio on
    /// the 7.1 map (and a carrier it recognises there).
    pub carrier_channels: Option<u32>,
}

/// Start the driver, the sink and the pump; returns the message stream the
/// engine thread consumes, as for the pipe.
pub fn spawn_own_sink(config: OwnSinkConfig) -> Result<Capture> {
    let (bytes_tx, bytes_rx) = frame_ring::<u8>(1, byte_ring(config.carrier_channels));
    let (stamps_tx, stamps_rx) = frame_ring::<CycleStamp>(1, STAMP_RING);
    let shared = Arc::new(Shared::default());
    let gate = CarrierGate::new(config.carrier_channels.is_some());
    let (ready_tx, ready_rx) = sync_channel::<Result<()>>(1);
    let (tx, rx) = sync_channel(1024);
    let stats = Arc::new(ReaderStats::default());
    let stop = Arc::new(AtomicBool::new(false));
    // From here the capture owns its threads: an early return stops and
    // joins the ones already started.
    let mut capture = Capture::new(rx, Arc::clone(&stats), Arc::clone(&stop));
    capture.advertise_latency_ms(config.latency_ns as f64 / 1e6);
    let latency = capture.advertised_latency();
    {
        let shared = Arc::clone(&shared);
        let stop = Arc::clone(&stop);
        capture.own(
            thread::Builder::new()
                .name("own-sink".into())
                .spawn(move || {
                    if let Err(e) = run_sink(
                        config, shared, bytes_tx, stamps_tx, &ready_tx, &stop, &latency,
                    ) {
                        let _ = ready_tx.send(Err(e));
                    }
                })?,
        );
    }
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e),
        Err(_) => return Err(anyhow!("own sink did not start within 5 s")),
    }
    capture.own(
        thread::Builder::new()
            .name("own-sink-pump".into())
            .spawn(move || pump(bytes_rx, stamps_rx, tx, &stats, &stop, gate))?,
    );
    Ok(capture)
}

/// Main loop of the sink thread: driver node, follower stream, then wait.
fn run_sink(
    config: OwnSinkConfig,
    shared: Arc<Shared>,
    mut bytes_tx: Producer<u8>,
    mut stamps_tx: Producer<CycleStamp>,
    ready: &SyncSender<Result<()>>,
    stop: &AtomicBool,
    latency_ns: &AtomicI64,
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
    if let Some(channels) = config.carrier_channels {
        build_pipewire_bridge_carrier_properties(&mut props, channels as u16);
    }
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
    let mut data_total: u64 = 0;
    let mut gaps = CarrierGaps::default();
    let mut last_epoch = 0u32;
    // A carrier sink whose volume or mute someone moved: put back from the
    // loop below (a carrier has no level; at any other it is not one).
    let carrier_sink = config.carrier_channels.is_some();
    let level_moved = std::rc::Rc::new(Cell::new(false));
    let level_moved_in_callback = std::rc::Rc::clone(&level_moved);

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
            let format_changed = param_shared.pcm.swap(!iec958, Ordering::Relaxed) == iec958;
            // Setting `node.force-rate` renegotiates, which lands here again
            // with the same format: only a real change is a new epoch, and
            // the property is only written when its value changes.
            let rate_changed =
                param_shared.rate.swap(info.rate(), Ordering::Relaxed) != info.rate();
            let channels_changed = param_shared
                .channels
                .swap(info.channels(), Ordering::Relaxed)
                != info.channels();
            if !rate_changed && !channels_changed && !format_changed {
                return;
            }
            log::info!(
                "own sink: format {} {} Hz × {} ch",
                if iec958 { "IEC958" } else { "raw" },
                info.rate(),
                info.channels()
            );
            param_shared.epoch.fetch_add(1, Ordering::Release);
            // Every carrier, the 48 kHz ones included: the sink asks for
            // 192 kHz by default, and a group left at that rate runs a 48 kHz
            // carrier (AC-3, DTS core, linear PCM) four times too fast.
            let force = info.rate();
            if forced_for_param.get() != Some(force) {
                forced_for_param.set(Some(force));
                pending_for_param.set(Some(force));
            }
        })
        .control_info(move |_, _, id, control| {
            if !carrier_sink || control.is_null() {
                return;
            }
            // SAFETY: PipeWire hands a valid control, with `n_values`
            // readable values, for the duration of the call.
            let values = unsafe {
                let control = &*control;
                if control.values.is_null() {
                    return;
                }
                std::slice::from_raw_parts(control.values, control.n_values as usize)
            };
            let unity = match id {
                spa::sys::SPA_PROP_volume | spa::sys::SPA_PROP_channelVolumes => 1.0,
                spa::sys::SPA_PROP_mute => 0.0,
                _ => return,
            };
            if values.iter().any(|value| *value != unity) {
                level_moved_in_callback.set(true);
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
            // (The cycle's length: frames of the graph's rate, and that rate
            // as a fraction of a second per frame.)
            let (t_ns, discont, cycle) = if io.is_null() {
                process_shared.no_clock.fetch_add(1, Ordering::Relaxed);
                (0, false, (0, 0, 1))
            } else {
                let clock = unsafe { &(*io).clock };
                let discont = clock.flags & CLOCK_RESTARTED != 0;
                (
                    clock.nsec,
                    discont,
                    (clock.duration, clock.rate.num, clock.rate.denom.max(1)),
                )
            };
            if discont {
                process_shared.epoch.fetch_add(1, Ordering::Release);
            }
            let epoch = process_shared.epoch.load(Ordering::Acquire);
            if epoch != last_epoch {
                last_epoch = epoch;
                bytes_total = 0;
                data_total = 0;
                gaps = CarrierGaps::default();
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
                data_total += pushed as u64;
                got_any = true;
            }
            if !got_any {
                process_shared.empty.fetch_add(1, Ordering::Relaxed);
                return;
            }
            // The carrier's time since the last cycle with data: cycles the
            // player missed, or the graph skipped for it.
            let (frames, num, denom) = cycle;
            let cycle_ns = (frames as f64 * f64::from(num) / f64::from(denom) * 1e9) as u64;
            let rate = process_shared.rate.load(Ordering::Relaxed);
            let sample_bytes = if process_shared.pcm.load(Ordering::Relaxed) {
                CarrierFormat::PcmF32
            } else {
                CarrierFormat::Iec958
            }
            .sample_bytes();
            let frame_bytes =
                u64::from(sample_bytes * process_shared.channels.load(Ordering::Relaxed));
            let idle_frames =
                (gaps.data(t_ns, cycle_ns) as f64 * 1e-9 * f64::from(rate)).round() as u64;
            bytes_total += idle_frames * frame_bytes;
            let _ = stamps_tx.push(&[CycleStamp {
                t_ns,
                bytes_end: bytes_total,
                data_end: data_total,
                rate: process_shared.rate.load(Ordering::Relaxed),
                channels: process_shared.channels.load(Ordering::Relaxed),
                format: if process_shared.pcm.load(Ordering::Relaxed) {
                    CarrierFormat::PcmF32
                } else {
                    CarrierFormat::Iec958
                },
                epoch,
            }]);
        })
        .register()
        .map_err(|e| anyhow!("own sink listener: {e:?}"))?;

    // 3. Formats, full-quantum buffers, connect. Linear PCM comes last: a
    // player that can pass its stream through finds an encoded format first.
    // It is offered at the renderer's own rate only (the sink does not
    // resample; a player at another rate converts on its side): on the fixed
    // 7.1 map, or for a carrier sink on its unpositioned channels.
    let enum_fmt = spa::param::ParamType::EnumFormat;
    let pcm_channels = config.carrier_channels.unwrap_or(PCM_CHANNELS);
    let pods: Vec<Vec<u8>> = vec![
        build_pipewire_bridge_quantum_buffers_pod(
            pcm_channels * std::mem::size_of::<f32>() as u32,
        )?,
        build_pipewire_bridge_format_pod(IEC958_DTSHD_RATE_HZ, 8, enum_fmt)?,
        build_pipewire_bridge_format_pod(IEC958_DTSHD_RATE_HZ, 2, enum_fmt)?,
        build_pipewire_bridge_codec_format_pod(
            spa::sys::SPA_AUDIO_IEC958_CODEC_DTSHD,
            IEC958_DTSHD_RATE_HZ,
            IEC958_DTSHD_CHANNELS,
            enum_fmt,
        )?,
        // AC-3 and the DTS core ride a carrier at the content's own rate:
        // 44.1 kHz and 32 kHz streams are taken as they are.
        build_pipewire_bridge_codec_format_pod_at_rates(
            spa::sys::SPA_AUDIO_IEC958_CODEC_AC3,
            &IEC958_CONTENT_RATES_HZ,
            IEC958_AC3_CHANNELS,
            enum_fmt,
        )?,
        build_pipewire_bridge_codec_format_pod_at_rates(
            spa::sys::SPA_AUDIO_IEC958_CODEC_DTS,
            &IEC958_CONTENT_RATES_HZ,
            IEC958_DTS_CHANNELS,
            enum_fmt,
        )?,
        match config.carrier_channels {
            Some(channels) => {
                build_pipewire_bridge_carrier_format_pod_at(PCM_RATE_HZ, channels as u16, enum_fmt)?
            }
            None => {
                build_pipewire_bridge_raw_format_pod_at(PCM_RATE_HZ, PCM_CHANNELS as u16, enum_fmt)?
            }
        },
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

    // The latency players see: the target, a setpoint. It is advertised
    // again only when the setpoint moves (see the loop below).
    let mut advertised_ns = config.latency_ns;
    let latency_pod = build_pipewire_bridge_latency_pod(advertised_ns)?;
    if let Some(pod) = Pod::from_bytes(&latency_pod) {
        stream
            .update_params(&mut [pod])
            .map_err(|e| anyhow!("advertising the latency: {e:?}"))?;
    }
    shared.ready.store(true, Ordering::Relaxed);
    let _ = ready.send(Ok(()));
    log::info!(
        "own sink: {} in group {group}, advertising {:.1} ms{}",
        config.node_name,
        config.latency_ns as f64 / 1e6,
        config
            .carrier_channels
            .map_or_else(String::new, |channels| format!(
                ", a carrier sink of {channels} channels"
            ))
    );

    let mut last_report = std::time::Instant::now();
    let mut level_restores = 0u64;
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
        if level_moved.take() {
            level_restores += 1;
            // (Said the first time and then ever less often: something that
            // keeps moving the level must not fill the log.)
            if level_restores.is_power_of_two() {
                log::warn!(
                    "own sink: the carrier sink's volume or mute was moved: put back to 100 %, \
                     unmuted (a carrier at another level is not one; {level_restores} time(s))"
                );
            }
            restore_level(&stream, config.carrier_channels.unwrap_or(1) as usize);
        }
        // The host moved the setpoint: a new target, or one the servo had to
        // raise. The player's A/V sync follows what the sink says.
        let wanted_ns = latency_ns.load(Ordering::Relaxed);
        if wanted_ns != advertised_ns {
            advertised_ns = wanted_ns;
            let pod = build_pipewire_bridge_latency_pod(wanted_ns)?;
            match Pod::from_bytes(&pod).map(|pod| stream.update_params(&mut [pod])) {
                Some(Ok(())) => {
                    log::info!("own sink: now advertising {:.1} ms", wanted_ns as f64 / 1e6)
                }
                Some(Err(e)) => log::warn!("own sink: advertising the latency: {e:?}"),
                None => log::warn!("own sink: invalid latency pod"),
            }
        }
        if stop.load(Ordering::Relaxed) || sys::ShutdownHandle::is_requested() {
            break;
        }
    }
    Ok(())
}

/// Put a carrier sink's level back: unmuted, and at 100 % on the overall
/// volume and on each channel's (two controls: either scales the samples).
///
/// One call for the three, through the C function: it takes `(id, count,
/// values)` triplets up to a 0, and the crate's `set_control` passes one
/// triplet without the 0, so the function reads on into whatever follows on
/// the stack (seen: the overall volume set to garbage, over and over).
fn restore_level(stream: &pw::stream::Stream, channels: usize) {
    let mut mute = [0.0f32];
    let mut volume = [1.0f32];
    let mut channel_volumes = vec![1.0f32; channels];
    // SAFETY: a valid stream; each triplet's pointer is valid for its count
    // for the duration of the call, and the list ends with the 0 the
    // function reads up to.
    let r = unsafe {
        pw::sys::pw_stream_set_control(
            stream.as_raw_ptr(),
            spa::sys::SPA_PROP_mute,
            mute.len() as u32,
            mute.as_mut_ptr(),
            spa::sys::SPA_PROP_volume,
            volume.len() as u32,
            volume.as_mut_ptr(),
            spa::sys::SPA_PROP_channelVolumes,
            channel_volumes.len() as u32,
            channel_volumes.as_mut_ptr(),
            0u32,
        )
    };
    if r < 0 {
        log::warn!("own sink: restoring the carrier sink's level: error {r}");
    }
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

/// Turn stamps and bytes into the engine thread's messages. `gate` says what
/// the sink's linear PCM is: audio, or the words of a burst carrier.
fn pump(
    mut bytes: Consumer<u8>,
    mut stamps: Consumer<CycleStamp>,
    tx: SyncSender<ReaderMsg>,
    stats: &ReaderStats,
    stop: &AtomicBool,
    mut gate: CarrierGate,
) {
    let mut stamp = [CycleStamp::default()];
    // The capture's epoch, whose byte count `bytes_end` runs in, and the
    // stream that is open, by what its bytes are. They differ: a pause ends
    // the stream, but the capture keeps its epoch and its count, and resumes
    // them; and linear PCM that turns into a carrier, or back, is another
    // stream of the same capture.
    let mut epoch: Option<u32> = None;
    let mut in_stream: Option<CarrierFormat> = None;
    let mut last_bytes_end = 0u64;
    let mut last_data_end = 0u64;
    // The capture's count where the open stream started: what the stream's
    // own count (`Transport::bytes_end`) starts from.
    let mut stream_base = 0u64;
    let mut last_data_at = std::time::Instant::now();
    while !stop.load(Ordering::Relaxed) {
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
        if epoch != Some(s.epoch) {
            // A new capture epoch counts its bytes from zero again.
            if in_stream.take().is_some() && tx.send(ReaderMsg::End).is_err() {
                return;
            }
            epoch = Some(s.epoch);
            last_bytes_end = 0;
            last_data_end = 0;
            gate.reset();
        }
        let n = s.data_end.saturating_sub(last_data_end) as usize;
        last_data_end = s.data_end;
        let mut chunk = vec![0u8; n];
        let got = bytes.read_up_to(&mut chunk);
        chunk.truncate(got);
        stats.bytes_read.fetch_add(got as u64, Ordering::Relaxed);
        // What the bytes are: an IEC 958 carrier as it comes; linear PCM as
        // the audio it is, or as the words of the carrier it holds.
        let (format, chunk) = match s.format {
            CarrierFormat::Iec958 => (Some(CarrierFormat::Iec958), chunk),
            CarrierFormat::PcmF32 => match gate.take(&chunk, s.channels, s.rate) {
                Take::Audio => (Some(CarrierFormat::PcmF32), chunk),
                Take::Words(words) => (Some(CarrierFormat::Iec958), words),
                // The carrier's time passes with nothing on it: the next
                // chunk says how much (`idled`), as after a cycle a player
                // gave nothing for.
                Take::Idle => continue,
                Take::Nothing => (None, Vec::new()),
            },
        };
        if in_stream.is_some() && in_stream != format {
            in_stream = None;
            if tx.send(ReaderMsg::End).is_err() {
                return;
            }
        }
        let Some(format) = format else {
            // Not a stream: the carrier's time passes all the same.
            last_bytes_end = s.bytes_end;
            continue;
        };
        if in_stream.is_none() {
            in_stream = Some(format);
            // (What the carrier idled before a stream is not the stream's.)
            last_bytes_end = s.bytes_end.saturating_sub(n as u64);
            stream_base = last_bytes_end;
            if tx.send(ReaderMsg::Start).is_err() {
                return;
            }
        }
        // Carrier that passed with nothing in it since the last chunk: the
        // player missed those cycles.
        let idled = s
            .bytes_end
            .saturating_sub(last_bytes_end)
            .saturating_sub(n as u64);
        last_bytes_end = s.bytes_end;
        last_data_at = std::time::Instant::now();
        let frame_bytes = s.format.sample_bytes() * s.channels.max(1);
        let frames = n as f64 / frame_bytes as f64;
        let t_end = s.t_ns as f64 * 1e-9 + frames / s.rate.max(1) as f64;
        // The stream is counted in the bytes it is handed over in: a word
        // takes half the bytes of the float it came as.
        let per_byte = u64::from(s.format.sample_bytes() / format.sample_bytes());
        let msg = ReaderMsg::Chunk {
            t: t_end,
            bytes: chunk,
            transport: Some(Transport {
                rate: s.rate,
                channels: s.channels,
                format,
                bytes_end: (s.bytes_end - stream_base) / per_byte,
            }),
            skipped: idled / per_byte,
        };
        if tx.send(msg).is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;

    const RATE: u32 = 48_000;

    fn stamp(t_ns: u64, bytes_end: u64, epoch: u32) -> CycleStamp {
        CycleStamp {
            t_ns,
            bytes_end,
            data_end: bytes_end,
            rate: RATE,
            channels: 2,
            format: CarrierFormat::Iec958,
            epoch,
        }
    }

    fn next(rx: &Receiver<ReaderMsg>) -> ReaderMsg {
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the pump answers")
    }

    fn chunk(msg: ReaderMsg) -> (f64, usize, u64) {
        match msg {
            ReaderMsg::Chunk {
                t,
                bytes,
                transport: Some(transport),
                ..
            } => (t, bytes.len(), transport.bytes_end),
            _ => panic!("expected a chunk with its transport"),
        }
    }

    /// A pause ends the stream, but the capture keeps its epoch and its byte
    /// count: when it resumes, only the new bytes are read and timed, and
    /// the new stream counts from where it started (reviewer's probe on
    /// #767: 4096 bytes, End, 4096 more at a cumulative 8192).
    #[test]
    fn a_pause_resumes_the_capture_count_where_it_was() {
        let (mut bytes_tx, bytes_rx) = frame_ring::<u8>(1, BYTE_RING);
        let (mut stamps_tx, stamps_rx) = frame_ring::<CycleStamp>(1, STAMP_RING);
        let (tx, rx) = sync_channel(64);
        let stats = Arc::new(ReaderStats::default());
        let pump_stats = Arc::clone(&stats);
        let stop = AtomicBool::new(false);
        let pump = thread::spawn(move || {
            pump(
                bytes_rx,
                stamps_rx,
                tx,
                &pump_stats,
                &stop,
                CarrierGate::new(false),
            )
        });

        bytes_tx.push(&[1; 4096]);
        stamps_tx.push(&[stamp(1_000_000_000, 4096, 7)]);
        assert!(matches!(next(&rx), ReaderMsg::Start));
        let (t, len, bytes_end) = chunk(next(&rx));
        assert_eq!((len, bytes_end), (4096, 4096));
        assert!((t - (1.0 + 1024.0 / RATE as f64)).abs() < 1e-9, "{t}");
        assert!(matches!(next(&rx), ReaderMsg::End), "the pause ends it");

        bytes_tx.push(&[2; 4096]);
        stamps_tx.push(&[stamp(2_000_000_000, 8192, 7)]);
        assert!(matches!(next(&rx), ReaderMsg::Start));
        let (t, len, bytes_end) = chunk(next(&rx));
        assert_eq!(len, 4096);
        assert_eq!(bytes_end, 4096, "the new stream counts from its start");
        assert!((t - (2.0 + 1024.0 / RATE as f64)).abs() < 1e-9, "{t}");

        // A new capture epoch ends the open stream and counts from zero.
        bytes_tx.push(&[3; 4096]);
        stamps_tx.push(&[stamp(3_000_000_000, 4096, 8)]);
        assert!(matches!(next(&rx), ReaderMsg::End));
        assert!(matches!(next(&rx), ReaderMsg::Start));
        let (_, len, bytes_end) = chunk(next(&rx));
        assert_eq!((len, bytes_end), (4096, 4096));

        drop(rx);
        stamps_tx.push(&[stamp(4_000_000_000, 4096, 9)]);
        pump.join().unwrap();
    }

    /// A cycle the player gave nothing for is carrier time: the position
    /// moves by its length, and the chunk that follows says how much of the
    /// carrier before it was idle. A silence longer than the one that ends a
    /// stream is not counted on, and nothing is before a stream has started.
    #[test]
    fn a_cycle_without_data_is_counted_on_the_carrier() {
        let cycle_ns = 21_333_333u64;
        let mut gaps = CarrierGaps::default();
        assert_eq!(gaps.data(0, cycle_ns), 0, "nothing before the first");
        assert_eq!(gaps.data(cycle_ns, cycle_ns), 0, "the next cycle follows");
        // One cycle skipped: the next data comes a cycle late.
        assert_eq!(gaps.data(3 * cycle_ns, cycle_ns), cycle_ns);
        // The grid's rounding is not a gap.
        assert_eq!(gaps.data(4 * cycle_ns + 3, cycle_ns), 0);
        // A silence that ends the stream is not counted on.
        assert_eq!(gaps.data(5 * cycle_ns + 1_000_000_000, cycle_ns), 0);
        assert_eq!(gaps.data(6 * cycle_ns + 1_000_000_000, cycle_ns), 0);

        let (mut bytes_tx, bytes_rx) = frame_ring::<u8>(1, BYTE_RING);
        let (mut stamps_tx, stamps_rx) = frame_ring::<CycleStamp>(1, STAMP_RING);
        let (tx, rx) = sync_channel(64);
        let stats = Arc::new(ReaderStats::default());
        let pump_stats = Arc::clone(&stats);
        let stop = AtomicBool::new(false);
        let pump = thread::spawn(move || {
            pump(
                bytes_rx,
                stamps_rx,
                tx,
                &pump_stats,
                &stop,
                CarrierGate::new(false),
            )
        });
        let skipped = |msg: ReaderMsg| match msg {
            ReaderMsg::Chunk {
                bytes,
                transport: Some(transport),
                skipped,
                ..
            } => (bytes.len(), transport.bytes_end, skipped),
            _ => panic!("expected a chunk with its transport"),
        };
        // A cycle of data, a cycle missed, a cycle of data.
        bytes_tx.push(&[1; 4096]);
        stamps_tx.push(&[stamp(1_000_000_000, 4096, 7)]);
        assert!(matches!(next(&rx), ReaderMsg::Start));
        assert_eq!(skipped(next(&rx)), (4096, 4096, 0));
        bytes_tx.push(&[2; 4096]);
        stamps_tx.push(&[CycleStamp {
            data_end: 8192,
            ..stamp(1_042_666_666, 12_288, 7)
        }]);
        assert_eq!(skipped(next(&rx)), (4096, 12_288, 4096));
        // The carrier idled, the stream ended, another starts: what idled
        // before it is not in its count.
        assert!(matches!(next(&rx), ReaderMsg::End));
        bytes_tx.push(&[3; 4096]);
        stamps_tx.push(&[CycleStamp {
            data_end: 12_288,
            ..stamp(3_000_000_000, 40_000, 7)
        }]);
        assert!(matches!(next(&rx), ReaderMsg::Start));
        assert_eq!(skipped(next(&rx)), (4096, 4096, 0));
        drop(rx);
        stamps_tx.push(&[stamp(4_000_000_000, 4096, 9)]);
        pump.join().unwrap();
    }

    /// Linear PCM that holds a burst carrier is another stream, handed over
    /// as the 16-bit words it carries and counted in them. A cycle of the
    /// carrier with nothing on it is not handed over: the chunk after it
    /// says how much of the carrier idled. When the bursts have stopped for
    /// the silence that ends a stream, the sink's audio is a stream again.
    #[test]
    fn linear_pcm_that_carries_bursts_is_a_stream_of_its_words() {
        const CHANNELS: u32 = 8;
        const CYCLE_FRAMES: u64 = 1024;
        // A cycle in the sink's floats, and in the carrier's words.
        const FLOATS: u64 = CYCLE_FRAMES * CHANNELS as u64 * 4;
        const WORDS: u64 = FLOATS / 2;
        let silence = vec![0u8; FLOATS as usize];
        let mut burst_words = Vec::new();
        spdif::write_iamf_burst(&mut burst_words, &[7; 300], WORDS as usize).unwrap();
        let burst: Vec<u8> = burst_words
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|w| (f32::from(i16::from_le_bytes(*w)) / 32768.0).to_le_bytes())
            .collect();

        let (mut bytes_tx, bytes_rx) = frame_ring::<u8>(1, BYTE_RING);
        let (mut stamps_tx, stamps_rx) = frame_ring::<CycleStamp>(1, STAMP_RING);
        let (tx, rx) = sync_channel(64);
        let stats = Arc::new(ReaderStats::default());
        let stop = AtomicBool::new(false);
        let pump = thread::spawn(move || {
            pump(
                bytes_rx,
                stamps_rx,
                tx,
                &stats,
                &stop,
                CarrierGate::new(false),
            )
        });
        let mut cycles = 0u64;
        let mut cycle = |samples: &[u8]| {
            cycles += 1;
            bytes_tx.push(samples);
            stamps_tx.push(&[CycleStamp {
                t_ns: 1_000_000_000 + cycles * 21_333_333,
                bytes_end: cycles * FLOATS,
                data_end: cycles * FLOATS,
                rate: RATE,
                channels: CHANNELS,
                format: CarrierFormat::PcmF32,
                epoch: 7,
            }]);
        };
        let chunk = |msg: ReaderMsg| match msg {
            ReaderMsg::Chunk {
                bytes,
                transport: Some(transport),
                skipped,
                ..
            } => (
                transport.format,
                bytes.len() as u64,
                transport.bytes_end,
                skipped,
            ),
            _ => panic!("expected a chunk with its transport"),
        };

        // The sink idling on silence: audio.
        cycle(&silence);
        assert!(matches!(next(&rx), ReaderMsg::Start));
        assert_eq!(chunk(next(&rx)), (CarrierFormat::PcmF32, FLOATS, FLOATS, 0));
        // A burst: the audio stream ends and the carrier's starts, counted
        // from here in words.
        cycle(&burst);
        assert!(matches!(next(&rx), ReaderMsg::End));
        assert!(matches!(next(&rx), ReaderMsg::Start));
        let msg = next(&rx);
        match &msg {
            ReaderMsg::Chunk { bytes, .. } => assert_eq!(*bytes, burst_words),
            _ => panic!("expected the burst"),
        }
        assert_eq!(chunk(msg), (CarrierFormat::Iec958, WORDS, WORDS, 0));
        // A cycle of nothing, then a burst: that cycle idled.
        cycle(&silence);
        cycle(&burst);
        assert_eq!(
            chunk(next(&rx)),
            (CarrierFormat::Iec958, WORDS, 3 * WORDS, WORDS)
        );
        // The player stops. Nothing is handed over while the carrier may
        // still resume; then the stream ends and the audio is back, counted
        // from its own start in floats.
        for _ in 0..23 {
            cycle(&silence);
        }
        assert!(matches!(next(&rx), ReaderMsg::End));
        assert!(matches!(next(&rx), ReaderMsg::Start));
        assert_eq!(chunk(next(&rx)), (CarrierFormat::PcmF32, FLOATS, FLOATS, 0));
        drop(rx);
        cycle(&silence);
        pump.join().unwrap();
    }
}
