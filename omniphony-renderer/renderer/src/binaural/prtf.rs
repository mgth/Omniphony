//! Spagnol/Geronazzo/Avanzini structural PRTF model — the "resonances + notches"
//! pinna alternative to the Brown-Duda echo model
//! ([`super::hrir::ParametricPinnaHrir`]).
//!
//! Layered on the analytic head shadow ([`SyntheticHrir`]), it adds:
//!   - a *resonance block*: two parallel second-order peak filters (concha
//!     modes) — the "ear timbre" the synthetic / Brown-Duda models lack, and
//!     the dominant cue against in-head localization;
//!   - a *reflection block*: three cascaded second-order notch filters whose
//!     center frequency rises with elevation (the elevation cue).
//!
//! Filters are exactly those of Geronazzo, Spagnol & Avanzini, "A HRTF Model
//! for Real-Time Customized 3-D Sound Rendering", SITIS 2011 (eqs 2–9).
//! Their parameters used to be a population average read off that paper's
//! mean PRTF plots, as linear ramps in elevation. They are now **fitted to
//! the embedded KEMAR set**: [`KEMAR_TRACKS`] holds, per 10° of elevation
//! in the median plane, the resonance envelope's first peak and its level
//! around 12 kHz, and the three notch tracks (centre, depth, width) of the
//! ear-averaged KEMAR response over the analytic head shadow — what the PRTF
//! multiplies. The table is produced by the `print_kemar_prtf_tracks`
//! instrumentation in this file's tests: twelfth-octave magnitude, an upper
//! envelope by three passes of clamped half-octave smoothing, the residual's
//! minima assigned to the tracks by continuity. Rows are interpolated
//! linearly in elevation; a notch absent on one side of a gap fades in
//! place instead of sweeping. The model is therefore "KEMAR average",
//! individualised as before by `freq_scale` (every centre ∝ 1/pinna size)
//! and `depth`.

use super::hrir::{HRIR_LEN, HrirPair, HrirProvider, SyntheticHrir, ear_exposure, pinna_shade};

const PI: f32 = std::f32::consts::PI;

/// A normalised second-order section (`a0` = 1).
#[derive(Clone, Copy)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Biquad {
    /// Zölzer peak/notch (boost/cut) section — SITIS eqs 2–6, with the `cut`
    /// variant of `k` (eq 9) for notches. `g_db > 0` boosts (`cut = false`),
    /// `g_db < 0` cuts (`cut = true`).
    fn peak_notch(cf: f32, g_db: f32, bw: f32, fs: f32, cut: bool) -> Self {
        let v0 = 10f32.powf(g_db / 20.0);
        let h0 = v0 - 1.0;
        let t = (PI * bw / fs).tan();
        let k = if cut {
            (t - v0) / (t + v0)
        } else {
            (t - 1.0) / (t + 1.0)
        };
        let l = -(2.0 * PI * cf / fs).cos();
        Self {
            b0: 1.0 + (1.0 + k) * h0 / 2.0,
            b1: l * (1.0 - k),
            b2: -k - (1.0 + k) * h0 / 2.0,
            a1: l * (1.0 - k),
            a2: -k,
        }
    }

    /// Bandpass-shaped peak with zero gain at DC/Nyquist (SITIS eqs 7–8). Used
    /// for the second resonance so the *parallel* sum keeps unity low-frequency
    /// gain (the first peak already carries the signal through).
    fn bandpass_peak(cf: f32, g_db: f32, bw: f32, fs: f32) -> Self {
        let v0 = 10f32.powf(g_db / 20.0);
        let h = 1.0 / (1.0 + (PI * bw / fs).tan());
        let l = -(2.0 * PI * cf / fs).cos();
        Self {
            b0: v0 * (1.0 - h),
            b1: 0.0,
            b2: -v0 * (1.0 - h),
            a1: 2.0 * l * h,
            a2: 2.0 * h - 1.0,
        }
    }

    /// Filter `x` (direct form I), returning `HRIR_LEN` output samples (the IIR
    /// tail beyond the FIR length is truncated — fine for these mild sections).
    fn process(&self, x: &[f32; HRIR_LEN]) -> [f32; HRIR_LEN] {
        let mut y = [0.0f32; HRIR_LEN];
        let (mut x1, mut x2, mut y1, mut y2) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for n in 0..HRIR_LEN {
            let xn = x[n];
            let yn = self.b0 * xn + self.b1 * x1 + self.b2 * x2 - self.a1 * y1 - self.a2 * y2;
            y[n] = yn;
            x2 = x1;
            x1 = xn;
            y2 = y1;
            y1 = yn;
        }
        y
    }
}

/// The five second-order sections at one direction (shared by both ears since
/// the PRTF is azimuth-invariant up to ~30° off the median plane).
struct PrtfBlocks {
    res1: Biquad,
    res2: Biquad,
    n1: Biquad,
    n2: Biquad,
    n3: Biquad,
}

/// One row of the KEMAR median-plane fit (see the module doc). Frequencies
/// in Hz, levels in dB.
#[derive(Clone, Copy)]
struct TrackRow {
    el: f32,
    /// First resonance: `(centre, gain above the 300–1200 Hz baseline, −3 dB
    /// width)`.
    res1: (f32, f32, f32),
    /// Envelope around 12 kHz, `(centre, level above the baseline, width)`:
    /// what the second, bandpass section has to add to reach.
    res2: (f32, f32, f32),
    /// The three notch tracks: `(centre, depth below the envelope — 0 when
    /// the track is absent, with the centre held — width at half depth)`.
    notches: [(f32, f32, f32); 3],
}

/// The fit, −50° to +90° by 10°; pasted from `print_kemar_prtf_tracks`.
const KEMAR_TRACKS: [TrackRow; 15] = [
    TrackRow {
        el: -50.0,
        res1: (3891.0, 13.3, 1969.0),
        res2: (12000.0, 12.6, 4500.0),
        notches: [
            (6234.0, -8.6, 844.0),
            (9141.0, -7.9, 1242.0),
            (15469.0, -6.2, 1078.0),
        ],
    },
    TrackRow {
        el: -40.0,
        res1: (3750.0, 12.6, 2227.0),
        res2: (12000.0, 10.5, 4805.0),
        notches: [
            (6234.0, 0.0, 844.0),
            (8438.0, -14.0, 1172.0),
            (14883.0, -6.6, 1359.0),
        ],
    },
    TrackRow {
        el: -30.0,
        res1: (3586.0, 9.3, 2789.0),
        res2: (12000.0, 12.2, 4148.0),
        notches: [
            (6633.0, -7.2, 914.0),
            (9000.0, -15.8, 984.0),
            (15844.0, -3.1, 1008.0),
        ],
    },
    TrackRow {
        el: -20.0,
        res1: (4266.0, 9.1, 2086.0),
        res2: (12000.0, 10.8, 4734.0),
        notches: [
            (6914.0, -5.2, 2602.0),
            (8062.0, -12.0, 1078.0),
            (16359.0, -4.3, 1477.0),
        ],
    },
    TrackRow {
        el: -10.0,
        res1: (4008.0, 10.7, 2156.0),
        res2: (12000.0, 10.5, 5367.0),
        notches: [
            (7781.0, -8.3, 1969.0),
            (8508.0, -7.1, 2062.0),
            (16359.0, 0.0, 1477.0),
        ],
    },
    TrackRow {
        el: 0.0,
        res1: (4008.0, 11.0, 2578.0),
        res2: (12000.0, 10.3, 6000.0),
        notches: [
            (7781.0, 0.0, 1969.0),
            (8250.0, -4.9, 2156.0),
            (11836.0, -3.2, 1312.0),
        ],
    },
    TrackRow {
        el: 10.0,
        res1: (4148.0, 11.5, 3305.0),
        res2: (12000.0, 9.9, 6000.0),
        notches: [
            (7781.0, 0.0, 1969.0),
            (8391.0, -5.1, 1805.0),
            (11766.0, -3.5, 1266.0),
        ],
    },
    TrackRow {
        el: 20.0,
        res1: (4336.0, 12.5, 3609.0),
        res2: (12000.0, 9.4, 6000.0),
        notches: [
            (7781.0, 0.0, 1969.0),
            (8766.0, -4.9, 1500.0),
            (11648.0, -3.8, 1125.0),
        ],
    },
    TrackRow {
        el: 30.0,
        res1: (4336.0, 12.7, 3938.0),
        res2: (12000.0, 8.0, 6000.0),
        notches: [
            (7781.0, 0.0, 1969.0),
            (9023.0, -4.3, 1477.0),
            (11672.0, -3.9, 1078.0),
        ],
    },
    TrackRow {
        el: 40.0,
        res1: (4336.0, 12.6, 4359.0),
        res2: (12000.0, 6.4, 6000.0),
        notches: [
            (7781.0, 0.0, 1969.0),
            (9375.0, -3.0, 1242.0),
            (11531.0, -5.2, 1359.0),
        ],
    },
    TrackRow {
        el: 50.0,
        res1: (4477.0, 12.0, 4758.0),
        res2: (12000.0, 5.6, 6000.0),
        notches: [
            (7781.0, 0.0, 1969.0),
            (9375.0, 0.0, 1242.0),
            (11227.0, -7.1, 1383.0),
        ],
    },
    TrackRow {
        el: 60.0,
        res1: (5766.0, 11.9, 5227.0),
        res2: (12000.0, 7.1, 6000.0),
        notches: [
            (7781.0, 0.0, 1969.0),
            (9375.0, 0.0, 1242.0),
            (11250.0, -4.4, 1406.0),
        ],
    },
    TrackRow {
        el: 70.0,
        res1: (5531.0, 11.5, 6422.0),
        res2: (12000.0, 8.2, 6000.0),
        notches: [
            (7781.0, 0.0, 1969.0),
            (9375.0, 0.0, 1242.0),
            (11250.0, 0.0, 1406.0),
        ],
    },
    TrackRow {
        el: 80.0,
        res1: (6000.0, 11.5, 10898.0),
        res2: (12000.0, 9.5, 6000.0),
        notches: [
            (7781.0, 0.0, 1969.0),
            (9375.0, 0.0, 1242.0),
            (11250.0, 0.0, 1406.0),
        ],
    },
    TrackRow {
        el: 90.0,
        res1: (6000.0, 11.4, 11555.0),
        res2: (12000.0, 10.5, 6000.0),
        notches: [
            (7781.0, 0.0, 1969.0),
            (9375.0, 0.0, 1242.0),
            (11250.0, 0.0, 1406.0),
        ],
    },
];

impl TrackRow {
    /// The row at `el_deg`, interpolated linearly between its two
    /// neighbours in [`KEMAR_TRACKS`] (the end rows held beyond the range).
    /// A notch absent on one side keeps the other side's centre and width
    /// and only its depth moves: it fades in place instead of sweeping
    /// across the band.
    fn at(el_deg: f32) -> Self {
        let rows = &KEMAR_TRACKS;
        let el = el_deg.clamp(rows[0].el, rows[rows.len() - 1].el);
        let i = rows
            .iter()
            .rposition(|r| r.el <= el)
            .unwrap_or(0)
            .min(rows.len() - 2);
        let (a, b) = (rows[i], rows[i + 1]);
        let t = ((el - a.el) / (b.el - a.el)).clamp(0.0, 1.0);
        let lerp3 = |x: (f32, f32, f32), y: (f32, f32, f32)| {
            (
                x.0 + (y.0 - x.0) * t,
                x.1 + (y.1 - x.1) * t,
                x.2 + (y.2 - x.2) * t,
            )
        };
        let notch = |x: (f32, f32, f32), y: (f32, f32, f32)| {
            if x.1 == 0.0 {
                (y.0, y.1 * t, y.2)
            } else if y.1 == 0.0 {
                (x.0, x.1 * (1.0 - t), x.2)
            } else {
                lerp3(x, y)
            }
        };
        Self {
            el,
            res1: lerp3(a.res1, b.res1),
            res2: lerp3(a.res2, b.res2),
            notches: [
                notch(a.notches[0], b.notches[0]),
                notch(a.notches[1], b.notches[1]),
                notch(a.notches[2], b.notches[2]),
            ],
        }
    }
}

/// Structural PRTF pinna model (Spagnol et al.) fitted to the embedded
/// KEMAR set. Resonances run in parallel, the three notches in series.
pub struct SpagnolPrtfHrir {
    /// Pinna coloration amount in [0, 1] — a dry/wet mix between the bare head
    /// shadow and the full PRTF. 0 ⇒ ≈ synthetic, 1 ⇒ full resonances + notches.
    pub depth: f32,
    /// Center-frequency scale (1.0 = the KEMAR fit). <1 lowers all notches
    /// and resonances (larger pinna), >1 raises them — crude 1-DOF
    /// individualization standing in for the photo-derived notch frequencies.
    pub freq_scale: f32,
    /// Head radius of the underlying head-shadow stage (see
    /// [`SyntheticHrir::head_radius_m`]).
    pub head_radius_m: f32,
}

impl SpagnolPrtfHrir {
    fn clamp_cf(cf: f32, fs: f32) -> f32 {
        cf.clamp(200.0, 0.49 * fs)
    }

    /// The five sections at `el_deg`, at this model's frequency scale.
    fn blocks(&self, el_deg: f32, fs: f32) -> PrtfBlocks {
        let sc = self.freq_scale.clamp(0.5, 1.5);
        let cf = |f: f32| Self::clamp_cf(f * sc, fs);
        let row = TrackRow::at(el_deg);
        // The resonance block is a parallel sum — unity plus the first
        // peak, plus a pure bandpass. Around the second centre the first
        // section is back near unity, so the bandpass's own gain is what
        // takes 1 to the envelope's level there: `1 + v0`.
        let v0 = (10f32.powf(row.res2.1 / 20.0) - 1.0).max(0.0);
        let res2_db = if v0 > 1e-3 { 20.0 * v0.log10() } else { -120.0 };
        let notch = |(centre, depth_db, bw): (f32, f32, f32)| {
            Biquad::peak_notch(cf(centre), depth_db, bw, fs, true)
        };
        PrtfBlocks {
            res1: Biquad::peak_notch(cf(row.res1.0), row.res1.1, row.res1.2, fs, false),
            res2: Biquad::bandpass_peak(cf(row.res2.0), res2_db, row.res2.2, fs),
            n1: notch(row.notches[0]),
            n2: notch(row.notches[1]),
            n3: notch(row.notches[2]),
        }
    }

    /// Resonance (parallel) + notch (cascade) blocks on one ear's head-shadow
    /// IR, then dry/wet-mixed by `depth` (this ear's, see
    /// [`pinna_shade`]).
    fn apply(&self, base: &[f32; HRIR_LEN], b: &PrtfBlocks, depth: f32) -> [f32; HRIR_LEN] {
        // Resonance block: H_res1 (unity + peak) in parallel with H_res2 (pure
        // bandpass), summed.
        let r1 = b.res1.process(base);
        let r2 = b.res2.process(base);
        let mut res = [0.0f32; HRIR_LEN];
        for n in 0..HRIR_LEN {
            res[n] = r1[n] + r2[n];
        }
        // Reflection block: three notches in series.
        let wet = b.n3.process(&b.n2.process(&b.n1.process(&res)));
        // Dry/wet mix — `depth` is the amount of pinna coloration.
        let d = depth.clamp(0.0, 1.0);
        let mut out = [0.0f32; HRIR_LEN];
        for n in 0..HRIR_LEN {
            out[n] = (1.0 - d) * base[n] + d * wet[n];
        }
        out
    }
}

impl HrirProvider for SpagnolPrtfHrir {
    fn render(&self, az_deg: f32, el_deg: f32, sample_rate: u32) -> HrirPair {
        // Base: analytic head shadow + ILD (identical to SyntheticHrir).
        let head = SyntheticHrir {
            head_radius_m: self.head_radius_m,
        };
        let mut pair = head.render(az_deg, el_deg, sample_rate);
        if self.depth <= 1e-4 {
            return pair;
        }
        let fs = sample_rate as f32;
        let blocks = self.blocks(el_deg, fs);

        // Each ear gets the pinna colouration to the extent it faces the
        // source; the blocks themselves are shared (the PRTF is azimuth-
        // invariant near the median plane).
        let (exp_l, exp_r) = ear_exposure(az_deg, el_deg);
        pair.left = self.apply(&pair.left, &blocks, self.depth * pinna_shade(exp_l));
        pair.right = self.apply(&pair.right, &blocks, self.depth * pinna_shade(exp_r));
        pair
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── KEMAR median-plane analysis (the fit's instrumentation) ────────────

    use super::super::measured::MeasuredHrirData;
    use realfft::RealFftPlanner;

    const FS: u32 = 48_000;
    /// 23.4 Hz bins: ten of them per twelfth-octave at 4 kHz.
    const NFFT: usize = 2048;

    fn bin_hz(k: usize) -> f32 {
        k as f32 * FS as f32 / NFFT as f32
    }

    fn hz_bin(hz: f32) -> usize {
        ((hz * NFFT as f32 / FS as f32).round() as usize).min(NFFT / 2)
    }

    /// Magnitude (dB) of `ir` on the analysis grid.
    fn magnitude_db(ir: &[f32]) -> Vec<f32> {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(NFFT);
        let n = ir.len().min(NFFT);
        let mut input = vec![0.0f32; NFFT];
        input[..n].copy_from_slice(&ir[..n]);
        let mut spec = fft.make_output_vec();
        fft.process(&mut input, &mut spec).expect("magnitude FFT");
        spec.iter()
            .map(|c| 20.0 * c.norm().max(1e-9).log10())
            .collect()
    }

    /// `db` averaged over a `width_oct`-octave window around each bin
    /// (above 200 Hz; below it the bins are too coarse to smooth).
    fn smooth_octaves(db: &[f32], width_oct: f32) -> Vec<f32> {
        let half = 2f32.powf(width_oct / 2.0);
        (0..db.len())
            .map(|k| {
                let f = bin_hz(k);
                if f < 200.0 {
                    return db[k];
                }
                let lo = hz_bin(f / half);
                let hi = hz_bin(f * half).min(db.len() - 1);
                db[lo..=hi].iter().sum::<f32>() / (hi - lo + 1) as f32
            })
            .collect()
    }

    /// The pinna response KEMAR shows in the median plane at `el`: the
    /// ear-averaged magnitude of the embedded set over the analytic head
    /// shadow's (what the PRTF multiplies), twelfth-octave smoothed, at
    /// 0 dB over 300–1200 Hz.
    fn kemar_prtf_db(set: &MeasuredHrirData, el: f32) -> Vec<f32> {
        let ((az_m, el_m), (l, r)) = set.nearest_measurement(0.0, el);
        assert!(
            az_m.abs() < 0.5 && (el_m - el).abs() < 0.5,
            "no median-plane measurement at {el}°: nearest is ({az_m}, {el_m})"
        );
        let (ld, rd) = (magnitude_db(l), magnitude_db(r));
        let head = SyntheticHrir::default().render(0.0, el, FS);
        let sd = magnitude_db(&head.left);
        let mut t: Vec<f32> = (0..ld.len())
            .map(|k| 0.5 * (ld[k] + rd[k]) - sd[k])
            .collect();
        let (k0, k1) = (hz_bin(300.0), hz_bin(1200.0));
        let base = t[k0..=k1].iter().sum::<f32>() / (k1 - k0 + 1) as f32;
        for v in &mut t {
            *v -= base;
        }
        smooth_octaves(&t, 1.0 / 12.0)
    }

    /// Upper envelope of `t`: half-octave smoothing, three passes, each
    /// clamped from below by `t` so the notches stop pulling it down.
    fn upper_envelope(t: &[f32]) -> Vec<f32> {
        let mut e = t.to_vec();
        for _ in 0..3 {
            let m: Vec<f32> = e.iter().zip(t).map(|(a, b)| a.max(*b)).collect();
            e = smooth_octaves(&m, 0.5);
        }
        e
    }

    fn argmax(x: &[f32], lo_hz: f32, hi_hz: f32) -> usize {
        let (lo, hi) = (hz_bin(lo_hz), hz_bin(hi_hz));
        (lo..=hi)
            .max_by(|&a, &b| x[a].partial_cmp(&x[b]).unwrap())
            .unwrap()
    }

    /// Width (Hz) around bin `k` over which `x` stays above `level`
    /// (`above`) or below it.
    fn width_hz(x: &[f32], k: usize, level: f32, above: bool) -> f32 {
        let inside = |v: f32| if above { v >= level } else { v <= level };
        let mut lo = k;
        while lo > 0 && inside(x[lo - 1]) {
            lo -= 1;
        }
        let mut hi = k;
        while hi + 1 < x.len() && inside(x[hi + 1]) {
            hi += 1;
        }
        bin_hz(hi + 1) - bin_hz(lo)
    }

    /// Local minima of `r` between `lo_hz` and `hi_hz` deeper than
    /// `min_depth_db`, merged when closer than 700 Hz (the deeper one
    /// stays): `(cf, depth, width at half depth)`.
    fn notches(r: &[f32], lo_hz: f32, hi_hz: f32, min_depth_db: f32) -> Vec<(f32, f32, f32)> {
        let (lo, hi) = (hz_bin(lo_hz), hz_bin(hi_hz));
        let mut out: Vec<(usize, f32)> = Vec::new();
        for k in lo..=hi {
            if r[k] < r[k - 1] && r[k] <= r[k + 1] && r[k] < min_depth_db {
                if let Some(last) = out.last_mut() {
                    if bin_hz(k) - bin_hz(last.0) < 700.0 {
                        if r[k] < last.1 {
                            *last = (k, r[k]);
                        }
                        continue;
                    }
                }
                out.push((k, r[k]));
            }
        }
        out.into_iter()
            .map(|(k, d)| (bin_hz(k), d, width_hz(r, k, d / 2.0, false)))
            .collect()
    }

    /// Farthest a notch may move between two rows 10° apart and still be
    /// the same track; past it the track fades out where it was and the
    /// notch starts on a track that was absent (N2 hands over to N3 that
    /// way between −10° and 0°, from 8.5 to 11.8 kHz).
    const TRACK_JUMP_HZ: f32 = 2_000.0;

    /// Assign the notches of one row (ascending frequency) to the three
    /// tracks, in order, at the least total movement from each track's
    /// last centre; a move past `TRACK_JUMP_HZ` is only allowed onto a
    /// track that was absent in the previous row. Returns the tracks'
    /// `(cf, depth, bw)` with depth 0 (and the last centre held) where a
    /// track is absent.
    fn assign_tracks(
        found: &[(f32, f32, f32)],
        last: &[(f32, f32, f32); 3],
    ) -> [(f32, f32, f32); 3] {
        let n = found.len().min(3);
        let found = &found[..n];
        let cost = |i: usize, track: usize| -> f32 {
            let d = (found[i].0 - last[track].0).abs();
            if d <= TRACK_JUMP_HZ {
                d
            } else if last[track].1 == 0.0 {
                TRACK_JUMP_HZ // a fresh start on a free track
            } else {
                f32::INFINITY
            }
        };
        // Ordered subsets of the tracks of size n: 3 choose n ≤ 3.
        let choices: &[&[usize]] = match n {
            0 => &[&[]],
            1 => &[&[0], &[1], &[2]],
            2 => &[&[0, 1], &[0, 2], &[1, 2]],
            _ => &[&[0, 1, 2]],
        };
        let best = choices
            .iter()
            .min_by(|a, b| {
                let ca: f32 = a.iter().enumerate().map(|(i, &t)| cost(i, t)).sum();
                let cb: f32 = b.iter().enumerate().map(|(i, &t)| cost(i, t)).sum();
                ca.partial_cmp(&cb).unwrap()
            })
            .unwrap();
        let mut out = last.map(|(cf, _, bw)| (cf, 0.0, bw));
        for (i, &t) in best.iter().enumerate() {
            out[t] = found[i];
        }
        out
    }

    /// The fit itself, row by row: `(el, res1, res2, notches)` — the
    /// resonance envelope's peak in 2–6 kHz and its level around 12 kHz
    /// (both `(cf, dB above the 300–1200 Hz baseline, −3 dB width)`), and
    /// the three notch tracks `(cf, depth dB re the envelope, width at
    /// half depth)`.
    fn kemar_fit_rows() -> Vec<(f32, (f32, f32, f32), (f32, f32, f32), [(f32, f32, f32); 3])> {
        let set = MeasuredHrirData::saf_kemar_shared(FS);
        let mut last = [
            (6_000.0, 0.0, 1_000.0),
            (9_000.0, 0.0, 1_000.0),
            (15_000.0, 0.0, 1_000.0),
        ];
        let mut rows = Vec::new();
        for el_i in -5..=9 {
            let el = el_i as f32 * 10.0;
            let t = kemar_prtf_db(&set, el);
            let e = upper_envelope(&t);
            let r: Vec<f32> = t.iter().zip(&e).map(|(a, b)| a - b).collect();
            let k1 = argmax(&e, 2_000.0, 6_000.0);
            let res1 = (bin_hz(k1), e[k1], width_hz(&e, k1, e[k1] - 3.0, true));
            // The second resonance is read at a fixed centre: past 40°
            // the envelope above 8 kHz is a plateau with no peak of its own.
            let k2 = hz_bin(12_000.0);
            let res2 = (
                12_000.0,
                e[k2],
                width_hz(&e, k2, e[k2] - 3.0, true).min(6_000.0),
            );
            let tracks = assign_tracks(&notches(&r, 4_000.0, 16_500.0, -3.0), &last);
            last = tracks;
            rows.push((el, res1, res2, tracks));
        }
        rows
    }

    /// Instrumentation for the KEMAR fit: prints the rows of
    /// [`kemar_fit_rows`] as the `KEMAR_TRACKS` table. Run with
    /// `-- --ignored --nocapture` and paste.
    /// The sections the model used before the fit: the SITIS population
    /// average as linear ramps in elevation. Kept as the reference the fit
    /// is measured against.
    fn legacy_blocks(el: f32, fs: f32) -> PrtfBlocks {
        let t = ((el.min(45.0) + 45.0) / 90.0).clamp(0.0, 1.0);
        let fade = if el > 45.0 {
            ((90.0 - el) / 45.0).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let g2 = 10.0 - 7.0 * t;
        let nd = (-13.0 + 5.0 * t) * fade;
        let bw = 1800.0 + 700.0 * t;
        PrtfBlocks {
            res1: Biquad::peak_notch(4200.0, 12.0, 2500.0, fs, false),
            res2: Biquad::bandpass_peak(13000.0, g2, 4000.0, fs),
            n1: Biquad::peak_notch(6000.0 + 3000.0 * t, nd, bw, fs, true),
            n2: Biquad::peak_notch(7500.0 + 3500.0 * t, nd, bw, fs, true),
            n3: Biquad::peak_notch(10000.0 + 4000.0 * t, nd, bw, fs, true),
        }
    }

    /// A model's pinna response at `el` on the analysis grid, normalised
    /// exactly like [`kemar_prtf_db`]: its output over the head-shadow base
    /// it was applied to, twelfth-octave smoothed, 0 dB over 300–1200 Hz.
    fn model_prtf_db(base: &[f32; HRIR_LEN], wet: &[f32; HRIR_LEN]) -> Vec<f32> {
        let (bd, wd) = (magnitude_db(base), magnitude_db(wet));
        let mut t: Vec<f32> = (0..bd.len()).map(|k| wd[k] - bd[k]).collect();
        let (k0, k1) = (hz_bin(300.0), hz_bin(1200.0));
        let base = t[k0..=k1].iter().sum::<f32>() / (k1 - k0 + 1) as f32;
        for v in &mut t {
            *v -= base;
        }
        smooth_octaves(&t, 1.0 / 12.0)
    }

    /// Mean absolute difference (dB) between two responses over 3–16 kHz.
    fn mean_abs_db(a: &[f32], b: &[f32]) -> f32 {
        let (lo, hi) = (hz_bin(3_000.0), hz_bin(16_000.0));
        (lo..=hi).map(|k| (a[k] - b[k]).abs()).sum::<f32>() / (hi - lo + 1) as f32
    }

    /// The fitted tracks follow the KEMAR median plane: the mean absolute
    /// error of the model's pinna response against KEMAR's, over 3–16 kHz
    /// and the fifteen rows, is well under the read-off-the-figures ramps'
    /// and under no pinna at all. Figures at the time of the fit: fit
    /// 2.44 dB, ramps 9.86 dB, no pinna 8.35 dB — the ramps were further
    /// from KEMAR than no pinna colouration at all.
    #[test]
    fn kemar_fit_tracks_the_median_plane() {
        let set = MeasuredHrirData::saf_kemar_shared(FS);
        let model = SpagnolPrtfHrir {
            depth: 1.0,
            freq_scale: 1.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        };
        let (mut fit, mut ramps, mut flat) = (0.0f32, 0.0f32, 0.0f32);
        let rows = KEMAR_TRACKS.len() as f32;
        for row in KEMAR_TRACKS {
            let target = kemar_prtf_db(&set, row.el);
            let base = SyntheticHrir::default().render(0.0, row.el, FS).left;
            let fitted = model.apply(&base, &model.blocks(row.el, FS as f32), 1.0);
            let legacy = model.apply(&base, &legacy_blocks(row.el, FS as f32), 1.0);
            let zero = vec![0.0f32; target.len()];
            fit += mean_abs_db(&model_prtf_db(&base, &fitted), &target) / rows;
            ramps += mean_abs_db(&model_prtf_db(&base, &legacy), &target) / rows;
            flat += mean_abs_db(&zero, &target) / rows;
        }
        println!(
            "[measure] prtf_kemar_fit: mean |error| 3–16 kHz — fit {fit:.2} dB, ramps {ramps:.2} dB, \
             no pinna {flat:.2} dB"
        );
        assert!(
            fit < 0.6 * ramps && fit < 0.5 * flat,
            "the fit does not beat the ramps clearly enough: fit {fit:.2} dB, ramps {ramps:.2} dB, \
             flat {flat:.2} dB"
        );
    }

    /// Between two rows the response moves gradually: over the whole
    /// elevation range, one degree never changes the left response by more
    /// than a few percent of its energy (the notches fade in place, the
    /// resonances slide).
    #[test]
    fn prtf_varies_smoothly_with_elevation() {
        let m = SpagnolPrtfHrir {
            depth: 1.0,
            freq_scale: 1.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        };
        let mut worst = (0.0f32, 0.0f32);
        let mut prev = m.render(0.0, -50.0, FS).left;
        for el_i in -49..=90 {
            let el = el_i as f32;
            let cur = m.render(0.0, el, FS).left;
            let energy: f32 = cur.iter().map(|x| x * x).sum();
            let step = sumsq_diff(&cur, &prev) / energy;
            if step > worst.0 {
                worst = (step, el);
            }
            prev = cur;
        }
        assert!(
            worst.0 < 0.02,
            "a one-degree step changes the response by {:.1} % of its energy at {}°",
            100.0 * worst.0,
            worst.1
        );
    }

    #[test]
    #[ignore = "instrumentation: prints the KEMAR median-plane fit, asserts nothing"]
    fn print_kemar_prtf_tracks() {
        let f = |(cf, g, bw): (f32, f32, f32)| format!("({cf:.0}.0, {g:.1}, {bw:.0}.0)");
        for (el, r1, r2, ns) in kemar_fit_rows() {
            println!(
                "    TrackRow {{ el: {el:.0}.0, res1: {}, res2: {}, notches: [{}, {}, {}] }},",
                f(r1),
                f(r2),
                f(ns[0]),
                f(ns[1]),
                f(ns[2])
            );
        }
    }

    fn sumsq_diff(a: &[f32; HRIR_LEN], b: &[f32; HRIR_LEN]) -> f32 {
        a.iter().zip(b.iter()).map(|(x, y)| (x - y) * (x - y)).sum()
    }

    #[test]
    fn prtf_depth_zero_matches_synthetic() {
        let p = SpagnolPrtfHrir {
            depth: 0.0,
            freq_scale: 1.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        }
        .render(0.0, 0.0, 48_000);
        let s = SyntheticHrir::default().render(0.0, 0.0, 48_000);
        assert_eq!(p.left, s.left);
        assert_eq!(p.right, s.right);
    }

    #[test]
    fn prtf_full_colours_the_spectrum() {
        // depth = 1 must clearly differ from the bare head shadow (resonances
        // + notches reshape the spectrum).
        let p = SpagnolPrtfHrir {
            depth: 1.0,
            freq_scale: 1.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        }
        .render(0.0, 0.0, 48_000);
        let s = SyntheticHrir::default().render(0.0, 0.0, 48_000);
        assert!(
            sumsq_diff(&p.left, &s.left) > 1e-3,
            "PRTF did not colour the spectrum"
        );
    }

    #[test]
    fn prtf_depends_on_elevation() {
        // Notch frequencies rise with elevation, so low vs high must differ.
        let m = SpagnolPrtfHrir {
            depth: 1.0,
            freq_scale: 1.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        };
        let lo = m.render(0.0, -45.0, 48_000);
        let hi = m.render(0.0, 45.0, 48_000);
        assert!(
            sumsq_diff(&lo.left, &hi.left) > 1e-3,
            "PRTF not elevation-dependent"
        );
    }

    #[test]
    fn prtf_freq_scale_shifts_response() {
        // The individualization knob must actually move the filters.
        let a = SpagnolPrtfHrir {
            depth: 1.0,
            freq_scale: 1.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        }
        .render(0.0, 0.0, 48_000);
        let b = SpagnolPrtfHrir {
            depth: 1.0,
            freq_scale: 1.3,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        }
        .render(0.0, 0.0, 48_000);
        assert!(
            sumsq_diff(&a.left, &b.left) > 1e-4,
            "freq_scale had no effect"
        );
    }

    /// The shadowed ear keeps a shallower version of the PRTF colouration
    /// (dry/wet at the floor); the median plane is untouched.
    #[test]
    fn prtf_is_shallower_on_the_shadowed_ear() {
        let m = SpagnolPrtfHrir {
            depth: 1.0,
            freq_scale: 1.0,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        };
        // Relative to each ear's own head-shadow base (see the pinna test).
        let colour = |az: f32, ear: fn(&HrirPair) -> &[f32; HRIR_LEN]| -> f32 {
            let p = m.render(az, 0.0, 48_000);
            let s = SyntheticHrir::default().render(az, 0.0, 48_000);
            let base: f32 = ear(&s).iter().map(|x| x * x).sum();
            sumsq_diff(ear(&p), ear(&s)) / base
        };
        let (right, left) = (colour(90.0, |p| &p.right), colour(90.0, |p| &p.left));
        let ratio = left / right;
        assert!(
            ratio < 0.2 && ratio > 0.04,
            "shadowed/facing ratio {ratio}, expected ≈ 0.09"
        );
        let (fl, fr) = (colour(0.0, |p| &p.left), colour(0.0, |p| &p.right));
        assert!((fl - fr).abs() < 1e-6 * fl.max(1e-12));
    }

    #[test]
    fn prtf_output_is_finite() {
        // Guard against unstable sections across the whole grid range.
        let m = SpagnolPrtfHrir {
            depth: 1.0,
            freq_scale: 1.5,
            head_radius_m: crate::binaural::itd::DEFAULT_HEAD_RADIUS_M,
        };
        for el in [-45.0, -20.0, 0.0, 20.0, 45.0, 70.0, 90.0] {
            let p = m.render(30.0, el, 48_000);
            assert!(
                p.left.iter().all(|x| x.is_finite()) && p.right.iter().all(|x| x.is_finite()),
                "non-finite PRTF at elevation {el}"
            );
        }
    }
}
