//! Load a room-response SOFA file with the BRIR loader and print what it
//! holds: `cargo run --release -p renderer --features sofa --example
//! brir_probe -- <file.sofa> [all|front]`. `all` keeps every head
//! orientation (what head tracking loads), `front` the one nearest straight
//! ahead. `raw M R E N` prints a few `Data.IR` values straight from the SOFA
//! reader instead, for a cross-check against another HDF5 reader.
use renderer::binaural::brir::{BrirLoadOptions, BrirSet, OrientationSelection};

fn main() {
    // Logs go to stderr through a minimal logger so the loader's info line shows.
    struct L;
    impl log::Log for L {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= log::Level::Info
        }
        fn log(&self, r: &log::Record) {
            if self.enabled(r.metadata()) {
                eprintln!("[{}] {}", r.level(), r.args());
            }
        }
        fn flush(&self) {}
    }
    let _ = log::set_logger(Box::leak(Box::new(L)))
        .map(|()| log::set_max_level(log::LevelFilter::Info));
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("path to a .sofa file");
    let mode = args.next();
    if mode.as_deref() == Some("raw") {
        // Raw Data.IR values through the reader alone, for a cross-check
        // against another HDF5 reader: [M][R][E][N] flat indices.
        let bytes = std::fs::read(&path).expect("read");
        let mut open = sofar::reader::OpenOptions::new();
        open.sample_rate(48000.0).normalized(false);
        let sofa = open.open_data(&bytes).expect("open");
        let h = sofa.hrtf();
        let v = &h.data_ir.values;
        // The true [M][R][E][N] shape (the reader misreads a 4-D one).
        let dim =
            |d: Option<String>, default: usize| d.and_then(|s| s.parse().ok()).unwrap_or(default);
        let m = dim(args.next(), 180);
        let r = dim(args.next(), 2);
        let e = dim(args.next(), 13);
        let n = dim(args.next(), 16384);
        println!("Data.IR values: {} (M×R×E×N = {})", v.len(), m * r * e * n);
        if v.len() != m * r * e * n {
            eprintln!("shape mismatch: pass the file's M R E N after `raw`");
            std::process::exit(1);
        }
        // Six values at the start, at a chunk boundary, deep inside and at
        // the very end.
        let probes = [
            (0, 0, 0, 0),
            (m / 2, r - 1, e / 2, n / 3),
            (m - 1, r - 1, e - 1, n - 6),
            (m / 4, 0, e / 6, n / 4 - 2),
        ];
        for (mi, ri, ei, n0) in probes {
            let base = ((mi * r + ri) * e + ei) * n + n0;
            let vals: Vec<String> = v[base..base + 6]
                .iter()
                .map(|x| format!("{x:.6e}"))
                .collect();
            println!("({mi}, {ri}, {ei}) {n0} [{}]", vals.join(", "));
        }
        let x = &v[..n];
        let (pi, pv) =
            x.iter().enumerate().fold(
                (0, 0.0f32),
                |a, (i, &y)| if y.abs() > a.1 { (i, y.abs()) } else { a },
            );
        let energy: f64 = x.iter().map(|&y| (y as f64) * (y as f64)).sum();
        println!("peak idx {pi} peak {pv:.6} energy {energy:.6}");
        return;
    }
    let sel = match mode.as_deref() {
        Some("front") => OrientationSelection::FrontOnly,
        _ => OrientationSelection::All,
    };
    let opts = BrirLoadOptions {
        orientations: sel,
        ..Default::default()
    };
    let t = std::time::Instant::now();
    let set = match BrirSet::from_sofa(&path, 48000, &opts) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("load failed: {e}");
            std::process::exit(1);
        }
    };
    let dt = t.elapsed();
    println!(
        "loaded in {:.1} s: {} ({} Hz)",
        dt.as_secs_f64(),
        set.conventions(),
        set.sample_rate()
    );
    println!("emitters ({}):", set.emitters().len());
    for (i, e) in set.emitters().iter().enumerate() {
        let r = (e[0] * e[0] + e[1] * e[1] + e[2] * e[2]).sqrt();
        let az = e[0].atan2(e[1]).to_degrees();
        let el = e[2].atan2((e[0] * e[0] + e[1] * e[1]).sqrt()).to_degrees();
        let taps = set.pair(i, 0).taps();
        println!(
            "  {i:2}: az {az:7.1}°  el {el:6.1}°  r {r:.2} m   {taps} taps ({:.3} s)",
            taps as f32 / 48000.0
        );
    }
    let o = set.orientations();
    let yaws: Vec<f32> = o.iter().map(|p| p.0).collect();
    println!(
        "orientations: {} (yaw {:.1}° … {:.1}°, pitch {:.1}° … {:.1}°)",
        o.len(),
        yaws.iter().cloned().fold(f32::INFINITY, f32::min),
        yaws.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
        o.iter().map(|p| p.1).fold(f32::INFINITY, f32::min),
        o.iter().map(|p| p.1).fold(f32::NEG_INFINITY, f32::max)
    );
    println!(
        "max taps {} ({:.3} s), resident {:.1} MiB",
        set.max_taps(),
        set.max_taps() as f32 / 48000.0,
        set.bytes() as f64 / (1024.0 * 1024.0)
    );
}
