//! Run one closed-loop scenario and print a CSV trace plus the report.
//!
//! ```text
//! cargo run -p audio_sync --release --features sim --example sim -- follow 600 > trace.csv
//! ```
//!
//! Scenarios: `own`, `follow`, `mpv`, `eac3`, `excursion`, `pause`, `resync`.
//! The trace keeps one row in `every` callbacks (third argument, default 10);
//! a fourth argument sets the target latency in ms (default 100), a fifth the
//! phase-loop bandwidth in Hz.

use audio_sync::sim::{Decoder, Delivery, Excursion, Loss, Pause, Scenario, run_with};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let name = args.get(1).map(String::as_str).unwrap_or("own");
    let duration_s: f64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(600.0);
    let every: u64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(10);

    let mut base = Scenario {
        duration_s,
        ..Scenario::default()
    };
    if let Some(ms) = args.get(4).and_then(|s| s.parse::<f64>().ok()) {
        base.servo.target_latency_s = ms / 1e3;
    }
    if let Some(hz) = args.get(5).and_then(|s| s.parse::<f64>().ok()) {
        base.servo.loop_bandwidth_hz = hz;
    }
    let scenario = match name {
        "own" => base,
        "follow" => Scenario {
            servo: audio_sync::ServoConfig {
                target_latency_s: base.servo.target_latency_s,
                loop_bandwidth_hz: if args.get(5).is_some() {
                    base.servo.loop_bandwidth_hz
                } else {
                    audio_sync::ServoConfig::follow().loop_bandwidth_hz
                },
                ..base.servo
            },
            source_ppm: 80.0,
            delivery: Delivery::Follow {
                video_fps: 24_000.0 / 1001.0,
                ahead_s: 0.050,
                arrival_jitter_s: 0.005,
            },
            ..base
        },
        // As measured from real mpv into the pipe (2026-10-01): ±20 ms of
        // arrival jitter and no source drift against the reference clock.
        "mpv" => Scenario {
            servo: audio_sync::ServoConfig {
                target_latency_s: base.servo.target_latency_s,
                loop_bandwidth_hz: if args.get(5).is_some() {
                    base.servo.loop_bandwidth_hz
                } else {
                    audio_sync::ServoConfig::follow().loop_bandwidth_hz
                },
                ..audio_sync::ServoConfig::follow()
            },
            source_ppm: 0.0,
            delivery: Delivery::Follow {
                video_fps: 24_000.0 / 1001.0,
                ahead_s: 0.050,
                arrival_jitter_s: 0.040,
            },
            ..base
        },
        "eac3" => Scenario {
            decoder: Decoder::EAC3,
            ..base
        },
        "excursion" => Scenario {
            excursion: Some(Excursion {
                start_s: duration_s / 4.0,
                ramp_s: 20.0,
                hold_s: 100.0,
                ppm: -66.0,
            }),
            ..base
        },
        "pause" => Scenario {
            pauses: vec![Pause {
                start_s: duration_s / 3.0,
                len_s: 2.0,
            }],
            ..base
        },
        "resync" => Scenario {
            losses: vec![Loss {
                at_s: duration_s / 3.0,
                frames: 5_136,
                reported: true,
            }],
            ..base
        },
        other => {
            eprintln!("unknown scenario {other:?}");
            std::process::exit(2);
        }
    };

    println!(
        "t_s,phase,true_latency_ms,measured_latency_ms,ratio_error_ppm,correction_ppm,feedforward_ppm"
    );
    // Tuning overrides for parameter sweeps.
    let mut scenario = scenario;
    let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<f64>().ok());
    if let Some(v) = env("SIM_SRC_DLL_HZ") {
        scenario.servo.source_dll.bandwidth_hz = v;
    }
    if let Some(v) = env("SIM_RATE_FAST") {
        scenario.servo.source_dll.rate_fast_start = v != 0.0;
    }
    if let Some(v) = env("SIM_SRC_PPM") {
        scenario.source_ppm = v;
    }
    if let Some(v) = env("SIM_MAX_FF_PPM") {
        scenario.servo.max_feedforward_deviation = v * 1e-6;
    }
    if let Some(v) = env("SIM_LOOP_HZ") {
        scenario.servo.loop_bandwidth_hz = v;
    }
    if let Some(v) = env("SIM_JITTER_S")
        && let Delivery::Follow {
            arrival_jitter_s, ..
        } = &mut scenario.delivery
    {
        *arrival_jitter_s = v;
    }
    let mut n = 0u64;
    let report = run_with(&scenario, |s, _plan, _servo| {
        if n.is_multiple_of(every) {
            println!(
                "{:.4},{:?},{},{},{:.3},{:.3},{:.3}",
                s.t,
                s.phase,
                s.true_latency_s
                    .map(|l| format!("{:.4}", l * 1e3))
                    .unwrap_or_default(),
                s.measured_latency_s
                    .map(|l| format!("{:.4}", l * 1e3))
                    .unwrap_or_default(),
                s.ratio_error_ppm,
                s.correction_ppm,
                _servo.telemetry().feedforward_ppm
            );
        }
        n += 1;
    });
    eprintln!("{report:#?}");
}
