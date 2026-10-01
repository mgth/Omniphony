//! Live check of the PipeWire sync output against a private null sink.
//!
//! Creates `rwspike-p3-sink` (a `support.null-audio-sink` driven by its own
//! timer, linked to no hardware: nothing is audible), plays the output core
//! into it, and feeds the ring from a synthetic source running `SOURCE_PPM`
//! off the reference clock in 960-frame bursts. Prints the servo's telemetry
//! and exits non-zero if the latency was not held or it ever realigned.
//!
//! ```text
//! cargo run -p audio_output --release --example sync_pw_live -- [seconds] [source_ppm] [target_ms]
//! ```

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use audio_output::sync_output::{
        OutputCore, OutputCoreConfig, PipewireSyncOutput, PipewireSyncOutputConfig, SourceTap,
        SyncTelemetry, reference_now_s,
    };
    use audio_rt::frame_ring;
    use pipewire as pw;

    const SINK: &str = "rwspike-p3-sink";
    const RATE: f64 = 48_000.0;
    const CH: usize = 2;
    const BATCH: u64 = 960;

    let args: Vec<String> = std::env::args().collect();
    let seconds: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(120.0);
    let source_ppm: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(100.0);
    let target_ms: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(150.0);

    // The private sink, kept alive by its own loop thread.
    let stop_sink = Arc::new(AtomicBool::new(false));
    let sink_thread = {
        let stop = Arc::clone(&stop_sink);
        std::thread::spawn(move || -> anyhow::Result<()> {
            pw::init();
            let mainloop = pw::main_loop::MainLoopRc::new(None)?;
            let context = pw::context::ContextRc::new(&mainloop, None)?;
            let core = context.connect_rc(None)?;
            let props = pw::properties::properties! {
                "factory.name" => "support.null-audio-sink",
                "node.name" => SINK,
                "node.description" => "rwspike phase 3 null sink",
                "media.class" => "Audio/Sink",
                "audio.channels" => "2",
                "audio.position" => "FL,FR",
                "audio.rate" => "48000",
                "object.linger" => "false",
                // Never a candidate for the session's default sink.
                "priority.session" => "0",
            };
            let _node = core.create_object::<pw::node::Node>("adapter", &props)?;
            while !stop.load(Ordering::Relaxed) {
                mainloop.loop_().iterate(Duration::from_millis(100));
            }
            Ok(())
        })
    };
    std::thread::sleep(Duration::from_millis(800));

    let (mut tx, rx) = frame_ring(CH, 1 << 16);
    let tap = Arc::new(SourceTap::new());
    let telemetry = Arc::new(SyncTelemetry::new());
    let mut config = OutputCoreConfig::new(CH, 48_000, 48_000, 8192);
    config.servo.target_latency_s = target_ms / 1e3;
    let core = OutputCore::new(config, rx, Arc::clone(&tap), Arc::clone(&telemetry));
    let output = PipewireSyncOutput::start(
        PipewireSyncOutputConfig {
            node_name: "rwspike-p3-output".into(),
            target: Some(SINK.into()),
            channels: CH as u32,
            positions: Some(vec!["FL".into(), "FR".into()]),
            rate: 48_000,
            quantum: 1024,
        },
        core,
    )?;

    // Synthetic source: `source_ppm` off the reference clock, 1024-frame
    // capture cycles, decoded in 960-frame bursts.
    let t0 = reference_now_s();
    let source_rate = RATE * (1.0 + source_ppm * 1e-6);
    let silence = vec![0.0f32; BATCH as usize * CH];
    let mut pushed = 0u64;
    let started = Instant::now();
    let mut next_report = 5.0;
    let mut worst_error_ms = 0.0f64;
    while started.elapsed().as_secs_f64() < seconds {
        let now = reference_now_s();
        let received = (((now - t0) * source_rate) as u64 / 1024) * 1024;
        if received > 0 {
            tap.publish(t0 + received as f64 / source_rate, received);
        }
        while pushed + BATCH <= received {
            if tx.push(&silence) == 0 {
                break;
            }
            pushed += BATCH;
        }
        let elapsed = started.elapsed().as_secs_f64();
        let snap = telemetry.snapshot();
        if elapsed > 40.0 && snap.phase == 1 {
            worst_error_ms = worst_error_ms.max(snap.error_ms.abs());
        }
        if elapsed >= next_report {
            next_report += 5.0;
            println!(
                "t={elapsed:6.1}s streaming={} phase={} latency={:?} err={:+.3}ms src={:+.2}ppm dev={:+.2}ppm ff={:+.2}ppm corr={:+.2}ppm floor={:.1}ms realigns={} underruns={} short={} lag={}",
                output.is_streaming(),
                snap.phase,
                snap.latency_ms.map(|l| (l * 1000.0).round() / 1000.0),
                snap.error_ms,
                snap.source_ppm,
                snap.device_ppm,
                snap.feedforward_ppm,
                snap.correction_ppm,
                snap.floor_ms,
                snap.realigns,
                snap.underruns,
                snap.short_reads,
                received - pushed,
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let snap = telemetry.snapshot();
    drop(output);
    stop_sink.store(true, Ordering::Relaxed);
    let _ = sink_thread.join();

    println!("worst |error| after 40 s: {worst_error_ms:.3} ms");
    let ok =
        worst_error_ms < 0.5 && snap.realigns == 0 && snap.underruns == 0 && snap.short_reads == 0;
    if !ok {
        anyhow::bail!("latency not held: {snap:?}");
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("Linux only");
}
