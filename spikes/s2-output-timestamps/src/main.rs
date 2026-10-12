//! Spike S2 — output timestamps on PipeWire.
//!
//! Plays DIGITAL SILENCE (all-zero buffers) to an explicitly targeted sink and
//! records, at every process callback, the CLOCK_MONOTONIC callback time, the
//! full `pw_time` from `pw_stream_get_time_n` (before dequeue and after
//! queue), and the driver's `spa_io_position.clock` read through the
//! `SPA_IO_Position` area handed to the stream in `io_changed`.
//!
//! Usage: s2-output-timestamps <target node.name> <quantum> <seconds> <out.csv>
//!        [expected clock-name prefix, default "api.alsa."]
//!
//! Throwaway code: not part of the product.

use pipewire as pw;
use pw::spa;
use std::ffi::CStr;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const RATE: u32 = 48_000;
const CHANNELS: u32 = 2;
const STRIDE: usize = 4 * CHANNELS as usize;

#[derive(Clone, Copy, Default)]
struct Rec {
    t_cb: u64,
    t_raw: u64,
    t_end: u64,
    // pw_time before dequeue
    now: i64,
    rate_num: u32,
    rate_den: u32,
    ticks: u64,
    delay: i64,
    queued: u64,
    buffered: u64,
    queued_buffers: u32,
    avail_buffers: u32,
    size: u64,
    // pw_time after queue
    queued_after: u64,
    delay_after: i64,
    // buffer
    requested: u64,
    written: u32,
    // driver clock (spa_io_position.clock)
    clk_nsec: u64,
    clk_next_nsec: u64,
    clk_position: u64,
    clk_duration: u64,
    clk_delay: i64,
    clk_rate_diff: f64,
    clk_rate_den: u32,
    clk_flags: u32,
    clk_cycle: u32,
    clk_xrun: u64,
    clk_id: u32,
}

struct Shared {
    recs: Mutex<Vec<Rec>>,
    position: AtomicPtr<spa::sys::spa_io_position>,
    clock_name: Mutex<String>,
    callbacks: AtomicU64,
    no_buffer: AtomicU64,
    stop: AtomicBool,
}

fn mono_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

fn raw_ns() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC_RAW, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

fn get_time(stream: *mut pw::sys::pw_stream) -> pw::sys::pw_time {
    let mut t = MaybeUninit::<pw::sys::pw_time>::zeroed();
    unsafe {
        pw::sys::pw_stream_get_time_n(stream, t.as_mut_ptr(), std::mem::size_of::<pw::sys::pw_time>());
        t.assume_init()
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!("usage: {} <target node.name> <quantum> <seconds> <out.csv> [clock prefix]", args[0]);
        std::process::exit(2);
    }
    let target = args[1].clone();
    let quantum: u32 = args[2].parse()?;
    let seconds: u64 = args[3].parse()?;
    let out = args[4].clone();
    let clock_prefix = args.get(5).cloned().unwrap_or_else(|| "api.alsa.".to_string());
    if target == "omniphony" || target.is_empty() {
        eprintln!("refusing to target '{target}'");
        std::process::exit(2);
    }

    let cap = (seconds as usize * RATE as usize / quantum as usize) * 2 + 4096;
    let shared = Arc::new(Shared {
        recs: Mutex::new(Vec::with_capacity(cap)),
        position: AtomicPtr::new(std::ptr::null_mut()),
        clock_name: Mutex::new(String::new()),
        callbacks: AtomicU64::new(0),
        no_buffer: AtomicU64::new(0),
        stop: AtomicBool::new(false),
    });

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;

    let node_name = format!("rwspike-s2-q{quantum}");
    let latency = format!("{quantum}/{RATE}");
    let mut props = pw::properties::PropertiesBox::new();
    props.insert("media.type", "Audio");
    props.insert("media.category", "Playback");
    props.insert("node.name", node_name.as_str());
    props.insert("node.description", "rework spike S2 (silence)");
    props.insert("node.latency", latency.as_str());
    props.insert("target.object", target.as_str());
    props.insert("node.dont-reconnect", "true");
    props.insert("node.dont-fallback", "true");
    props.insert("node.autoconnect", "true");
    props.insert("audio.channels", "2");
    let stream = pw::stream::StreamRc::new(core.clone(), &node_name, props)?;

    let sh_io = shared.clone();
    let sh_proc = shared.clone();
    let _listener = stream
        .add_local_listener_with_user_data(())
        .state_changed(|_, _, old, new| eprintln!("state {old:?} -> {new:?}"))
        .io_changed(move |_, _, id, area, size| {
            if id == spa::sys::SPA_IO_Position {
                eprintln!("io_changed Position area={area:p} size={size}");
                sh_io.position.store(area.cast(), Ordering::Release);
            }
        })
        .process(move |stream, _| {
            let t_cb = mono_ns();
            let t_raw = raw_ns();
            let raw = stream.as_raw_ptr();
            let mut r = Rec { t_cb, t_raw, ..Default::default() };
            let t = get_time(raw);
            r.now = t.now;
            r.rate_num = t.rate.num;
            r.rate_den = t.rate.denom;
            r.ticks = t.ticks;
            r.delay = t.delay;
            r.queued = t.queued;
            r.buffered = t.buffered;
            r.queued_buffers = t.queued_buffers;
            r.avail_buffers = t.avail_buffers;
            r.size = t.size;
            let pos = sh_proc.position.load(Ordering::Acquire);
            if !pos.is_null() {
                let c = unsafe { &(*pos).clock };
                r.clk_nsec = c.nsec;
                r.clk_next_nsec = c.next_nsec;
                r.clk_position = c.position;
                r.clk_duration = c.duration;
                r.clk_delay = c.delay;
                r.clk_rate_diff = c.rate_diff;
                r.clk_rate_den = c.rate.denom;
                r.clk_flags = c.flags;
                r.clk_cycle = c.cycle;
                r.clk_xrun = c.xrun;
                r.clk_id = c.id;
                if sh_proc.callbacks.load(Ordering::Relaxed) == 10 {
                    if let Ok(mut n) = sh_proc.clock_name.try_lock() {
                        let name = unsafe { CStr::from_ptr(c.name.as_ptr()) };
                        *n = name.to_string_lossy().into_owned();
                    }
                }
            }
            let b = unsafe { pw::sys::pw_stream_dequeue_buffer(raw) };
            if b.is_null() {
                sh_proc.no_buffer.fetch_add(1, Ordering::Relaxed);
            } else {
                unsafe {
                    let requested = (*b).requested;
                    r.requested = requested;
                    let sb = (*b).buffer;
                    let d = &mut *(*sb).datas;
                    let mut n_frames = 0usize;
                    if !d.data.is_null() {
                        let max_frames = d.maxsize as usize / STRIDE;
                        n_frames = if requested > 0 { (requested as usize).min(max_frames) } else { max_frames };
                        // DIGITAL SILENCE ONLY.
                        std::ptr::write_bytes(d.data as *mut u8, 0, n_frames * STRIDE);
                    }
                    let c = &mut *d.chunk;
                    c.offset = 0;
                    c.stride = STRIDE as i32;
                    c.size = (n_frames * STRIDE) as u32;
                    // pw_buffer.size is in app units: use frames so pw_time.queued is in frames.
                    (*b).size = n_frames as u64;
                    r.written = n_frames as u32;
                    pw::sys::pw_stream_queue_buffer(raw, b);
                }
            }
            let t2 = get_time(raw);
            r.queued_after = t2.queued;
            r.delay_after = t2.delay;
            r.t_end = mono_ns();
            sh_proc.callbacks.fetch_add(1, Ordering::Relaxed);
            if !sh_proc.stop.load(Ordering::Relaxed) {
                if let Ok(mut v) = sh_proc.recs.try_lock() {
                    if v.len() < v.capacity() {
                        v.push(r);
                    }
                }
            }
        })
        .register()?;

    let mut audio_info = spa::param::audio::AudioInfoRaw::new();
    audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
    audio_info.set_rate(RATE);
    audio_info.set_channels(CHANNELS);
    let mut position = [0; spa::param::audio::MAX_CHANNELS];
    position[0] = spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = spa::sys::SPA_AUDIO_CHANNEL_FR;
    audio_info.set_position(position);
    let values: Vec<u8> = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(spa::pod::Object {
            type_: spa::sys::SPA_TYPE_OBJECT_Format,
            id: spa::sys::SPA_PARAM_EnumFormat,
            properties: audio_info.into(),
        }),
    )
    .unwrap()
    .0
    .into_inner();
    let mut params = [spa::pod::Pod::from_bytes(&values).unwrap()];
    stream.connect(
        spa::utils::Direction::Output,
        None,
        pw::stream::StreamFlags::AUTOCONNECT
            | pw::stream::StreamFlags::MAP_BUFFERS
            | pw::stream::StreamFlags::RT_PROCESS,
        &mut params,
    )?;

    // Quit on SIGINT/SIGTERM.
    let ml = mainloop.clone();
    let _sigint = mainloop.loop_().add_signal_local(pw::loop_::Signal::SIGINT, move || ml.quit());
    let ml = mainloop.clone();
    let _sigterm = mainloop.loop_().add_signal_local(pw::loop_::Signal::SIGTERM, move || ml.quit());

    // Safety check after 3 s: the driver clock must be the targeted hardware.
    let ml = mainloop.clone();
    let sh = shared.clone();
    let prefix = clock_prefix.clone();
    let check = mainloop.loop_().add_timer(move |_| {
        let name = sh.clock_name.lock().unwrap().clone();
        let cbs = sh.callbacks.load(Ordering::Relaxed);
        eprintln!("check: driver clock name='{name}' callbacks={cbs}");
        if !name.starts_with(&prefix) || cbs == 0 {
            eprintln!("ABORT: unexpected driver clock (expected prefix '{prefix}') or no callbacks");
            sh.stop.store(true, Ordering::Relaxed);
            ml.quit();
        }
    });
    check.update_timer(Some(Duration::from_secs(3)), None).into_result()?;

    // Kernel NTP discipline log (CLOCK_MONOTONIC is slewed by it).
    let ntp_log: std::rc::Rc<std::cell::RefCell<Vec<(u64, u64, i64, i64, i32)>>> = Default::default();
    let nl = ntp_log.clone();
    let ntp_timer = mainloop.loop_().add_timer(move |_| {
        let mut tx: libc::timex = unsafe { std::mem::zeroed() };
        unsafe { libc::adjtimex(&mut tx) };
        nl.borrow_mut().push((mono_ns(), raw_ns(), tx.freq as i64, tx.offset as i64, tx.status));
    });
    ntp_timer
        .update_timer(Some(Duration::from_millis(100)), Some(Duration::from_secs(1)))
        .into_result()?;

    let ml = mainloop.clone();
    let done = mainloop.loop_().add_timer(move |_| ml.quit());
    done.update_timer(Some(Duration::from_secs(seconds)), None).into_result()?;

    mainloop.run();

    shared.stop.store(true, Ordering::Relaxed);
    let _ = stream.disconnect();
    drop(_listener);

    let recs = shared.recs.lock().unwrap().clone();
    let name = shared.clock_name.lock().unwrap().clone();
    eprintln!(
        "done: {} records, callbacks={} no_buffer={} clock='{}'",
        recs.len(),
        shared.callbacks.load(Ordering::Relaxed),
        shared.no_buffer.load(Ordering::Relaxed),
        name
    );
    use std::io::Write;
    {
        let mut g = std::io::BufWriter::new(std::fs::File::create(format!("{out}.ntp"))?);
        writeln!(g, "mono_ns,raw_ns,freq_scaled_ppm,offset,status")?;
        for (m, r, fq, of, st) in ntp_log.borrow().iter() {
            writeln!(g, "{m},{r},{fq},{of},{st}")?;
        }
    }
    let mut f = std::io::BufWriter::new(std::fs::File::create(&out)?);
    writeln!(f, "# clock_name={name} target={target} quantum={quantum}")?;
    writeln!(f, "t_cb,t_raw,t_end,now,rate_num,rate_den,ticks,delay,queued,buffered,queued_buffers,avail_buffers,size,queued_after,delay_after,requested,written,clk_nsec,clk_next_nsec,clk_position,clk_duration,clk_delay,clk_rate_diff,clk_rate_den,clk_flags,clk_cycle,clk_xrun,clk_id")?;
    for r in &recs {
        writeln!(
            f,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{:.12},{},{},{},{},{}",
            r.t_cb, r.t_raw, r.t_end, r.now, r.rate_num, r.rate_den, r.ticks, r.delay, r.queued, r.buffered,
            r.queued_buffers, r.avail_buffers, r.size, r.queued_after, r.delay_after, r.requested,
            r.written, r.clk_nsec, r.clk_next_nsec, r.clk_position, r.clk_duration, r.clk_delay,
            r.clk_rate_diff, r.clk_rate_den, r.clk_flags, r.clk_cycle, r.clk_xrun, r.clk_id
        )?;
    }
    Ok(())
}
