//! Quality and position harness.
//!
//! Every run feeds 3 channels through a candidate under a ratio schedule:
//! ch0 = test signal (sum of tones), ch1/ch2 = 0.5 sin / cos at 100 Hz
//! (position probe). Each tone is analytic, so the ideal output at nominal
//! input position p is known exactly. Analysis:
//!
//! * Joint least-squares fit of every tone of ch0 against sin/cos of its
//!   phase at the *nominal* positions (the same linear-ramp law every
//!   candidate uses). A constant delay is absorbed by the fit. THD+N =
//!   residual power / fitted signal power (full band 0..fs/2). Gain per tone
//!   gives the passband.
//! * Position: unwrapped atan2(ch1, ch2) gives the actually played position
//!   to ~1e-5 frame. Compared with the nominal positions (offset and its
//!   wander) and, for the in-house resampler, with the reported position.

use s3_resampler::candidates::{all_candidates, Candidate};
use s3_resampler::ring::Ring;
use std::f64::consts::PI;
use std::io::Write;

/// Input sample rate (tones, probe and fits are all in input frames).
static FS_BITS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0x40E7700000000000); // 48000.0
fn fs() -> f64 {
    f64::from_bits(FS_BITS.load(std::sync::atomic::Ordering::Relaxed))
}
fn set_fs(v: f64) {
    FS_BITS.store(v.to_bits(), std::sync::atomic::Ordering::Relaxed)
}
const CH: usize = 3;
const OUT_FRAMES: usize = 3 * 48000;
const WARMUP: usize = 4096;
const PROBE_HZ: f64 = 100.0;

#[derive(Clone)]
struct Tone {
    f: f64,
    a: f64,
    ph: f64,
}

fn signals() -> Vec<(&'static str, Vec<Tone>)> {
    let one = |f: f64| vec![Tone { f, a: 0.891, ph: 0.3 }];
    let mut multi = Vec::new();
    let mut seed: u64 = 12345;
    for i in 0..31 {
        let f = (20.0f64 * (1000.0f64).powf(i as f64 / 30.0)).round() + 0.37;
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let ph = (seed >> 11) as f64 / (1u64 << 53) as f64 * 2.0 * PI;
        multi.push(Tone { f, a: 0.03, ph });
    }
    vec![
        ("t1k", one(997.0)),
        ("t10k", one(9973.0)),
        ("t20k", one(19997.0)),
        ("multi", multi),
        ("t22k", one(22001.0)),
    ]
}

/// Ratio (input frames per output frame) for call `k` and its frame count.
fn schedule(name: &str, k: usize, t: f64) -> (f64, usize) {
    let n = 256;
    match name {
        "r1" => (1.0, n),
        "p100" => (1.0 + 100e-6, n),
        "m100" => (1.0 - 100e-6, n),
        "p500" => (1.0 + 500e-6, n),
        "m500" => (1.0 - 500e-6, n),
        "p2000" => (1.0 + 2000e-6, n),
        "m2000" => (1.0 - 2000e-6, n),
        "mod" => (1.0 + 500e-6 * (2.0 * PI * t / 4.0).sin(), n),
        "varN" => {
            let ns = [64, 256, 1000, 4096, 333, 128];
            (1.0 + 500e-6 * (2.0 * PI * t / 4.0).sin(), ns[k % ns.len()])
        }
        // fixed SRC with drift on top
        "src441to48" => (44100.0 / 48000.0 * (1.0 + 300e-6 * (2.0 * PI * t / 4.0).sin()), n),
        "src48to441" => (48000.0 / 44100.0 * (1.0 + 300e-6 * (2.0 * PI * t / 4.0).sin()), n),
        _ => panic!("schedule {name}"),
    }
}

fn input_frame(tones: &[Tone], j: usize, frame: &mut [f32]) {
    let t = j as f64 / fs();
    let mut s = 0.0;
    for tn in tones {
        s += tn.a * (2.0 * PI * tn.f * t + tn.ph).sin();
    }
    frame[0] = s as f32;
    frame[1] = (0.5 * (2.0 * PI * PROBE_HZ * t).sin()) as f32;
    frame[2] = (0.5 * (2.0 * PI * PROBE_HZ * t).cos()) as f32;
}

struct Run {
    out: Vec<f32>,
    nominal: Vec<f64>,
    reported: Vec<f64>,
}

fn run(cand: &mut dyn Candidate, tones: &[Tone], sched: &str) -> Run {
    let mut ring = Ring::new(CH, 16384);
    let mut written = 0usize;
    let mut frame = [0.0f32; CH];
    let mut out = vec![0.0f32; OUT_FRAMES * CH];
    let mut nominal = Vec::with_capacity(OUT_FRAMES);
    let mut reported = Vec::with_capacity(OUT_FRAMES);
    let mut buf = vec![0.0f32; 4096 * CH];
    let mut produced = 0;
    let mut p = 0.0f64;
    let mut s = 1.0f64;
    let mut k = 0;
    while produced < OUT_FRAMES {
        let (ratio, n) = schedule(sched, k, produced as f64 / fs());
        let n = n.min(OUT_FRAMES - produced);
        // keep the ring topped up (producer side, untimed)
        while ring.free() > 0 && ring.available() < 12000 {
            input_frame(tones, written, &mut frame);
            ring.write(&frame);
            written += 1;
        }
        let had = cand.planned_positions(n, ratio, &mut reported);
        assert!(cand.process(&mut ring, &mut buf, n, ratio), "underrun");
        out[produced * CH..(produced + n) * CH].copy_from_slice(&buf[..n * CH]);
        // nominal positions: linear ramp of the step, increment first
        let d = (ratio - s) / n as f64;
        let after = cand.steps_before_sampling();
        for _ in 0..n {
            s += d;
            nominal.push(if after { p + s } else { p });
            p += s;
        }
        if had {
            // the call must land exactly where it said it would
            let last = *reported.last().unwrap();
            let end = cand.position().unwrap();
            assert!(end > last && end - last < 1.2, "end {end} last {last}");
        }
        produced += n;
        k += 1;
    }
    Run {
        out,
        nominal,
        reported,
    }
}

/// Solve A x = b (n x n, row-major) by Gaussian elimination with pivoting.
fn solve(mut a: Vec<f64>, mut b: Vec<f64>, n: usize) -> Vec<f64> {
    for col in 0..n {
        let piv = (col..n).max_by(|&i, &j| a[i * n + col].abs().total_cmp(&a[j * n + col].abs())).unwrap();
        if piv != col {
            for c in 0..n {
                a.swap(col * n + c, piv * n + c);
            }
            b.swap(col, piv);
        }
        let d = a[col * n + col];
        for r in col + 1..n {
            let f = a[r * n + col] / d;
            if f != 0.0 {
                for c in col..n {
                    a[r * n + c] -= f * a[col * n + c];
                }
                b[r] -= f * b[col];
            }
        }
    }
    let mut x = vec![0.0; n];
    for r in (0..n).rev() {
        let mut s = b[r];
        for c in r + 1..n {
            s -= a[r * n + c] * x[c];
        }
        x[r] = s / a[r * n + r];
    }
    x
}

struct Fit {
    thdn_db: f64,
    gains_db: Vec<f64>,
    /// delay (frames) of each tone relative to the nominal position
    delays: Vec<f64>,
}

fn fit(run: &Run, tones: &[Tone]) -> Fit {
    let k = tones.len();
    let m = 2 * k;
    let mut ata = vec![0.0; m * m];
    let mut atb = vec![0.0; m];
    let mut basis = vec![0.0; m];
    let range = WARMUP..OUT_FRAMES;
    // Fit against the positions the resampler reports when it exposes them
    // (that is the contract: output = x(reported p)), else the nominal law.
    let pos = if run.reported.len() == OUT_FRAMES { &run.reported } else { &run.nominal };
    for n in range.clone() {
        let p = pos[n];
        for (i, tn) in tones.iter().enumerate() {
            let th = 2.0 * PI * tn.f * p / fs() + tn.ph;
            basis[2 * i] = th.sin();
            basis[2 * i + 1] = th.cos();
        }
        let y = run.out[n * CH] as f64;
        for r in 0..m {
            atb[r] += basis[r] * y;
            for c in r..m {
                ata[r * m + c] += basis[r] * basis[c];
            }
        }
    }
    for r in 0..m {
        for c in 0..r {
            ata[r * m + c] = ata[c * m + r];
        }
    }
    let x = solve(ata, atb, m);
    let mut res = 0.0;
    let mut sig = 0.0;
    for n in range {
        let p = pos[n];
        let mut model = 0.0;
        for (i, tn) in tones.iter().enumerate() {
            let th = 2.0 * PI * tn.f * p / fs() + tn.ph;
            model += x[2 * i] * th.sin() + x[2 * i + 1] * th.cos();
        }
        let y = run.out[n * CH] as f64;
        res += (y - model) * (y - model);
        sig += model * model;
    }
    let mut gains_db = Vec::new();
    let mut delays = Vec::new();
    for (i, tn) in tones.iter().enumerate() {
        // y = g sin(th - w D) = g cos(wD) sin(th) - g sin(wD) cos(th)
        let (a, b) = (x[2 * i], x[2 * i + 1]);
        let g = (a * a + b * b).sqrt();
        gains_db.push(20.0 * (g / tn.a).log10());
        let wd = (-b).atan2(a);
        delays.push(wd / (2.0 * PI * tn.f / fs()));
    }
    Fit {
        thdn_db: 10.0 * (res / sig).log10(),
        gains_db,
        delays,
    }
}

struct PosStats {
    /// mean (measured - nominal), frames
    offset: f64,
    /// max |measured - nominal - offset|, frames
    wander: f64,
    /// max |measured - reported| (in-house only), frames
    vs_reported: Option<f64>,
}

fn position_check(run: &Run) -> PosStats {
    let w = 2.0 * PI * PROBE_HZ / fs();
    let mut prev = 0.0;
    let mut unwrap = 0.0;
    let mut meas = Vec::with_capacity(OUT_FRAMES);
    for n in 0..OUT_FRAMES {
        let s = run.out[n * CH + 1] as f64;
        let c = run.out[n * CH + 2] as f64;
        let th = s.atan2(c);
        if n > 0 {
            let d = th - prev;
            if d < -PI {
                unwrap += 2.0 * PI;
            } else if d > PI {
                unwrap -= 2.0 * PI;
            }
        }
        prev = th;
        meas.push((th + unwrap) / w);
    }
    // the first output frames see the zero history: skip warm-up, then anchor
    // the 2*pi ambiguity on the nominal position
    let n0 = WARMUP;
    let turns = ((run.nominal[n0] - meas[n0]) * w / (2.0 * PI)).round();
    let shift = turns * 2.0 * PI / w;
    let diffs: Vec<f64> = (n0..OUT_FRAMES).map(|n| meas[n] + shift - run.nominal[n]).collect();
    let offset = diffs.iter().sum::<f64>() / diffs.len() as f64;
    let wander = diffs.iter().map(|d| (d - offset).abs()).fold(0.0, f64::max);
    let vs_reported = if run.reported.len() == OUT_FRAMES {
        Some(
            (n0..OUT_FRAMES)
                .map(|n| (meas[n] + shift - run.reported[n]).abs())
                .fold(0.0, f64::max),
        )
    } else {
        None
    };
    PosStats {
        offset,
        wander,
        vs_reported,
    }
}

fn run_src(f: &mut std::fs::File, sigs: &[(&'static str, Vec<Tone>)]) {
    use s3_resampler::candidates::{InHouse, Rubato5, Rubato5Kind};
    use s3_resampler::polyphase::{Design, Interp};
    let d = |taps: usize, cutoff: f64| Design { taps, segments: 32, beta: 14.0, cutoff, interp: Interp::Hermite };
    let cases: Vec<(&str, f64, &str, Box<dyn Fn() -> Box<dyn Candidate>>)> = vec![
        ("ih_t96_up", 44100.0, "src441to48", Box::new(move || Box::new(InHouse::new("ih_t96", CH, d(96, 1.0), 4096)))),
        ("ih_t64_up", 44100.0, "src441to48", Box::new(move || Box::new(InHouse::new("ih_t64", CH, d(64, 1.0), 4096)))),
        ("ih_t112_down", 48000.0, "src48to441", Box::new(move || Box::new(InHouse::new("ih_t112", CH, d(112, 44100.0 / 48000.0), 4096)))),
        ("ih_t96_down", 48000.0, "src48to441", Box::new(move || Box::new(InHouse::new("ih_t96", CH, d(96, 44100.0 / 48000.0), 4096)))),
        ("rb5_sinc256_lin_up", 44100.0, "src441to48", Box::new(|| Box::new(Rubato5::with_nominal("rb5", CH, Rubato5Kind::Sinc {
            len: 256, os: 256, interp: rubato::SincInterpolationType::Linear,
            window: rubato::WindowFunction::BlackmanHarris2, cutoff: Some(0.95) }, 4096, 48000.0 / 44100.0)))),
    ];
    for (name, fsin, sched, make) in cases {
        set_fs(fsin);
        for (sname, tones) in sigs {
            if *sname == "t22k" {
                continue;
            }
            let mut cand = make();
            let r = run(cand.as_mut(), tones, sched);
            let ft = fit(&r, tones);
            let ps = position_check(&r);
            let gmin = ft.gains_db.iter().cloned().fold(f64::INFINITY, f64::min);
            let gmax = ft.gains_db.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            let line = format!(
                "{name}\t{sched}\t{sname}\t{:.1}\t{:+.5}\t{:+.5}\t-\t-\t{:.5}\t{:.2e}\t{}",
                ft.thdn_db, gmin, gmax, ps.offset, ps.wander,
                ps.vs_reported.map(|v| format!("{v:.2e}")).unwrap_or("-".into())
            );
            println!("{line}");
            writeln!(f, "{line}").unwrap();
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out_path = args.get(1).cloned().unwrap_or_else(|| "quality.tsv".into());
    let filter = args.get(2).cloned();
    let mut f = std::fs::File::create(&out_path).unwrap();
    writeln!(
        f,
        "cand\tsched\tsig\tthdn_db\tgain_min_db\tgain_max_db\tdelay_mean\tdelay_spread\tpos_offset\tpos_wander\tpos_vs_reported"
    )
    .unwrap();
    let sigs = signals();
    if filter.as_deref() == Some("src") {
        run_src(&mut f, &sigs);
        return;
    }
    let scheds_full = ["p100", "m100", "p500", "m500", "p2000", "m2000", "mod"];
    let scheds_extra = ["r1", "varN"];
    let names: Vec<String> = all_candidates(CH, 4096).iter().map(|c| c.name()).collect();
    for (ci, name) in names.iter().enumerate() {
        if let Some(fl) = &filter {
            if !name.contains(fl.as_str()) {
                continue;
            }
        }
        for sched in scheds_full.iter().chain(scheds_extra.iter()) {
            for (sname, tones) in &sigs {
                if scheds_extra.contains(sched) && !matches!(*sname, "t1k" | "multi" | "t20k") {
                    continue;
                }
                // fresh candidate per run
                let mut cand = all_candidates(CH, 4096).swap_remove(ci);
                let r = run(cand.as_mut(), tones, sched);
                let ft = fit(&r, tones);
                let ps = position_check(&r);
                let gmin = ft.gains_db.iter().cloned().fold(f64::INFINITY, f64::min);
                let gmax = ft.gains_db.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
                let dmean = ft.delays.iter().sum::<f64>() / ft.delays.len() as f64;
                let dspread = ft.delays.iter().map(|d| (d - dmean).abs()).fold(0.0, f64::max);
                let line = format!(
                    "{name}\t{sched}\t{sname}\t{:.1}\t{:+.5}\t{:+.5}\t{:.5}\t{:.2e}\t{:.5}\t{:.2e}\t{}",
                    ft.thdn_db,
                    gmin,
                    gmax,
                    dmean,
                    dspread,
                    ps.offset,
                    ps.wander,
                    ps.vs_reported.map(|v| format!("{v:.2e}")).unwrap_or("-".into())
                );
                println!("{line}");
                writeln!(f, "{line}").unwrap();
            }
        }
    }
}
