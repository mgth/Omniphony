//! Single-thread CPU bench + steady-state allocation count.
//!
//! Each config resamples `SECONDS` of audio in calls of N output frames,
//! reading from an interleaved ring (the reader's memcpy is part of the timed
//! region, the producer's refill is not). Ratio = 1 + 300 ppm with a slow
//! +-50 ppm wobble, updated every call. Reported: best of `REPS` runs.
//!
//! Usage: bench [filter] (run under `nice -n 19 taskset -c <cpu>`)

use rubato014::Resampler as _;
use s3_resampler::candidates::{Candidate, InHouse, Rubato5, Rubato5Kind};
use s3_resampler::candidates::inhouse_designs;
use s3_resampler::ring::Ring;
use rubato::{PolynomialDegree, SincInterpolationType, WindowFunction};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

struct Counting;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(p, l, n) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

const FS: f64 = 48000.0;
const SECONDS: f64 = 4.0;
const REPS: usize = 5;

fn noise(len: usize) -> Vec<f32> {
    let mut s: u32 = 0x1234_5678;
    (0..len)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s as f32 / u32::MAX as f32) - 0.5
        })
        .collect()
}

fn refill(ring: &mut Ring, src: &[f32], ch: usize, cursor: &mut usize) {
    let frames_src = src.len() / ch;
    while ring.available() < 10000 {
        let n = (frames_src - *cursor).min(4096).min(ring.free());
        ring.write(&src[*cursor * ch..(*cursor + n) * ch]);
        *cursor = (*cursor + n) % frames_src;
    }
}

fn ratio_for(k: usize) -> f64 {
    1.0 + 300e-6 + 50e-6 * (k as f64 / 100.0).sin()
}

/// Returns (best ns per output frame, steady-state allocations).
fn bench_candidate(mut make: impl FnMut() -> Box<dyn Candidate>, ch: usize, n: usize) -> (f64, usize) {
    let src = noise(65536 * ch);
    let mut out = vec![0.0f32; n * ch];
    let calls = (SECONDS * FS / n as f64) as usize;
    let mut best = f64::INFINITY;
    let mut allocs = 0;
    for _ in 0..REPS {
        let mut c = make();
        let mut ring = Ring::new(ch, 32768);
        let mut cur = 0;
        for k in 0..64 {
            refill(&mut ring, &src, ch, &mut cur);
            assert!(c.process(&mut ring, &mut out, n, ratio_for(k)));
        }
        let a0 = ALLOCS.load(Ordering::Relaxed);
        let mut ns = 0u128;
        for k in 0..calls {
            refill(&mut ring, &src, ch, &mut cur);
            let t = Instant::now();
            let ok = c.process(&mut ring, &mut out, n, ratio_for(k + 64));
            ns += t.elapsed().as_nanos();
            assert!(ok);
        }
        allocs = allocs.max(ALLOCS.load(Ordering::Relaxed) - a0);
        std::hint::black_box(&out);
        best = best.min(ns as f64 / (calls * n) as f64);
    }
    (best, allocs)
}

/// Today's path: rubato 0.14 SincFixedIn, 1024-frame input chunks, planar
/// deinterleave/interleave around it (the ArrayQueue atomics are not counted).
fn bench_rubato014(ch: usize) -> (f64, usize) {
    use rubato014::{SincFixedIn, SincInterpolationParameters, SincInterpolationType as T, WindowFunction as W};
    let src = noise(65536 * ch);
    let chunk = 1024;
    let total_out = (SECONDS * FS) as usize;
    let mut best = f64::INFINITY;
    let mut allocs = 0;
    for _ in 0..REPS {
        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: T::Linear,
            oversampling_factor: 256,
            window: W::BlackmanHarris2,
        };
        let mut rs = SincFixedIn::<f32>::new(1.0, 1.1, params, chunk, ch).unwrap();
        let mut planar_in = vec![vec![0.0f32; chunk]; ch];
        let mut planar_out = vec![vec![0.0f32; rs.output_frames_max()]; ch];
        let mut inter_in = vec![0.0f32; chunk * ch];
        let mut inter_out = vec![0.0f32; rs.output_frames_max() * ch];
        let mut ring = Ring::new(ch, 32768);
        let mut cur = 0;
        let mut step = |rs: &mut SincFixedIn<f32>, ring: &mut Ring, k: usize| -> usize {
            assert!(ring.read_into(&mut inter_in));
            for (f, frame) in inter_in.chunks_exact(ch).enumerate() {
                for c in 0..ch {
                    planar_in[c][f] = frame[c];
                }
            }
            rs.set_resample_ratio(1.0 / ratio_for(k), true).unwrap();
            let (_, produced) = rs.process_into_buffer(&planar_in, &mut planar_out, None).unwrap();
            for f in 0..produced {
                for c in 0..ch {
                    inter_out[f * ch + c] = planar_out[c][f];
                }
            }
            produced
        };
        for k in 0..16 {
            refill(&mut ring, &src, ch, &mut cur);
            step(&mut rs, &mut ring, k);
        }
        let a0 = ALLOCS.load(Ordering::Relaxed);
        let mut ns = 0u128;
        let mut produced = 0;
        let mut k = 16;
        while produced < total_out {
            refill(&mut ring, &src, ch, &mut cur);
            let t = Instant::now();
            produced += step(&mut rs, &mut ring, k);
            ns += t.elapsed().as_nanos();
            k += 1;
        }
        allocs = allocs.max(ALLOCS.load(Ordering::Relaxed) - a0);
        best = best.min(ns as f64 / produced as f64);
    }
    (best, allocs)
}

fn main() {
    let filter = std::env::args().nth(1);
    let chans = [2usize, 8, 16, 24];
    let ns = [64usize, 256, 1024];
    println!("cand\tch\tN\tns_per_frame\tns_per_frame_ch\tpct_core_48k\tallocs");
    let keep = |name: &str| filter.as_ref().is_none_or(|f| name.contains(f.as_str()));
    let rb = |label: &'static str| -> Option<Rubato5Kind> {
        Some(match label {
            "rb5_sinc256_lin" => Rubato5Kind::Sinc {
                len: 256,
                os: 256,
                interp: SincInterpolationType::Linear,
                window: WindowFunction::BlackmanHarris2,
                cutoff: Some(0.95),
            },
            "rb5_sinc256_cub" => Rubato5Kind::Sinc {
                len: 256,
                os: 128,
                interp: SincInterpolationType::Cubic,
                window: WindowFunction::BlackmanHarris2,
                cutoff: None,
            },
            "rb5_sinc64_cub" => Rubato5Kind::Sinc {
                len: 64,
                os: 128,
                interp: SincInterpolationType::Cubic,
                window: WindowFunction::BlackmanHarris2,
                cutoff: None,
            },
            "rb5_poly_septic" => Rubato5Kind::Poly(PolynomialDegree::Septic),
            "rb5_poly_cubic" => Rubato5Kind::Poly(PolynomialDegree::Cubic),
            _ => return None,
        })
    };
    let report = |name: &str, ch: usize, n: usize, (t, a): (f64, usize)| {
        println!(
            "{name}\t{ch}\t{n}\t{t:.1}\t{:.2}\t{:.3}\t{a}",
            t / ch as f64,
            t * FS / 1e9 * 100.0
        );
    };
    for (name, d) in inhouse_designs() {
        if !keep(name) {
            continue;
        }
        for &ch in &chans {
            for &n in &ns {
                let r = bench_candidate(|| Box::new(InHouse::new(name, ch, d, 4096)), ch, n);
                report(name, ch, n, r);
            }
        }
    }
    for label in ["rb5_sinc256_lin", "rb5_sinc256_cub", "rb5_sinc64_cub", "rb5_poly_septic", "rb5_poly_cubic"] {
        if !keep(label) {
            continue;
        }
        for &ch in &chans {
            for &n in &ns {
                let r = bench_candidate(|| Box::new(Rubato5::new(label, ch, rb(label).unwrap(), 4096)), ch, n);
                report(label, ch, n, r);
            }
        }
    }
    if keep("rb014_current") {
        for &ch in &chans {
            report("rb014_current", ch, 1024, bench_rubato014(ch));
        }
    }
}
