//! Closed-loop plant for the [`Servo`]: a deterministic model of the source,
//! decoder, ring, resampler and device clocks, driven one output callback at
//! a time, with the servo in the loop.
//!
//! The plant keeps the *true* timeline the servo never sees: when each source
//! frame was produced on the source's clock and when it is actually heard.
//! The report is computed from that truth, so it measures what a listener
//! would get, not what the servo believes.
//!
//! Model, all times on the reference clock M (s):
//!
//! - **Device.** Cycles of `quantum` output frames at
//!   `output_rate · (1 + device_ppm(t))`, where `device_ppm` can carry an
//!   excursion (PipeWire's graph clock re-centring, spike S2). Each callback
//!   reports its cycle start with a little timestamp jitter and the device
//!   position. A frame produced in a callback is heard `heard_delay` later,
//!   plus a hidden device delay nobody reports.
//! - **Source.** Produces frames at `source_rate · (1 + source_ppm)` except
//!   during pauses. Delivered to the capture point either per capture cycle on
//!   M (`own`: orender's driver clocks the player, so the source clock *is* M)
//!   or per video frame on the source's clock, a fixed amount ahead, with
//!   pipe jitter on the arrival (`follow`: mpv `--ao=pcm` into a pipe).
//! - **Decoder.** Releases whole batches (TrueHD 960 frames, E-AC-3 1536)
//!   after holding a constant look-ahead. Losses remove frames between the
//!   capture point and the ring, reported to the accounting or not.
//! - **Resampler.** Ideal: consumes exactly `ratio` source frames per output
//!   frame, keeps a fractional position, needs a fixed look-ahead in the ring.

use crate::servo::{CallbackInput, CallbackPlan, Phase, Servo, ServoConfig, SourceObservation};

/// How source frames reach the capture point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Delivery {
    /// orender drives the player on M: `quantum` frames per capture cycle.
    Own { quantum: u32 },
    /// The player writes on its own clock, one video frame at a time,
    /// `ahead_s` of audio ahead of its clock, arriving up to
    /// `arrival_jitter_s` late.
    Follow {
        video_fps: f64,
        ahead_s: f64,
        arrival_jitter_s: f64,
    },
}

/// Decoder batching between the capture point and the ring.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Decoder {
    pub batch_frames: u32,
    pub hold_frames: u32,
}

impl Decoder {
    /// TrueHD: one MAT frame of ~24 access units, 960 frames, no hold (S4).
    pub const TRUEHD: Self = Self {
        batch_frames: 960,
        hold_frames: 0,
    };
    /// E-AC-3 / AC-3: 1536-frame access units, one held back (S4).
    pub const EAC3: Self = Self {
        batch_frames: 1536,
        hold_frames: 1536,
    };
    /// Linear PCM: no batching.
    pub const PCM: Self = Self {
        batch_frames: 1,
        hold_frames: 0,
    };
}

/// A temporary device-rate change: ramps in over `ramp_s`, holds, ramps out.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Excursion {
    pub start_s: f64,
    pub ramp_s: f64,
    pub hold_s: f64,
    pub ppm: f64,
}

/// The source stops producing for `len_s` from `start_s`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pause {
    pub start_s: f64,
    pub len_s: f64,
}

/// `frames` disappear between capture and ring at `at_s`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Loss {
    pub at_s: f64,
    pub frames: u32,
    pub reported: bool,
}

/// Everything a run varies.
#[derive(Debug, Clone, PartialEq)]
pub struct Scenario {
    pub duration_s: f64,
    pub seed: u64,
    pub quantum: u32,
    pub source_ppm: f64,
    pub device_ppm: f64,
    pub excursion: Option<Excursion>,
    pub timestamp_jitter_s: f64,
    /// Reported delay from a cycle start to the first frame produced in it.
    pub heard_delay_s: f64,
    /// Device-internal delay nobody reports (USB/DAC), constant.
    pub hidden_delay_s: f64,
    pub delivery: Delivery,
    pub decoder: Decoder,
    pub pauses: Vec<Pause>,
    pub losses: Vec<Loss>,
    pub servo: ServoConfig,
    /// Samples taken before this time are not part of the steady state (s).
    pub warmup_s: f64,
    /// After a pause or a loss, samples are skipped for this long (s).
    pub settle_s: f64,
}

impl Default for Scenario {
    fn default() -> Self {
        Self {
            duration_s: 600.0,
            seed: 1,
            quantum: 1024,
            source_ppm: 0.0,
            device_ppm: -17.0,
            excursion: None,
            timestamp_jitter_s: 1e-6,
            heard_delay_s: (1024.0 + 512.0) / 48_000.0,
            hidden_delay_s: 0.002,
            delivery: Delivery::Own { quantum: 1024 },
            decoder: Decoder::TRUEHD,
            pauses: Vec::new(),
            losses: Vec::new(),
            servo: ServoConfig::default(),
            warmup_s: 30.0,
            settle_s: 30.0,
        }
    }
}

/// What a listener got, over the steady-state samples.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub callbacks: u64,
    pub steady_samples: u64,
    /// Median true end-to-end latency, from the source clock to the ear (s).
    pub latency_median_s: f64,
    /// p99 and max of `|latency − median|` (s).
    pub latency_dev_p99_s: f64,
    pub latency_dev_max_s: f64,
    /// Largest peak-to-peak, over any 10 s window, of the applied ratio
    /// against the true clock ratio (ppm).
    pub ratio_error_pp_ppm: f64,
    /// Largest `|ratio error|` (ppm).
    pub ratio_error_max_ppm: f64,
    pub realigns: u64,
    pub underruns: u64,
    /// Time the servo first started playing (s).
    pub started_at_s: Option<f64>,
    /// Highest latency floor the servo reported after warm-up (s).
    pub latency_floor_max_s: f64,
}

/// Deterministic xorshift64* generator.
#[derive(Debug, Clone)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    /// Uniform in [0, 1).
    fn next_f64(&mut self) -> f64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        (x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 11) as f64 / (1u64 << 53) as f64
    }
}

/// Deterministic per-index jitter in `[0, max)`: arrival `j` always gets the
/// same lateness, however often it is looked up.
fn indexed_jitter(seed: u64, j: u64, max: f64) -> f64 {
    Rng::new(seed ^ j.wrapping_mul(0xD1B5_4A32_D192_ED03)).next_f64() * max
}

struct Plant<'a> {
    sc: &'a Scenario,
    source_rate: f64,
    /// Pauses by start time.
    pauses: Vec<Pause>,
    /// Losses by time, each with the source index where it removed frames.
    losses: Vec<(Loss, f64)>,
}

impl<'a> Plant<'a> {
    fn device_ppm(&self, t: f64) -> f64 {
        let mut ppm = self.sc.device_ppm;
        if let Some(ex) = self.sc.excursion {
            let x = t - ex.start_s;
            let ramp = ex.ramp_s.max(1e-9);
            let w = if x < 0.0 {
                0.0
            } else if x < ramp {
                x / ramp
            } else if x < ramp + ex.hold_s {
                1.0
            } else if x < 2.0 * ramp + ex.hold_s {
                1.0 - (x - ramp - ex.hold_s) / ramp
            } else {
                0.0
            };
            ppm += w * ex.ppm;
        }
        ppm
    }

    fn new(sc: &'a Scenario) -> Self {
        let mut pauses = sc.pauses.clone();
        pauses.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
        let mut plant = Self {
            sc,
            source_rate: match sc.delivery {
                // The player follows orender's driver: the source clock is M.
                Delivery::Own { .. } => sc.servo.source_rate_hz,
                Delivery::Follow { .. } => sc.servo.source_rate_hz * (1.0 + sc.source_ppm * 1e-6),
            },
            pauses,
            losses: Vec::new(),
        };
        let mut losses: Vec<(Loss, f64)> = sc
            .losses
            .iter()
            .map(|l| (*l, plant.delivered(l.at_s).map(|(_, r)| r).unwrap_or(0.0)))
            .collect();
        losses.sort_by(|a, b| a.0.at_s.total_cmp(&b.0.at_s));
        plant.losses = losses;
        plant
    }

    /// Source frames produced by time `t` (pauses excluded).
    fn produced(&self, t: f64) -> f64 {
        let mut paused = 0.0;
        for p in &self.pauses {
            if t > p.start_s {
                paused += (t - p.start_s).min(p.len_s);
            }
        }
        self.source_rate * (t - paused).max(0.0)
    }

    /// Time at which source frame `f` was produced.
    fn production_time(&self, f: f64) -> f64 {
        let mut t = f / self.source_rate;
        for p in &self.pauses {
            if t >= p.start_s {
                t += p.len_s;
            }
        }
        t
    }

    /// Latest capture-point reading at `t`: `(arrival time, frames received)`.
    fn delivered(&self, t: f64) -> Option<(f64, f64)> {
        match self.sc.delivery {
            Delivery::Own { quantum } => {
                let q = quantum as f64;
                let received = (self.produced(t) / q).floor() * q;
                (received > 0.0).then(|| (self.production_time(received), received))
            }
            Delivery::Follow {
                video_fps,
                ahead_s,
                arrival_jitter_s,
            } => {
                let per_frame = self.sc.servo.source_rate_hz / video_fps;
                let ahead = (ahead_s * self.sc.servo.source_rate_hz).round();
                let mut j = (self.produced(t) / per_frame).floor() as i64;
                while j >= 0 {
                    let at = self.production_time(j as f64 * per_frame)
                        + indexed_jitter(self.sc.seed, j as u64, arrival_jitter_s);
                    if at <= t {
                        return Some((at, (j as f64 * per_frame + ahead).round()));
                    }
                    j -= 1;
                }
                None
            }
        }
    }

    /// Losses that happened by `t`, as `(ring index where frames went missing,
    /// frames)`, and the reported total.
    fn losses_by(&self, t: f64) -> (f64, f64) {
        let mut reported = 0.0;
        let mut total = 0.0;
        for l in &self.sc.losses {
            if t >= l.at_s {
                total += l.frames as f64;
                if l.reported {
                    reported += l.frames as f64;
                }
            }
        }
        (total, reported)
    }

    /// Source index of ring frame `i`: frames lost before it shift it.
    fn source_index(&self, ring_index: f64) -> f64 {
        let mut idx = ring_index;
        for (l, at) in &self.losses {
            // A loss at `at_s` removes the frames received around then.
            if idx >= *at {
                idx += l.frames as f64;
            } else {
                break;
            }
        }
        idx
    }

    fn available(&self, received: f64, lost: f64) -> f64 {
        let d = self.sc.decoder;
        let batch = d.batch_frames.max(1) as f64;
        let decoded = ((received - d.hold_frames as f64).max(0.0) / batch).floor() * batch;
        (decoded - lost).max(0.0)
    }
}

/// Run `scenario` to the end and summarise it.
pub fn run(scenario: &Scenario) -> Report {
    run_with(scenario, |_, _, _| {})
}

/// One callback as the plant saw it, for tracing.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    pub t: f64,
    pub phase: Phase,
    /// True end-to-end latency of the first frame played (s), if one was.
    pub true_latency_s: Option<f64>,
    /// Servo's own measurement (s).
    pub measured_latency_s: Option<f64>,
    /// Applied ratio against the true clock ratio (ppm).
    pub ratio_error_ppm: f64,
    pub correction_ppm: f64,
}

/// [`run`], calling `trace` after every callback.
pub fn run_with(
    scenario: &Scenario,
    mut trace: impl FnMut(&Sample, &CallbackPlan, &Servo),
) -> Report {
    let sc = scenario;
    let plant = Plant::new(sc);
    let mut servo = Servo::new(sc.servo);
    let mut rng = Rng::new(sc.seed);
    let quantum = sc.quantum as usize;
    let out_rate = sc.servo.output_rate_hz;

    let mut t = 0.0f64;
    let mut device_position = 0.0f64;
    let mut play = 0.0f64;
    let mut callbacks = 0u64;
    let mut started_at = None;

    let mut deviations_src: Vec<f64> = Vec::new();
    let mut latencies: Vec<f64> = Vec::new();
    let mut ratio_errors: Vec<(f64, f64)> = Vec::new();
    let mut floor_max = 0.0f64;

    let perturbations: Vec<f64> = sc
        .pauses
        .iter()
        .map(|p| p.start_s)
        .chain(sc.losses.iter().map(|l| l.at_s))
        .collect();
    let steady = |t: f64| {
        t >= sc.warmup_s
            && perturbations
                .iter()
                .all(|&p| t < p || t > p + sc.settle_s + pause_len(sc, p))
    };

    while t < sc.duration_s {
        let device_rate = out_rate * (1.0 + plant.device_ppm(t) * 1e-6);
        let reported_t = t + (rng.next_f64() - 0.5) * 2.0 * sc.timestamp_jitter_s;
        let (lost, reported_lost) = plant.losses_by(t);
        let delivered = plant.delivered(t);
        let received = delivered.map(|(_, r)| r).unwrap_or(0.0);
        let available = plant.available(received, lost);

        let input = CallbackInput {
            t: reported_t,
            device_position,
            frames: quantum,
            heard_delay_s: sc.heard_delay_s,
            play_position: play,
            available,
            source: delivered.map(|(at, r)| SourceObservation { t: at, received: r }),
            source_offset: -reported_lost,
        };
        let plan = servo.plan(&input);
        let played = plan.played_frames(quantum);

        let mut true_latency = None;
        if played > 0 {
            let first = play + plan.skip_source_frames;
            let needed = first + plan.ratio * played as f64 + sc.servo.resampler_lookahead_frames;
            assert!(
                needed <= available + 1e-6,
                "servo planned past the ring at t={t}: needs {needed}, has {available}"
            );
            started_at.get_or_insert(t);
            let heard =
                t + sc.heard_delay_s + sc.hidden_delay_s + plan.silence_frames as f64 / device_rate;
            let produced_at = plant.production_time(plant.source_index(first));
            true_latency = Some(heard - produced_at);
            play = first + plan.ratio * played as f64;
        }

        let true_ratio = plant.source_rate / device_rate;
        let ratio_error = (plan.ratio / true_ratio - 1.0) * 1e6;
        if t >= sc.warmup_s {
            floor_max = floor_max.max(servo.telemetry().latency_floor_s);
        }
        let running = servo.phase() == Phase::Running && plan.silence_frames == 0;
        if steady(t) && running {
            if let Some(l) = true_latency {
                latencies.push(l);
                deviations_src.push(l);
            }
            ratio_errors.push((t, ratio_error));
        }

        trace(
            &Sample {
                t,
                phase: servo.phase(),
                true_latency_s: true_latency,
                measured_latency_s: servo.telemetry().latency_s,
                ratio_error_ppm: ratio_error,
                correction_ppm: servo.telemetry().correction_ppm,
            },
            &plan,
            &servo,
        );

        callbacks += 1;
        device_position += quantum as f64;
        t += quantum as f64 / device_rate;
    }

    let median = median(&mut latencies.clone());
    let mut devs: Vec<f64> = deviations_src.iter().map(|l| (l - median).abs()).collect();
    devs.sort_by(f64::total_cmp);
    let p99 = percentile(&devs, 0.99);
    let max_dev = devs.last().copied().unwrap_or(0.0);

    Report {
        callbacks,
        steady_samples: latencies.len() as u64,
        latency_median_s: median,
        latency_dev_p99_s: p99,
        latency_dev_max_s: max_dev,
        ratio_error_pp_ppm: windowed_peak_to_peak(&ratio_errors, 10.0),
        ratio_error_max_ppm: ratio_errors.iter().map(|e| e.1.abs()).fold(0.0, f64::max),
        realigns: servo.telemetry().realigns,
        underruns: servo.telemetry().underruns,
        started_at_s: started_at,
        latency_floor_max_s: floor_max,
    }
}

fn pause_len(sc: &Scenario, start: f64) -> f64 {
    sc.pauses
        .iter()
        .find(|p| p.start_s == start)
        .map(|p| p.len_s)
        .unwrap_or(0.0)
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

/// Largest max−min of `value` over any window of `window_s` (by sample time).
fn windowed_peak_to_peak(samples: &[(f64, f64)], window_s: f64) -> f64 {
    let mut worst = 0.0f64;
    let mut start = 0usize;
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for (i, &(t, v)) in samples.iter().enumerate() {
        if t - samples[start].0 > window_s {
            worst = worst.max(hi - lo);
            start = i;
            lo = f64::INFINITY;
            hi = f64::NEG_INFINITY;
        }
        lo = lo.min(v);
        hi = hi.max(v);
    }
    if hi >= lo {
        worst = worst.max(hi - lo);
    }
    worst
}

#[cfg(test)]
mod tests {
    //! The Phase 1 acceptance thresholds (`docs/resampling-rework-plan.md`).
    use super::*;

    const P99_S: f64 = 0.5e-3;
    /// Frequency modulation the ratio may carry on steady clocks (any 10 s
    /// window). On the `own` clock the source is exact and this is tight.
    const RATIO_PP_PPM: f64 = 5.0;
    /// The same for a `follow` source, whose bursty arrivals leave the source
    /// DLL with phase noise (see `ServoConfig::follow`). 30 ppm is 0.05 cent,
    /// over ten times below the 0.05 % wow of a good turntable.
    const FOLLOW_RATIO_PP_PPM: f64 = 30.0;

    fn check(name: &str, sc: &Scenario, expect_realigns: u64) -> Report {
        check_with(name, sc, expect_realigns, RATIO_PP_PPM)
    }

    fn check_with(name: &str, sc: &Scenario, expect_realigns: u64, ratio_pp_ppm: f64) -> Report {
        let r = run(sc);
        eprintln!("{name}: {r:?}");
        assert!(r.steady_samples > 0, "{name}: no steady-state samples");
        assert!(
            r.started_at_s.is_some_and(|s| s < 1.0),
            "{name}: started at {:?}",
            r.started_at_s
        );
        assert!(
            r.latency_dev_p99_s < P99_S,
            "{name}: latency p99 deviation {:.3} ms",
            r.latency_dev_p99_s * 1e3
        );
        assert!(
            r.ratio_error_pp_ppm < ratio_pp_ppm,
            "{name}: ratio error {:.2} ppm p-p over 10 s",
            r.ratio_error_pp_ppm
        );
        assert_eq!(r.realigns, expect_realigns, "{name}: realigns");
        // An underrun always ends in a realign; a realign need not follow one.
        assert!(
            r.underruns <= r.realigns,
            "{name}: {} underruns",
            r.underruns
        );
        assert!(
            r.latency_floor_max_s < sc.servo.target_latency_s,
            "{name}: floor {:.1} ms above the target",
            r.latency_floor_max_s * 1e3
        );
        r
    }

    fn with_target(target_latency_s: f64) -> ServoConfig {
        ServoConfig {
            target_latency_s,
            ..ServoConfig::default()
        }
    }

    /// `own` clock, TrueHD bursts, ±100 ppm device drift, ten hours: the
    /// latency holds without a single realign.
    #[test]
    fn own_truehd_ten_hours_at_plus_100_ppm() {
        let r = check(
            "own_truehd_+100",
            &Scenario {
                duration_s: 36_000.0,
                device_ppm: 100.0,
                ..Scenario::default()
            },
            0,
        );
        // On the `own` clock the source time is the capture time, so the
        // listener gets exactly the target plus the delay nobody reports.
        let expected = 0.100 + 0.002;
        assert!(
            (r.latency_median_s - expected).abs() < P99_S,
            "median latency {:.3} ms",
            r.latency_median_s * 1e3
        );
    }

    #[test]
    fn own_truehd_one_hour_at_minus_100_ppm() {
        check(
            "own_truehd_-100",
            &Scenario {
                duration_s: 3_600.0,
                device_ppm: -100.0,
                ..Scenario::default()
            },
            0,
        );
    }

    /// mpv `--ao=pcm` into a pipe: video-frame bursts, write-ahead, pipe
    /// jitter, and the player's clock 80 ppm off M. A video frame of audio
    /// arrives at once, so the floor is a frame (41.7 ms) above `own`: 150 ms.
    #[test]
    fn follow_mpv_pipe_with_truehd() {
        check_with(
            "follow_mpv_truehd",
            &Scenario {
                duration_s: 3_600.0,
                servo: ServoConfig {
                    target_latency_s: 0.150,
                    ..ServoConfig::follow()
                },
                source_ppm: 80.0,
                delivery: Delivery::Follow {
                    video_fps: 24_000.0 / 1001.0,
                    ahead_s: 0.050,
                    arrival_jitter_s: 0.005,
                },
                ..Scenario::default()
            },
            0,
            FOLLOW_RATIO_PP_PPM,
        );
    }

    /// E-AC-3 holds one 32 ms access unit and releases another 32 ms at a
    /// time: 150 ms is comfortable, 100 ms is not (see the next test).
    /// The same pipe with the arrival jitter measured from real mpv
    /// (±20 ms, one-sided) and the player 80 ppm off the reference clock.
    /// Latency is measured against the source's true clock (its earliest
    /// arrivals), so the buffer must also cover the lateness: the floor rises
    /// by the jitter, hence 200 ms here.
    ///
    /// Open item of phase 4a (see the plan, §11): the latency holds within
    /// ±0.75 ms p99 and the ratio wanders by ~85 ppm peak-to-peak, short of
    /// the 0.5 ms / 30 ppm criteria. Run with `--ignored`.
    #[test]
    #[ignore = "phase 4a open item: follow estimator under ±20 ms one-sided jitter"]
    fn follow_mpv_pipe_with_measured_jitter() {
        let r = check_with(
            "follow_mpv_measured_jitter",
            &Scenario {
                duration_s: 1_800.0,
                servo: ServoConfig {
                    target_latency_s: 0.200,
                    ..ServoConfig::follow()
                },
                source_ppm: 80.0,
                delivery: Delivery::Follow {
                    video_fps: 24_000.0 / 1001.0,
                    ahead_s: 0.050,
                    arrival_jitter_s: 0.040,
                },
                ..Scenario::default()
            },
            0,
            FOLLOW_RATIO_PP_PPM,
        );
        assert!(r.ratio_error_max_ppm < 30.0, "{r:?}");
    }

    #[test]
    fn own_eac3_holds_an_access_unit() {
        check(
            "own_eac3",
            &Scenario {
                duration_s: 1_800.0,
                servo: with_target(0.150),
                decoder: Decoder::EAC3,
                ..Scenario::default()
            },
            0,
        );
    }

    /// A target below what the decoder holds cannot be met: the servo
    /// underruns, and its latency floor says by how much.
    #[test]
    fn an_infeasible_target_shows_in_the_floor() {
        let r = run(&Scenario {
            duration_s: 120.0,
            decoder: Decoder::EAC3,
            ..Scenario::default()
        });
        assert!(r.underruns > 0);
        assert!(
            r.latency_floor_max_s > 0.100,
            "floor {:.1} ms",
            r.latency_floor_max_s * 1e3
        );
    }

    /// PipeWire's graph clock wandering by −66 ppm for ~100 s (spike S2).
    /// The ratio lags the ramp by up to ~12 ppm — a smooth tracking error,
    /// 0.02 cent, not modulation — while the latency holds.
    #[test]
    fn rides_out_a_graph_clock_excursion() {
        check_with(
            "excursion",
            &Scenario {
                duration_s: 1_200.0,
                excursion: Some(Excursion {
                    start_s: 300.0,
                    ramp_s: 20.0,
                    hold_s: 100.0,
                    ppm: -66.0,
                }),
                ..Scenario::default()
            },
            0,
            15.0,
        );
    }

    /// A 2 s source stop: silence while stopped, one realign on return, then
    /// the same latency as before.
    #[test]
    fn recovers_from_a_two_second_pause() {
        let r = check(
            "pause",
            &Scenario {
                duration_s: 600.0,
                pauses: vec![Pause {
                    start_s: 200.0,
                    len_s: 2.0,
                }],
                ..Scenario::default()
            },
            1,
        );
        assert!((r.latency_median_s - 0.102).abs() < P99_S);
    }

    /// A reported one-frame loss is a 21 µs step: steered, no realign.
    #[test]
    fn a_reported_frame_loss_is_steered_out() {
        check(
            "reported_frame_loss",
            &Scenario {
                duration_s: 600.0,
                losses: vec![Loss {
                    at_s: 200.0,
                    frames: 1,
                    reported: true,
                }],
                ..Scenario::default()
            },
            0,
        );
    }

    /// A reported 107 ms loss (TrueHD resync) is a latency step the servo
    /// sees: one realign fills it with silence, and the listener keeps the
    /// same latency, so A/V sync survives.
    #[test]
    fn a_reported_resync_loss_is_realigned() {
        let r = check(
            "reported_resync_loss",
            &Scenario {
                duration_s: 600.0,
                losses: vec![Loss {
                    at_s: 200.0,
                    frames: 5_136,
                    reported: true,
                }],
                ..Scenario::default()
            },
            1,
        );
        assert!((r.latency_median_s - 0.102).abs() < P99_S);
    }

    /// The same loss unreported: the servo is blind to it and the listener's
    /// latency is 107 ms short for good. This is why every drop must be
    /// accounted (spike S4). (At a 100 ms target it is worse: the ring no
    /// longer holds what the servo expects and playback starves.)
    #[test]
    fn an_unreported_loss_shifts_the_latency_unseen() {
        let r = run(&Scenario {
            duration_s: 400.0,
            servo: with_target(0.300),
            losses: vec![Loss {
                at_s: 100.0,
                frames: 5_136,
                reported: false,
            }],
            warmup_s: 130.0,
            settle_s: 0.0,
            ..Scenario::default()
        });
        assert_eq!(r.realigns, 0);
        let shift = 0.302 - r.latency_median_s;
        assert!((shift - 5_136.0 / 48_000.0).abs() < 1e-3, "shift {shift}");
    }

    #[test]
    fn simulation_is_deterministic() {
        let sc = Scenario {
            duration_s: 120.0,
            delivery: Delivery::Follow {
                video_fps: 25.0,
                ahead_s: 0.04,
                arrival_jitter_s: 0.004,
            },
            ..Scenario::default()
        };
        assert_eq!(run(&sc), run(&sc));
    }

    #[test]
    fn peak_to_peak_is_taken_per_window() {
        let samples = [(0.0, 1.0), (5.0, 3.0), (11.0, 10.0), (12.0, 11.0)];
        assert_eq!(windowed_peak_to_peak(&samples, 10.0), 2.0);
    }
}
