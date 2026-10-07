//! Quality of the resampler against the exact band-limited signal.
//!
//! A sine is resampled at a fixed ratio; every output sample is compared with
//! the sine evaluated at the source position the resampler reports for it.
//! The residual therefore holds the filter's error *and* any position error:
//! THD+N here bounds both. Thresholds are the Phase 2 carry-overs of spike S3.

use audio_rt::{Design, DriftResampler, frame_ring};
use std::f64::consts::TAU;

const FS: f64 = 48_000.0;
const CH: usize = 4;
const N: usize = 1024;

/// THD+N in dB of a `freq` sine resampled at `ratio`, after the warm-up.
fn thd_n_db(freq: f64, ratio: f64) -> f64 {
    let mut rs = DriftResampler::new(CH, &Design::DRIFT_48K, N, 1.01);
    rs.set_ratio(ratio);
    let (mut tx, mut rx) = frame_ring(CH, 1 << 16);
    let mut produced = 0u64;
    let mut out = vec![0.0f32; N * CH];
    let (mut err2, mut sig2) = (0.0f64, 0.0f64);
    let source = |j: u64| (TAU * freq * j as f64 / FS).sin() * 0.5;
    let mut chunk = Vec::with_capacity(4096 * CH);
    for call in 0..300 {
        while produced < rx.read() + 8192 {
            chunk.clear();
            for j in produced..produced + 4096 {
                let v = source(j) as f32;
                chunk.extend(std::iter::repeat_n(v, CH));
            }
            produced += tx.push(&chunk) as u64;
        }
        let p0 = rs.position();
        let need = rs.prepare(N, ratio);
        assert!(rx.read_exact(rs.input_slot()));
        assert_eq!(need, rs.input_slot().len() / CH);
        rs.render(&mut out);
        if call < 4 {
            continue; // the history starts silent
        }
        let step = (ratio * 4_294_967_296.0).round() / 4_294_967_296.0;
        for (n, frame) in out.as_chunks::<CH>().0.iter().enumerate() {
            let p = p0.as_f64() + n as f64 * step;
            let ideal = (TAU * freq * p / FS).sin() * 0.5;
            for &v in frame {
                err2 += (v as f64 - ideal).powi(2);
                sig2 += ideal * ideal;
            }
        }
    }
    10.0 * (err2 / sig2).log10()
}

#[test]
fn transparent_in_the_passband() {
    for &ratio in &[1.0, 1.0 + 100e-6, 1.0 - 500e-6, 1.0 + 2000e-6] {
        for &(freq, limit) in &[(1_000.0, -130.0), (10_000.0, -130.0), (20_000.0, -125.0)] {
            let db = thd_n_db(freq, ratio);
            eprintln!("{freq:>6} Hz, ratio {ratio:.4}: THD+N {db:.1} dB");
            assert!(
                db < limit,
                "{freq} Hz at ratio {ratio}: THD+N {db:.1} dB (limit {limit})"
            );
        }
    }
}

/// Passband flatness: the amplitude of a resampled sine, fitted by least
/// squares, stays within ±0.001 dB up to 20 kHz.
#[test]
fn passband_is_flat() {
    for &freq in &[100.0, 1_000.0, 10_000.0, 20_000.0] {
        let ratio = 1.0 + 300e-6;
        let mut rs = DriftResampler::new(1, &Design::DRIFT_48K, N, 1.01);
        rs.set_ratio(ratio);
        let input: Vec<f32> = (0..200_000)
            .map(|j| (TAU * freq * j as f64 / FS).sin() as f32)
            .collect();
        let mut cur = 0usize;
        let mut out = vec![0.0f32; N];
        let (mut sc, mut cc, mut ss, mut ys, mut yc) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for call in 0..150 {
            let p0 = rs.position();
            let need = rs.prepare(N, ratio);
            rs.input_slot().copy_from_slice(&input[cur..cur + need]);
            cur += need;
            rs.render(&mut out);
            if call < 4 {
                continue;
            }
            let step = (ratio * 4_294_967_296.0).round() / 4_294_967_296.0;
            for (n, &y) in out.iter().enumerate() {
                let ph = TAU * freq * (p0.as_f64() + n as f64 * step) / FS;
                let (s, c) = ph.sin_cos();
                ss += s * s;
                cc += c * c;
                sc += s * c;
                ys += y as f64 * s;
                yc += y as f64 * c;
            }
        }
        // Solve the 2x2 normal equations for y ≈ a·sin + b·cos.
        let det = ss * cc - sc * sc;
        let a = (ys * cc - yc * sc) / det;
        let b = (yc * ss - ys * sc) / det;
        let gain_db = 20.0 * (a * a + b * b).sqrt().log10();
        eprintln!("{freq:>6} Hz: {gain_db:+.6} dB");
        assert!(gain_db.abs() < 0.001, "{freq} Hz: {gain_db:+.5} dB");
    }
}
