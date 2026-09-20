//! Quality comparison between `RampMode::Sample` (exact per-sample VBAP) and
//! `RampMode::Interp` (per-sample linear blend of the block-endpoint gains).
//!
//! For a straight object trajectory p0→p1 spanning one decode block of N
//! samples we compute, per sample s:
//!   exact[s]  = G( lerp(p0, p1, s/(N-1)) )         (what Sample renders)
//!   interp[s] = lerp( G(p0), G(p1), s/(N-1) )      (what Interp renders)
//! and report how far apart the speaker gains are. G(·) is the real precomputed
//! cartesian VBAP lookup, queried via a single-object Off-mode render.
//!
//! Run:  cargo run -p renderer --example gain_modes --release

use renderer::live_params::{LiveEvaluationMode, PreferredEvaluationMode, RampMode};
use renderer::spatial_renderer::{SpatialChannelEvent, SpatialRenderer};
use renderer::spatial_vbap::{DistanceModel, VbapTableMode};
use renderer::speaker_layout::SpeakerLayout;

const N: usize = 40; // samples per decode block (TrueHD @ 48 kHz)

fn make_cartesian_renderer() -> SpatialRenderer {
    let r = SpatialRenderer::new(
        SpeakerLayout::preset("7.1.4").unwrap(),
        48_000,
        1,
        1,
        0.0,
        2.0,
        VbapTableMode::Cartesian {
            x_size: 31,
            y_size: 31,
            z_size: 15,
            z_neg_size: 15,
        },
        false,
        true, // position_interpolation: trilinear table lookup → true G(p), as real Sample mode
        DistanceModel::Linear,
        false,
        1.0,
        1.0,
        0.0,
        1.0,
        false,
        [1.0, 2.0, 0.5],
        2.0,
        0.5,
        0.0,
        0.0,
        false,
        false,
        false,
        1.0,
        1.0,
        PreferredEvaluationMode::PrecomputedCartesian,
        LiveEvaluationMode::PrecomputedCartesian,
        31,
        31,
        15,
        15,
    )
    .unwrap();
    {
        let ctrl = r.renderer_control();
        ctrl.set_requested_ramp_mode(RampMode::Off);
        ctrl.live.write().ramp_mode = RampMode::Off;
    }
    r
}

/// Exact VBAP speaker gains at ADM position `p`, via a single-object Off render.
fn gains_at(r: &mut SpatialRenderer, p: [f64; 3]) -> Vec<f32> {
    let pcm = vec![0.0f32; N]; // 1 object channel × N samples; content irrelevant
    let ev = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0.0),
        ramp_length: Some(0),
        size: Some([0.0, 0.0, 0.0]),
        position: Some(p),
        sample_pos: Some(0),
    }];
    let frame = r
        .render_frame(&pcm, 1, &ev, Vec::new(), true)
        .expect("render");
    frame
        .object_gains
        .iter()
        .find(|(ch, _)| *ch == 0)
        .map(|(_, g)| g.iter().copied().collect())
        .unwrap_or_default()
}

fn lerp_pt(a: [f64; 3], b: [f64; 3], t: f64) -> [f64; 3] {
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

fn compare(r: &mut SpatialRenderer, label: &str, p0: [f64; 3], p1: [f64; 3]) {
    let g0 = gains_at(r, p0);
    let g1 = gains_at(r, p1);
    let nspk = g0.len();

    let mut max_abs = 0.0f32; // largest per-speaker linear gain error
    let mut sum_sq = 0.0f64; // for RMS over all (sample, speaker)
    let mut count = 0usize;
    let mut worst_s = 0usize;

    for s in 0..N {
        let t = s as f64 / (N - 1) as f64;
        let exact = gains_at(r, lerp_pt(p0, p1, t));
        let tf = t as f32;
        for spk in 0..nspk {
            let interp = g0[spk] + (g1[spk] - g0[spk]) * tf;
            let d = (exact[spk] - interp).abs();
            if d > max_abs {
                max_abs = d;
                worst_s = s;
            }
            sum_sq += (d as f64) * (d as f64);
            count += 1;
        }
    }
    let rms = (sum_sq / count.max(1) as f64).sqrt();
    // Express the worst error in dB relative to full scale (1.0 = 0 dBFS gain).
    let max_db = if max_abs > 0.0 {
        20.0 * (max_abs).log10()
    } else {
        f32::NEG_INFINITY
    };
    println!(
        "{label:22} | max |Δgain| = {max_abs:.4} ({max_db:6.1} dBFS) at s={worst_s:2}/{N} \
         | RMS = {rms:.5}",
    );
}

fn main() {
    let mut r = make_cartesian_renderer();
    // Grid cell size = 2/30 ≈ 0.067 on x,y. Trajectories of increasing length
    // (in cells crossed) over one block, plus a couple of representative pans.
    println!("Sample (exact) vs Interp (gain-lerp) over one {N}-sample block, 7.1.4 cartesian:\n");
    compare(
        &mut r,
        "within 1 cell (0.05)",
        [-0.2, 0.0, 0.3],
        [-0.15, 0.0, 0.3],
    );
    compare(&mut r, "~3 cells (0.2)", [-0.3, 0.0, 0.2], [-0.1, 0.0, 0.2]);
    compare(&mut r, "~7 cells (0.5)", [-0.5, 0.1, 0.1], [0.0, 0.1, 0.1]);
    compare(&mut r, "L→R sweep (2.0)", [-1.0, 0.2, 0.0], [1.0, 0.2, 0.0]);
    compare(
        &mut r,
        "front→back (2.0)",
        [0.0, -1.0, 0.0],
        [0.0, 1.0, 0.0],
    );
    compare(
        &mut r,
        "diagonal+up (full)",
        [-1.0, -1.0, 0.0],
        [1.0, 1.0, 1.0],
    );
    println!(
        "\nΔgain is linear speaker gain (1.0 = unity). Interp matches Sample exactly at the\n\
         block endpoints; the error in between grows with how nonlinearly the VBAP gains\n\
         vary along the path (i.e. how many cells / speaker hand-offs are crossed)."
    );
}
