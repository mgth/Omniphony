//! PipeWire adapter for the [`OutputCore`].
//!
//! A playback `pw_stream` with `RT_PROCESS`: the process callback runs on the
//! data thread, reads the cycle's timing with one `pw_stream_get_time_n`, and
//! hands the buffer to the core. Everything the regulation needs comes from
//! that call (spike S2):
//!
//! - `now`: the cycle start on `CLOCK_MONOTONIC` (already smoothed by the
//!   driver's own DLL);
//! - `ticks`: the graph position at `now`, for the device-rate DLL;
//! - `delay` + `buffered`: with this cycle's `N`, a frame queued now is heard
//!   `(buffered + N)/fs + delay·rate` after `now` (the stream runs one cycle
//!   behind the graph). The DAC's own 1–3 ms is not reported by anyone.
//!
//! The adapter holds no regulation logic; it only translates.

use std::mem::MaybeUninit;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Result, anyhow};
use pipewire as pw;

use super::core::{DeviceTiming, OutputCore};
use crate::pipewire::{output_target_properties, to_pipewire_position};

/// What the stream is opened with.
#[derive(Debug, Clone)]
pub struct PipewireSyncOutputConfig {
    pub node_name: String,
    /// Pin to this sink (no move, no fallback); `None` follows the session
    /// manager's choice.
    pub target: Option<String>,
    /// Device channel count and, optionally, their positions (`FL`, `FR`, …).
    pub channels: u32,
    pub positions: Option<Vec<String>>,
    pub rate: u32,
    /// Processing quantum requested through `node.latency` (frames).
    pub quantum: u32,
}

/// A running stream. Dropping it stops the stream and joins its thread.
pub struct PipewireSyncOutput {
    shutdown: Arc<AtomicBool>,
    streaming: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl PipewireSyncOutput {
    /// Open the stream and start calling `core` from its data thread.
    pub fn start(config: PipewireSyncOutputConfig, core: OutputCore) -> Result<Self> {
        let shutdown = Arc::new(AtomicBool::new(false));
        let streaming = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<()>>();
        let thread = {
            let shutdown = Arc::clone(&shutdown);
            let streaming = Arc::clone(&streaming);
            thread::Builder::new()
                .name("pw-sync-output".into())
                .spawn(move || {
                    if let Err(e) = run(config, core, &shutdown, &streaming, &ready_tx) {
                        let _ = ready_tx.send(Err(e));
                    }
                })?
        };
        match ready_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(Ok(())) => Ok(Self {
                shutdown,
                streaming,
                thread: Some(thread),
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => {
                shutdown.store(true, Ordering::Relaxed);
                Err(anyhow!("PipeWire sync output did not start within 5 s"))
            }
        }
    }

    /// Whether the stream is in the STREAMING state.
    pub fn is_streaming(&self) -> bool {
        self.streaming.load(Ordering::Relaxed)
    }
}

impl Drop for PipewireSyncOutput {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct CallbackData {
    core: OutputCore,
    channels: usize,
    rate: f64,
}

fn run(
    config: PipewireSyncOutputConfig,
    core: OutputCore,
    shutdown: &AtomicBool,
    streaming: &Arc<AtomicBool>,
    ready: &std::sync::mpsc::Sender<Result<()>>,
) -> Result<()> {
    pw::init();
    // SAFETY: the thread loop is created, started and stopped on this thread
    // and outlives every object made from it below.
    let main_loop = unsafe { pw::thread_loop::ThreadLoopRc::new(Some("pw-sync-output"), None) }
        .map_err(|e| anyhow!("PipeWire thread loop: {e:?}"))?;
    let context = pw::context::ContextRc::new(&main_loop, None)
        .map_err(|e| anyhow!("PipeWire context: {e:?}"))?;
    let pw_core = context
        .connect_rc(None)
        .map_err(|e| anyhow!("PipeWire connect: {e:?}"))?;

    let mut props = pw::properties::PropertiesBox::new();
    props.insert("node.name", config.node_name.as_str());
    props.insert("media.type", "Audio");
    props.insert("media.category", "Playback");
    props.insert("media.name", "Omniphony spatial render");
    let latency = format!("{}/{}", config.quantum, config.rate);
    props.insert("node.latency", latency.as_str());
    let channels = config.channels.max(1);
    let channels_str = channels.to_string();
    props.insert("audio.channels", channels_str.as_str());
    let positions = config.positions.as_ref().map(|names| {
        names
            .iter()
            .map(|n| to_pipewire_position(n))
            .collect::<Vec<_>>()
            .join(",")
    });
    if let Some(p) = positions.as_deref() {
        props.insert("audio.position", p);
    }
    if let Some(target) = config.target.as_deref() {
        for (k, v) in output_target_properties(target) {
            props.insert(k, v);
        }
    }

    let stream = pw::stream::StreamBox::new(&pw_core, "omniphony-sync-output", props)
        .map_err(|e| anyhow!("PipeWire stream: {e:?}"))?;

    let streaming_flag = Arc::clone(streaming);
    let rate = config.rate as f64;
    let _listener = stream
        .add_local_listener_with_user_data(CallbackData {
            core,
            channels: channels as usize,
            rate,
        })
        .state_changed(move |_, _, old, new| {
            log::info!("PipeWire sync output: {old:?} -> {new:?}");
            streaming_flag.store(
                matches!(new, pw::stream::StreamState::Streaming),
                Ordering::Relaxed,
            );
        })
        .process(process)
        .register()
        .map_err(|e| anyhow!("PipeWire listener: {e:?}"))?;

    let mut info = pw::spa::param::audio::AudioInfoRaw::new();
    info.set_format(pw::spa::param::audio::AudioFormat::F32LE);
    info.set_rate(config.rate);
    info.set_channels(channels);
    let format = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(pw::spa::pod::Object {
            type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: pw::spa::param::ParamType::EnumFormat.as_raw(),
            properties: info.into(),
        }),
    )
    .map_err(|e| anyhow!("format pod: {e:?}"))?
    .0
    .into_inner();
    let pod = pw::spa::pod::Pod::from_bytes(&format).ok_or_else(|| anyhow!("format pod"))?;

    {
        let _lock = main_loop.lock();
        stream
            .connect(
                pw::spa::utils::Direction::Output,
                None,
                pw::stream::StreamFlags::AUTOCONNECT
                    | pw::stream::StreamFlags::MAP_BUFFERS
                    | pw::stream::StreamFlags::RT_PROCESS,
                &mut [pod],
            )
            .map_err(|e| anyhow!("PipeWire stream connect: {e:?}"))?;
    }
    main_loop.start();
    let _ = ready.send(Ok(()));

    while !shutdown.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(50));
    }
    main_loop.stop();
    Ok(())
}

/// The realtime callback: timing, then the core, then the chunk header.
fn process(stream: &pw::stream::Stream, data: &mut CallbackData) {
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let requested = buffer.requested() as usize;
    let mut time = MaybeUninit::<pw::sys::pw_time>::zeroed();
    // SAFETY: valid stream; the library writes at most `size_of::<pw_time>()`
    // bytes and the struct is zero-initialised either way.
    let time = unsafe {
        pw::sys::pw_stream_get_time_n(
            stream.as_raw_ptr(),
            time.as_mut_ptr(),
            std::mem::size_of::<pw::sys::pw_time>(),
        );
        time.assume_init()
    };
    let ch = data.channels;
    let datas = buffer.datas_mut();
    let Some(d) = datas.first_mut() else {
        return;
    };
    let written_frames = match d.data() {
        Some(slice) => {
            let capacity = slice.len() / (4 * ch);
            let frames = if requested > 0 {
                requested.min(capacity)
            } else {
                capacity
            };
            // SAFETY: the mapped buffer holds at least `capacity` frames of
            // `ch` f32 samples and PipeWire maps it suitably aligned.
            let dest = unsafe {
                std::slice::from_raw_parts_mut(slice.as_mut_ptr() as *mut f32, frames * ch)
            };
            if time.now <= 0 || time.rate.denom == 0 {
                // No timing yet (first cycles): nothing to regulate against.
                dest.fill(0.0);
                frames
            } else {
                let tick_s = time.rate.num as f64 / time.rate.denom as f64;
                let timing = DeviceTiming {
                    t_s: time.now as f64 * 1e-9,
                    position_frames: time.ticks as f64,
                    heard_delay_s: (time.buffered as f64 + frames as f64) / data.rate
                        + time.delay as f64 * tick_s,
                };
                data.core.process(timing, dest, ch)
            }
        }
        None => 0,
    };
    let chunk = d.chunk_mut();
    *chunk.offset_mut() = 0;
    *chunk.stride_mut() = (4 * ch) as i32;
    *chunk.size_mut() = (written_frames * 4 * ch) as u32;
}
