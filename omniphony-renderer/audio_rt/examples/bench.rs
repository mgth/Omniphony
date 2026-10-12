//! Cost of the resampler per output frame, at the default design.
//!
//! ```text
//! nice -n 19 cargo run -p audio_rt --release --example bench
//! ```

use audio_rt::{Design, DriftResampler};
use std::time::Instant;

fn main() {
    println!("channels\tN\tns/frame\tns/frame/ch\t% core @48k");
    for &ch in &[2usize, 8, 16, 24] {
        for &n in &[256usize, 1024] {
            let mut rs = DriftResampler::new(ch, &Design::DRIFT_48K, n, 1.01);
            let mut out = vec![0.0f32; n * ch];
            let calls = (2_000_000 / (n * ch)).max(200);
            let ratios = [1.0002, 0.9998];
            // Warm up.
            for i in 0..20 {
                rs.prepare(n, ratios[i % 2]);
                rs.input_slot().fill(0.1);
                rs.render(&mut out);
            }
            let start = Instant::now();
            for i in 0..calls {
                rs.prepare(n, ratios[i % 2]);
                rs.input_slot().fill(0.1);
                rs.render(&mut out);
            }
            let ns = start.elapsed().as_nanos() as f64 / (calls * n) as f64;
            println!(
                "{ch}\t{n}\t{ns:.1}\t{:.2}\t{:.3}",
                ns / ch as f64,
                ns * 48_000.0 / 1e9 * 100.0
            );
            std::hint::black_box(&out);
        }
    }
}
