//! The real output core (servo + ring + resampler + fades) in a closed loop
//! with a simulated device and source.
//!
//! The source pushes a ramp whose value is its own frame index, so the value
//! the device plays *is* the source position heard: the true end-to-end
//! latency comes straight out of the audio, through the real resampler.

use std::sync::Arc;

use audio_output::sync_output::{
    DeviceTiming, OutputCore, OutputCoreConfig, SourceTap, SyncTelemetry,
};
use audio_rt::frame_ring;

struct Run {
    /// (time, true latency) of every fully played callback after warm-up.
    latencies: Vec<(f64, f64)>,
    realigns: u64,
    underruns: u64,
    short_reads: u64,
}

/// `seconds` of playback: source on the reference clock delivered per
/// 1024-frame capture cycle and decoded in `batch`-frame bursts; device at
/// `device_ppm`, 1024-frame cycles, stereo.
fn run(seconds: f64, device_ppm: f64, batch: u64, target_s: f64) -> Run {
    const RATE: f64 = 48_000.0;
    const N: usize = 1024;
    let heard_delay = (1024.0 + 512.0) / RATE;
    let (mut tx, rx) = frame_ring(1, 1 << 16);
    let tap = Arc::new(SourceTap::new());
    let telemetry = Arc::new(SyncTelemetry::new());
    let mut config = OutputCoreConfig::new(1, 48_000, 48_000, N);
    config.servo.target_latency_s = target_s;
    let mut core = OutputCore::new(config, rx, Arc::clone(&tap), Arc::clone(&telemetry));

    let device_rate = RATE * (1.0 + device_ppm * 1e-6);
    let mut out = vec![0.0f32; N * 2];
    let mut pushed = 0u64;
    let mut chunk = Vec::new();
    let mut latencies = Vec::new();
    let mut k = 0u64;
    loop {
        let t = k as f64 * N as f64 / device_rate;
        if t > seconds {
            break;
        }
        // Capture side: whole 1024-frame cycles received by now.
        let received = ((t * RATE) as u64 / 1024) * 1024;
        if received > 0 {
            tap.publish(received as f64 / RATE, received);
        }
        // Decoder: whole batches of what was received.
        let decoded = (received / batch) * batch;
        if decoded > pushed {
            chunk.clear();
            chunk.extend((pushed..decoded).map(|j| j as f32));
            pushed += tx.push(&chunk) as u64;
        }
        let jitter = ((k * 2_654_435_761) % 1000) as f64 * 1e-9;
        let timing = DeviceTiming {
            t_s: t + jitter,
            position_frames: (k * N as u64) as f64,
            heard_delay_s: heard_delay,
        };
        let phase_before = core.servo().phase();
        core.process(timing, &mut out, 2);
        let fully_played = out[0] != 0.0 && out[(N - 1) * 2] != 0.0;
        if t > 30.0 && fully_played && phase_before == audio_sync::Phase::Running {
            // Value of the first frame = source frame heard at t + heard_delay.
            let heard_at = t + heard_delay;
            let produced_at = out[0] as f64 / RATE;
            latencies.push((t, heard_at - produced_at));
        }
        k += 1;
    }
    let snap = telemetry.snapshot();
    Run {
        latencies,
        realigns: snap.realigns,
        underruns: snap.underruns,
        short_reads: snap.short_reads,
    }
}

fn check(name: &str, r: &Run, target_s: f64) {
    assert!(
        r.latencies.len() > 1000,
        "{name}: {} samples",
        r.latencies.len()
    );
    let worst = r
        .latencies
        .iter()
        .map(|&(_, l)| (l - target_s).abs())
        .fold(0.0, f64::max);
    eprintln!(
        "{name}: {} samples, worst |latency - target| {:.1} us, realigns {}, underruns {}",
        r.latencies.len(),
        worst * 1e6,
        r.realigns,
        r.underruns
    );
    // The ramp is f32: half a frame of resolution at these indices (~10 µs).
    assert!(
        worst < 0.5e-3,
        "{name}: worst deviation {:.3} ms",
        worst * 1e3
    );
    assert_eq!(
        (r.realigns, r.underruns, r.short_reads),
        (0, 0, 0),
        "{name}"
    );
}

#[test]
fn holds_the_target_with_a_fast_device() {
    check(
        "+100 ppm, TrueHD bursts",
        &run(120.0, 100.0, 960, 0.120),
        0.120,
    );
}

#[test]
fn holds_the_target_with_a_slow_device() {
    check(
        "-100 ppm, E-AC-3 bursts",
        &run(120.0, -100.0, 1536, 0.150),
        0.150,
    );
}
