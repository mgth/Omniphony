//! Unit tests for the spatial render path, split out of `mod.rs` to keep the
//! core renderer file focused. Child module of `spatial_renderer`, so `super`
//! resolves to the renderer module and its private items.

use super::*;
// Types the tests construct directly. Imported here (not relied upon via
// `super::*`) so the production `mod.rs` only imports what its own code uses.
use crate::live_params::{LiveEvaluationMode, PreferredEvaluationMode};
use crate::render_backend::EffectiveEvaluationMode;
use crate::spatial_vbap::VbapTableMode;
use crate::speaker_layout::SpeakerLayout;

/// The unified multi-band cartesian table must render bit-equivalently to the
/// per-band path it replaces. Build two identical crossover renderers, force
/// one onto the per-band path (`unified_table = None`), feed both the same
/// frame, and require matching output.
#[test]
fn unified_crossover_matches_per_band() {
    fn build() -> SpatialRenderer {
        let mut layout = SpeakerLayout::preset("7.1.4").unwrap();
        for (sp, cutoff) in layout.speakers.iter_mut().zip([80.0, 200.0, 500.0]) {
            sp.freq_low = Some(cutoff);
        }
        SpatialRenderer::new(
            layout,
            48_000,
            1,
            1,
            0.0,
            2.0,
            VbapTableMode::Cartesian {
                x_size: 21,
                y_size: 21,
                z_size: 9,
                z_neg_size: 9,
            },
            false,
            true, // position interpolation → trilinear lookup + per-sample motion
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
            21,
            21,
            9,
            9,
        )
        .unwrap()
    }

    let mut unified = build();
    assert!(
        unified.unified_table.is_some(),
        "crossover layout should build a unified table"
    );
    let mut per_band = build();
    per_band.unified_table = None;

    let pcm: Vec<f32> = (0..40).map(|i| (i * 7 % 13) as f32 / 13.0 - 0.5).collect();
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([0.3, -0.2, 0.4]),
        sample_pos: Some(0),
    }];

    let a = unified
        .render_frame(&pcm, 1, &event, Vec::new(), false)
        .unwrap();
    let b = per_band
        .render_frame(&pcm, 1, &event, Vec::new(), false)
        .unwrap();
    assert_eq!(a.samples.len(), b.samples.len());
    let max_diff = a
        .samples
        .iter()
        .zip(&b.samples)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    assert!(
        max_diff < 1e-6,
        "unified vs per-band output mismatch: max diff {max_diff}"
    );
}

/// Polar counterpart of `unified_crossover_matches_per_band`: the unified
/// multi-band POLAR table must render bit-equivalently to the per-band polar
/// path. Same crossover layout, but a precomputed polar evaluator.
#[test]
fn unified_polar_matches_per_band() {
    fn build() -> SpatialRenderer {
        let mut layout = SpeakerLayout::preset("7.1.4").unwrap();
        for (sp, cutoff) in layout.speakers.iter_mut().zip([80.0, 200.0, 500.0]) {
            sp.freq_low = Some(cutoff);
        }
        SpatialRenderer::new(
            layout,
            48_000,
            1,
            1,
            0.0,
            2.0,
            VbapTableMode::Polar,
            false,
            true, // position interpolation → trilinear lookup + per-sample motion
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
            PreferredEvaluationMode::PrecomputedPolar,
            LiveEvaluationMode::PrecomputedPolar,
            31,
            31,
            15,
            15,
        )
        .unwrap()
    }

    let mut unified = build();
    assert!(
        unified.unified_table.is_some(),
        "polar crossover layout should build a unified table"
    );
    let mut per_band = build();
    per_band.unified_table = None;

    let pcm: Vec<f32> = (0..40).map(|i| (i * 7 % 13) as f32 / 13.0 - 0.5).collect();
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([0.3, -0.2, 0.4]),
        sample_pos: Some(0),
    }];

    let a = unified
        .render_frame(&pcm, 1, &event, Vec::new(), false)
        .unwrap();
    let b = per_band
        .render_frame(&pcm, 1, &event, Vec::new(), false)
        .unwrap();
    assert_eq!(a.samples.len(), b.samples.len());
    let max_diff = a
        .samples
        .iter()
        .zip(&b.samples)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    assert!(
        max_diff < 1e-6,
        "unified polar vs per-band output mismatch: max diff {max_diff}"
    );
}

/// A crossover band with only 1–2 speakers used to have no engine (hardcoded
/// equal-power), which disabled the unified table for the whole crossover.
/// Now such a band carries a `DegenerateVbapBackend`, so the unified table builds
/// and must stay bit-equivalent to the per-band path. Here the top band keeps
/// exactly 2 spatializable speakers (pairwise-VBAP fallback).
#[test]
fn unified_table_with_two_speaker_fallback_band() {
    fn build() -> SpatialRenderer {
        let mut layout = SpeakerLayout::preset("7.1.4").unwrap();
        // Cut all spatializable speakers at 200 Hz except the first two, so the
        // [200, ∞) band has exactly 2 speakers (a fallback band) and the
        // [0, 200) band keeps the rest (a normal ≥3 VBAP band).
        let mut kept = 0;
        for sp in layout.speakers.iter_mut() {
            if !sp.spatialize {
                continue;
            }
            if kept < 2 {
                kept += 1;
                continue;
            }
            sp.freq_high = Some(200.0);
        }
        SpatialRenderer::new(
            layout,
            48_000,
            1,
            1,
            0.0,
            2.0,
            VbapTableMode::Cartesian {
                x_size: 21,
                y_size: 21,
                z_size: 9,
                z_neg_size: 9,
            },
            false,
            true,
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
            21,
            21,
            9,
            9,
        )
        .unwrap()
    }

    let mut unified = build();
    assert!(
        unified.unified_table.is_some(),
        "a 2-speaker fallback band must not disable the unified table"
    );
    let mut per_band = build();
    per_band.unified_table = None;

    let pcm: Vec<f32> = (0..40).map(|i| (i * 7 % 13) as f32 / 13.0 - 0.5).collect();
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([0.3, -0.2, 0.4]),
        sample_pos: Some(0),
    }];

    let a = unified
        .render_frame(&pcm, 1, &event, Vec::new(), false)
        .unwrap();
    let b = per_band
        .render_frame(&pcm, 1, &event, Vec::new(), false)
        .unwrap();
    assert_eq!(a.samples.len(), b.samples.len());
    let max_diff = a
        .samples
        .iter()
        .zip(&b.samples)
        .map(|(x, y)| (x - y).abs())
        .fold(0.0f32, f32::max);
    assert!(
        max_diff < 1e-6,
        "unified vs per-band output mismatch (fallback band): max diff {max_diff}"
    );
}

/// An evaluation-mode change must reuse the triangulated gain model (the
/// geometry is mode-independent), rebuilding only the evaluation wrapper. A
/// geometry change (bumped generation) must rebuild the model. Verified via
/// `Arc::ptr_eq` on the decorated model.
#[test]
fn eval_mode_change_reuses_geometry() {
    let layout = SpeakerLayout::preset("7.1.4").unwrap();
    let r = SpatialRenderer::new(
        layout,
        48_000,
        1,
        1,
        0.0,
        2.0,
        VbapTableMode::Cartesian {
            x_size: 21,
            y_size: 21,
            z_size: 9,
            z_neg_size: 9,
        },
        false,
        true,
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
        21,
        21,
        9,
        9,
    )
    .unwrap();
    let control = r.renderer_control();
    let topo0 = control.active_topology();
    let model0 = topo0
        .backend
        .decorated_model()
        .expect("vbap backend exposes a decorated model");

    // Evaluation-mode-only change: geometry generation unchanged → reuse model.
    control
        .live
        .write()
        .set_evaluation_mode(LiveEvaluationMode::Realtime);
    let plan = control.prepare_topology_rebuild().expect("rebuild plan");
    let reused = plan
        .build_topology_reusing(Some(&topo0))
        .expect("reuse build");
    assert_eq!(
        reused.backend.evaluation_mode(),
        EffectiveEvaluationMode::Realtime
    );
    assert!(
        Arc::ptr_eq(&model0, &reused.backend.decorated_model().unwrap()),
        "evaluation-mode change must reuse the triangulated gain model"
    );

    // Geometry change bumps the generation → full rebuild (different model).
    control.bump_geometry_generation();
    let plan2 = control.prepare_topology_rebuild().expect("rebuild plan 2");
    let rebuilt = plan2.build_topology_reusing(Some(&topo0)).expect("rebuild");
    assert!(
        !Arc::ptr_eq(&model0, &rebuilt.backend.decorated_model().unwrap()),
        "a geometry change must rebuild the gain model"
    );
}

#[test]
fn test_renderer_creation() {
    let layout = SpeakerLayout::preset("7.1.4").unwrap();
    let renderer = SpatialRenderer::new(
        layout,
        48000,
        1,
        1,
        0.0,
        2.0,
        VbapTableMode::Polar,
        false,
        false,
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
        PreferredEvaluationMode::PrecomputedPolar,
        LiveEvaluationMode::PrecomputedPolar,
        31,
        31,
        15,
        15,
    );

    assert!(renderer.is_ok());

    let renderer = renderer.unwrap();
    assert_eq!(renderer.num_speakers(), 12);
}

/// The parametrable virtual bed mixes direct and virtualized channels in one
/// frame: `bed_indices` is full-length and carries `usize::MAX` for a channel
/// that must be VBAP-panned (object) even though its index is within
/// `num_beds`. This guards the render-loop generalisation from positional
/// (`idx < num_beds`) to sentinel-aware routing: channel 0 (sentinel) must
/// spread via VBAP while channel 1 (bed id 3 = LFE, a non-prefix bed) routes
/// one-hot to the LFE speaker.
#[test]
fn virtual_bed_mixes_direct_and_virtualized_channels() {
    fn build() -> SpatialRenderer {
        let layout = SpeakerLayout::preset("7.1.4").unwrap();
        SpatialRenderer::new(
            layout,
            48_000,
            1,
            1,
            0.0,
            2.0,
            VbapTableMode::Cartesian {
                x_size: 21,
                y_size: 21,
                z_size: 9,
                z_neg_size: 9,
            },
            false,
            true,
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
            21,
            21,
            9,
            9,
        )
        .unwrap()
    }

    // LFE is speaker index 3 in the 7.1.4 preset (spatialize:false).
    const LFE_SPK: usize = 3;
    let num_speakers = 12;
    let sample_length = 4;

    // Per-speaker summed |energy| across the block.
    let energy = |out: &[f32]| -> Vec<f32> {
        let mut e = vec![0.0f32; num_speakers];
        for s in 0..sample_length {
            for (spk, slot) in e.iter_mut().enumerate() {
                *slot += out[s * num_speakers + spk].abs();
            }
        }
        e
    };

    // Routing: channel 0 = virtual (object), channel 1 = direct LFE.
    let beds = [
        ChannelRoute::Virtual,
        ChannelRoute::Direct(bridge_api::RChannelLabel::LFE),
    ];

    // Pass A: only the object channel (0) carries signal.
    let mut ra = build();
    ra.configure_channel_routing(&beds);
    let pcm_a: Vec<f32> = (0..sample_length).flat_map(|_| [0.6f32, 0.0]).collect();
    let events_a = vec![
        SpatialChannelEvent {
            channel_idx: 0,
            is_bed: false,
            gain_db: Some(0),
            ramp_length: Some(0),
            size: Some([0.0, 0.0, 0.0]),
            position: Some([0.0, 1.0, 0.0]), // front-centre object
            sample_pos: Some(0),
        },
        SpatialChannelEvent {
            channel_idx: 1,
            is_bed: true,
            gain_db: Some(0),
            ramp_length: Some(0),
            size: None,
            position: None,
            sample_pos: Some(0),
        },
    ];
    let ea = energy(
        &ra.render_frame(&pcm_a, 2, &events_a, Vec::new(), false)
            .unwrap()
            .samples,
    );
    assert!(
        ea.iter().sum::<f32>() > 0.0,
        "object channel must produce output"
    );
    assert!(
        ea[LFE_SPK] < 1e-6,
        "front object must not leak into the non-spatialized LFE speaker (got {})",
        ea[LFE_SPK]
    );

    // Pass B: only the bed channel (1) carries signal → one-hot at the LFE.
    let mut rb = build();
    rb.configure_channel_routing(&beds);
    let pcm_b: Vec<f32> = (0..sample_length).flat_map(|_| [0.0f32, 0.6]).collect();
    let eb = energy(
        &rb.render_frame(&pcm_b, 2, &events_a, Vec::new(), false)
            .unwrap()
            .samples,
    );
    assert!(eb[LFE_SPK] > 0.0, "bed channel must reach the LFE speaker");
    for (spk, e) in eb.iter().enumerate() {
        if spk != LFE_SPK {
            assert!(
                *e < 1e-6,
                "bed routing must be one-hot; speaker {spk} got {e}"
            );
        }
    }
}

/// Locks the documented subwoofer bass-management recipe: flip the LFE to
/// `spatialize: true` with `freq_high: 120` while every other spatialized
/// speaker carries `freq_low: 120`. The sub is then alone in the `[0, 120)`
/// band, so the single-speaker degenerate rule routes the low band of every
/// object to it; the bands above exclude it entirely; and the stream's own
/// LFE bed channel keeps its direct one-hot feed (bed routing is keyed on
/// the speaker name, not on `spatialize`).
#[test]
fn spatialized_lfe_alone_in_low_band_routes_object_bass() {
    const CUTOFF: f32 = 120.0;
    const LFE_SPK: usize = 3; // 7.1.4 preset order
    fn build() -> SpatialRenderer {
        let mut layout = SpeakerLayout::preset("7.1.4").unwrap();
        for (idx, sp) in layout.speakers.iter_mut().enumerate() {
            if idx == LFE_SPK {
                sp.spatialize = true;
                sp.freq_high = Some(CUTOFF);
            } else {
                sp.freq_low = Some(CUTOFF);
            }
        }
        SpatialRenderer::new(
            layout,
            48_000,
            1,
            1,
            0.0,
            2.0,
            VbapTableMode::Cartesian {
                x_size: 21,
                y_size: 21,
                z_size: 9,
                z_neg_size: 9,
            },
            false,
            true,
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
            21,
            21,
            9,
            9,
        )
        .unwrap()
    }

    let num_speakers = 12;
    let sample_length = 9_600; // 200 ms — lets the 20 Hz tone settle
    let sample_rate = 48_000.0f32;

    // Per-speaker RMS over the second half of the block (filter steady state).
    let rms = |out: &[f32]| -> Vec<f32> {
        let half = sample_length / 2;
        let mut e = vec![0.0f32; num_speakers];
        for s in half..sample_length {
            for (spk, slot) in e.iter_mut().enumerate() {
                let v = out[s * num_speakers + spk];
                *slot += v * v;
            }
        }
        e.iter().map(|x| (x / half as f32).sqrt()).collect()
    };

    // A front-centre object playing a sine at `freq`.
    let render_tone = |freq: f32| -> Vec<f32> {
        let mut r = build();
        let pcm: Vec<f32> = (0..sample_length)
            .map(|i| 0.5 * (2.0 * std::f32::consts::PI * freq * i as f32 / sample_rate).sin())
            .collect();
        let events = vec![SpatialChannelEvent {
            channel_idx: 0,
            is_bed: false,
            gain_db: Some(0),
            ramp_length: Some(0),
            size: Some([0.0, 0.0, 0.0]),
            position: Some([0.0, 1.0, 0.0]),
            sample_pos: Some(0),
        }];
        rms(&r
            .render_frame(&pcm, 1, &events, Vec::new(), false)
            .unwrap()
            .samples)
    };

    // Deep bass: the sub must carry the tone and the mains must be genuinely
    // relieved of it — the LR4 high-pass rejects 20 Hz (fc/6) by ~60 dB.
    // Absolute levels include the renderer's distance attenuation for this
    // object position (identical across both tones), so the thresholds below
    // are calibrated with ample margin rather than derived from the input.
    let low = render_tone(20.0);
    assert!(
        low[LFE_SPK] > 0.08,
        "sub must carry the 20 Hz object tone, got RMS {}",
        low[LFE_SPK]
    );
    let max_main = low
        .iter()
        .enumerate()
        .filter(|(spk, _)| *spk != LFE_SPK)
        .map(|(_, e)| *e)
        .fold(0.0f32, f32::max);
    assert!(
        low[LFE_SPK] > 1.5 * max_main,
        "sub must dominate at 20 Hz: sub {} vs loudest main {}",
        low[LFE_SPK],
        max_main
    );
    assert!(
        max_main < 0.02,
        "mains must be relieved of the 20 Hz band (24 dB/oct high-pass), loudest main RMS {max_main}"
    );

    // Treble: the sub is out of the upper bands and the low-pass rejects
    // 4 kHz by >100 dB — it must stay silent.
    let high = render_tone(4_000.0);
    assert!(
        high[LFE_SPK] < 1e-4,
        "sub must not receive a 4 kHz object tone, got RMS {}",
        high[LFE_SPK]
    );
    assert!(
        high.iter().sum::<f32>() > 0.05,
        "the 4 kHz tone must reach the mains"
    );

    // The stream's own LFE bed channel still routes one-hot to the sub even
    // though the speaker is now spatialized.
    let mut r = build();
    let beds = [
        ChannelRoute::Virtual,
        ChannelRoute::Direct(bridge_api::RChannelLabel::LFE),
    ];
    r.configure_channel_routing(&beds);
    let bed_len = 8usize;
    let pcm: Vec<f32> = (0..bed_len).flat_map(|_| [0.0f32, 0.6]).collect();
    let events = vec![
        SpatialChannelEvent {
            channel_idx: 0,
            is_bed: false,
            gain_db: Some(0),
            ramp_length: Some(0),
            size: Some([0.0, 0.0, 0.0]),
            position: Some([0.0, 1.0, 0.0]),
            sample_pos: Some(0),
        },
        SpatialChannelEvent {
            channel_idx: 1,
            is_bed: true,
            gain_db: Some(0),
            ramp_length: Some(0),
            size: None,
            position: None,
            sample_pos: Some(0),
        },
    ];
    let out = r
        .render_frame(&pcm, 2, &events, Vec::new(), false)
        .unwrap()
        .samples;
    let mut e = vec![0.0f32; num_speakers];
    for s in 0..bed_len {
        for (spk, slot) in e.iter_mut().enumerate() {
            *slot += out[s * num_speakers + spk].abs();
        }
    }
    assert!(
        e[LFE_SPK] > 0.0,
        "the LFE bed channel must still reach the spatialized sub"
    );
    for (spk, v) in e.iter().enumerate() {
        if spk != LFE_SPK {
            assert!(
                *v < 1e-6,
                "LFE bed routing must stay one-hot with spatialize:true; speaker {spk} got {v}"
            );
        }
    }
}

/// Guard rail: the four ramp modes must stay wired and each keep its own
/// behaviour. `Off` snaps to the target, `Frame` holds the block-start
/// position, `Sample` interpolates the position per sample, and `Interp`
/// interpolates the gains per sample from the previous block's end. We render
/// TWO blocks per mode with a position change in between (the first block
/// seeds `Interp`'s start gains, so its ramp only shows on the second) and
/// compare the second block: every output must be finite, non-silent, and
/// the modes must not collapse onto one another for a moving object.
#[test]
fn all_four_ramp_modes_render_distinctly() {
    fn build() -> SpatialRenderer {
        let layout = SpeakerLayout::preset("7.1.4").unwrap();
        SpatialRenderer::new(
            layout,
            48_000,
            1,
            1,
            0.0,
            2.0,
            VbapTableMode::Cartesian {
                x_size: 21,
                y_size: 21,
                z_size: 9,
                z_neg_size: 9,
            },
            false,
            true, // position interpolation → trilinear lookup + per-sample motion
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
            21,
            21,
            9,
            9,
        )
        .unwrap()
    }

    let pcm = vec![0.5f32; 40];
    let event_at = |position: [f64; 3]| {
        vec![SpatialChannelEvent {
            channel_idx: 0,
            is_bed: false,
            gain_db: Some(0),
            ramp_length: Some(40),
            size: Some([0.0, 0.0, 0.0]),
            position: Some(position),
            sample_pos: Some(0),
        }]
    };
    let block_a = event_at([-0.7, 0.5, 0.2]);
    let block_b = event_at([0.8, -0.6, 0.5]);

    let render = |mode: RampMode| -> Vec<f32> {
        let mut r = build();
        r.control.live.write().ramp_mode = mode;
        // First block establishes a position (and seeds Interp's start gains).
        r.render_frame(&pcm, 1, &block_a, Vec::new(), false)
            .unwrap();
        // Second block moves the object — this is what we compare.
        r.render_frame(&pcm, 1, &block_b, Vec::new(), false)
            .unwrap()
            .samples
    };

    let off = render(RampMode::Off);
    let frame = render(RampMode::Frame);
    let sample = render(RampMode::Sample);
    let interp = render(RampMode::Interp);

    let expected_len = 40 * 12;
    for (name, out) in [
        ("off", &off),
        ("frame", &frame),
        ("sample", &sample),
        ("interp", &interp),
    ] {
        assert_eq!(out.len(), expected_len, "{name}: wrong output length");
        assert!(
            out.iter().all(|x| x.is_finite()),
            "{name}: non-finite output"
        );
        let energy: f32 = out.iter().map(|x| x * x).sum();
        assert!(energy > 0.0, "{name}: produced silence");
    }

    let max_diff = |a: &[f32], b: &[f32]| {
        a.iter()
            .zip(b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0f32, f32::max)
    };

    assert!(max_diff(&off, &frame) > 1e-3, "Off vs Frame collapsed");
    assert!(max_diff(&off, &sample) > 1e-3, "Off vs Sample collapsed");
    assert!(max_diff(&off, &interp) > 1e-3, "Off vs Interp collapsed");
    assert!(
        max_diff(&frame, &sample) > 1e-3,
        "Frame vs Sample collapsed"
    );
    // Sample (position-space) and Interp (gain-space) interpolate the same
    // endpoints differently, so they diverge mid-block too.
    assert!(
        max_diff(&sample, &interp) > 1e-3,
        "Sample vs Interp collapsed"
    );
}

/// Regression: in binaural mode the object position ramps MUST advance.
/// The VBAP mix loop that normally drives `advance_ramp` is bypassed, so the
/// binaural branch advances them itself; before that fix every object stayed
/// at the ramp default [0,0,0] — dead centre, and rotation-invariant (the
/// zero vector ignores the head pose) — which rendered as near-mono audio
/// that did not react to head tracking.
#[test]
fn binaural_object_ramp_advances_and_lateralizes() {
    let layout = SpeakerLayout::preset("7.1.4").unwrap();
    let mut r = SpatialRenderer::new(
        layout,
        48_000,
        1,
        1,
        0.0,
        2.0,
        VbapTableMode::Cartesian {
            x_size: 21,
            y_size: 21,
            z_size: 9,
            z_neg_size: 9,
        },
        false,
        true,
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
        21,
        21,
        9,
        9,
    )
    .unwrap();
    r.control.live.write().binaural.output_mode = crate::live_params::OutputMode::Binaural;

    // One object channel ramping from the default [0,0,0] to hard right.
    // Broadband pseudo-noise input: head-shadow ILD is a high-frequency
    // phenomenon, so a DC input would show almost no ear asymmetry.
    let mut lcg: u32 = 0x1234_5678;
    let mut noise_block = move || -> Vec<f32> {
        (0..40)
            .map(|_| {
                lcg = lcg.wrapping_mul(1664525).wrapping_add(1013904223);
                (lcg >> 8) as f32 / (1u32 << 24) as f32 - 0.5
            })
            .collect()
    };
    let pcm = noise_block();
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([1.0, 0.0, 0.0]),
        sample_pos: Some(0),
    }];

    let first = r.render_frame(&pcm, 1, &event, Vec::new(), false).unwrap();
    assert_eq!(
        first.samples.len(),
        40 * 2,
        "binaural output must be stereo"
    );

    // Let the ramp finish and the ITD delay lines / HRIR tails settle, then
    // measure ear energies over a few blocks.
    let (mut e_l, mut e_r) = (0.0f32, 0.0f32);
    for i in 0..8 {
        let pcm = noise_block();
        let out = r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();
        if i >= 4 {
            for s in out.samples.chunks_exact(2) {
                e_l += s[0] * s[0];
                e_r += s[1] * s[1];
            }
        }
    }

    let pos = r
        .channel_states
        .lock()
        .get(&0)
        .expect("channel state")
        .ramp
        .current_position;
    assert!(
        pos[0] > 0.99,
        "object ramp did not advance in binaural mode: current_position = {pos:?}"
    );
    assert!(e_l + e_r > 0.0, "binaural output is silent");
    assert!(
        e_r > 1.5 * e_l,
        "hard-right object not lateralized: E_L={e_l} E_R={e_r}"
    );
}

/// Regression: the master gain must scale the binaural output exactly like
/// it scales the speaker path (it used to be applied only in the VBAP
/// branch, so the master control was inert on headphones).
#[test]
fn binaural_output_follows_master_gain() {
    fn build() -> SpatialRenderer {
        let layout = SpeakerLayout::preset("7.1.4").unwrap();
        SpatialRenderer::new(
            layout,
            48_000,
            1,
            1,
            0.0,
            2.0,
            VbapTableMode::Cartesian {
                x_size: 21,
                y_size: 21,
                z_size: 9,
                z_neg_size: 9,
            },
            false,
            true,
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
            21,
            21,
            9,
            9,
        )
        .unwrap()
    }

    let pcm: Vec<f32> = (0..40).map(|i| (i * 7 % 13) as f32 / 13.0 - 0.5).collect();
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([0.5, 1.0, 0.0]),
        sample_pos: Some(0),
    }];

    let render = |master: f32| -> Vec<f32> {
        let mut r = build();
        {
            let mut live = r.control.live.write();
            live.binaural.output_mode = crate::live_params::OutputMode::Binaural;
            live.master_gain = master;
        }
        let mut out = Vec::new();
        for i in 0..4 {
            let ev: &[SpatialChannelEvent] = if i == 0 { &event } else { &[] };
            out = r
                .render_frame(&pcm, 1, ev, Vec::new(), false)
                .unwrap()
                .samples;
        }
        out
    };

    let unity = render(1.0);
    let double = render(2.0);
    assert!(unity.iter().any(|x| x.abs() > 1e-6), "silent baseline");
    for (a, b) in unity.iter().zip(&double) {
        assert!(
            (b - a * 2.0).abs() <= a.abs() * 1e-4 + 1e-6,
            "master gain not applied: {a} vs {b}"
        );
    }
}

/// In binaural mode the first two speaker param slots act as the L/R ear
/// channels (Studio's headphone rows drive them): muting slot 0 must silence
/// the left ear and leave the right ear untouched.
#[test]
fn binaural_ear_mute_uses_first_speaker_slots() {
    let layout = SpeakerLayout::preset("7.1.4").unwrap();
    let mut r = SpatialRenderer::new(
        layout,
        48_000,
        1,
        1,
        0.0,
        2.0,
        VbapTableMode::Cartesian {
            x_size: 21,
            y_size: 21,
            z_size: 9,
            z_neg_size: 9,
        },
        false,
        true,
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
        21,
        21,
        9,
        9,
    )
    .unwrap();
    {
        let mut live = r.control.live.write();
        live.binaural.output_mode = crate::live_params::OutputMode::Binaural;
        live.speakers.insert(
            0,
            crate::live_params::SpeakerLiveParams {
                muted: true,
                ..Default::default()
            },
        );
    }
    r.control
        .speaker_params_generation
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    let pcm: Vec<f32> = (0..40).map(|i| (i * 7 % 13) as f32 / 13.0 - 0.5).collect();
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([0.0, 1.0, 0.0]),
        sample_pos: Some(0),
    }];
    let mut out = Vec::new();
    for i in 0..4 {
        let ev: &[SpatialChannelEvent] = if i == 0 { &event } else { &[] };
        out = r
            .render_frame(&pcm, 1, ev, Vec::new(), false)
            .unwrap()
            .samples;
    }
    let e_l: f32 = out.iter().step_by(2).map(|x| x * x).sum();
    let e_r: f32 = out.iter().skip(1).step_by(2).map(|x| x * x).sum();
    assert!(e_l == 0.0, "left ear not silenced: {e_l}");
    assert!(e_r > 1e-6, "right ear should still play: {e_r}");
}

/// Binaural mode must carry the speaker path's overload policy (issue #149):
/// the clip flag always fires above 0 dBFS (with the ear in the first two
/// speaker slots, the same slots the headphone L/R rows ride), and auto-gain
/// folds the correction into the shared master gain.
#[test]
fn binaural_clipping_flags_ear_and_auto_gain_reduces_master() {
    fn build() -> SpatialRenderer {
        let layout = SpeakerLayout::preset("7.1.4").unwrap();
        SpatialRenderer::new(
            layout,
            48_000,
            1,
            1,
            0.0,
            2.0,
            VbapTableMode::Cartesian {
                x_size: 21,
                y_size: 21,
                z_size: 9,
                z_neg_size: 9,
            },
            false,
            true,
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
            21,
            21,
            9,
            9,
        )
        .unwrap()
    }

    let pcm: Vec<f32> = (0..40).map(|i| (i * 7 % 13) as f32 / 13.0 - 0.5).collect();
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: false,
        gain_db: Some(0),
        ramp_length: Some(40),
        size: Some([0.0, 0.0, 0.0]),
        position: Some([0.5, 1.0, 0.0]),
        sample_pos: Some(0),
    }];

    let render = |auto_gain: bool| -> SpatialRenderer {
        let mut r = build();
        {
            let mut live = r.control.live.write();
            live.binaural.output_mode = crate::live_params::OutputMode::Binaural;
            // Hot enough that the HRIR-summed stereo bus exceeds 0 dBFS.
            live.master_gain = 16.0;
            live.auto_gain = auto_gain;
        }
        for i in 0..4 {
            let ev: &[SpatialChannelEvent] = if i == 0 { &event } else { &[] };
            r.render_frame(&pcm, 1, ev, Vec::new(), false).unwrap();
        }
        r
    };

    // Auto-gain off: the flag must still fire (UI indicators), the master
    // gain must stay untouched.
    let r = render(false);
    let clip = r.control.take_clip_pending();
    assert!(
        matches!(clip, Some(0) | Some(1)),
        "clip flag not raised for an ear slot: {clip:?}"
    );
    assert!(!r.auto_gain_triggered(), "auto-gain fired while disabled");
    assert_eq!(r.control.live.read().master_gain, 16.0);

    // Auto-gain on: correction folded into the master gain, trigger visible.
    let r = render(true);
    assert!(
        matches!(r.control.take_clip_pending(), Some(0) | Some(1)),
        "clip flag not raised with auto-gain on"
    );
    assert!(r.auto_gain_triggered(), "auto-gain did not trigger");
    let master = r.control.live.read().master_gain;
    assert!(
        master < 16.0,
        "master gain not reduced by auto-gain: {master}"
    );
}

/// In binaural mode a bed mapped to a `spatialize: false` speaker (the LFE)
/// keeps its direct-routing intent (issue #156): both ears receive the
/// identical dry feed at constant power — no HRIR tail, no ITD, no head-pose
/// effect — instead of being HRTF-spatialized at the sub's direction.
#[test]
fn binaural_lfe_bed_feeds_both_ears_equally_and_dry() {
    let layout = SpeakerLayout::preset("7.1.4").unwrap();
    let mut r = SpatialRenderer::new(
        layout,
        48_000,
        1,
        1,
        0.0,
        2.0,
        VbapTableMode::Cartesian {
            x_size: 21,
            y_size: 21,
            z_size: 9,
            z_neg_size: 9,
        },
        false,
        true,
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
        21,
        21,
        9,
        9,
    )
    .unwrap();
    // Channel 0 = direct LFE → the LFE speaker (index 3, spatialize:false).
    r.configure_channel_routing(&[ChannelRoute::Direct(bridge_api::RChannelLabel::LFE)]);
    {
        let mut live = r.control.live.write();
        live.binaural.output_mode = crate::live_params::OutputMode::Binaural;
    }

    let n = 40;
    let mut pcm = vec![0.0f32; n];
    pcm[0] = 0.8; // impulse: any post-render tail would expose a convolver
    let event = vec![SpatialChannelEvent {
        channel_idx: 0,
        is_bed: true,
        gain_db: Some(0),
        ramp_length: Some(0),
        size: None,
        position: None,
        sample_pos: Some(0),
    }];
    // Warm up the per-channel gain slew (channels fade in over
    // GAIN_SLEW_SECS from silence) with one long silent block, so the
    // asserted block runs at settled gain.
    let warmup = vec![0.0f32; 4096];
    r.render_frame(&warmup, 1, &event, Vec::new(), false)
        .unwrap();
    let out = r
        .render_frame(&pcm, 1, &event, Vec::new(), false)
        .unwrap()
        .samples;

    assert_eq!(out.len(), n * 2);
    let expected = 0.8 * std::f32::consts::FRAC_1_SQRT_2;
    assert!(
        (out[0] - expected).abs() < 1e-6,
        "constant-power direct feed expected {expected}, got {}",
        out[0]
    );
    assert_eq!(out[0], out[1], "both ears must carry the identical feed");
    assert!(
        out[2..].iter().all(|&x| x == 0.0),
        "direct feed must not ring (no HRIR/ITD tail)"
    );
}

/// After claiming the FP environment, subnormal arithmetic must flush to
/// zero on this thread (issue #154) — without FTZ/DAZ the product below
/// stays a nonzero subnormal.
#[test]
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn render_thread_flushes_denormals() {
    ensure_denormals_flushed();
    // MIN_POSITIVE is the smallest *normal*; dividing it makes a subnormal.
    let tiny = std::hint::black_box(f32::MIN_POSITIVE) / std::hint::black_box(4.0f32);
    let prod = std::hint::black_box(tiny) * std::hint::black_box(1.0f32);
    assert_eq!(
        prod, 0.0,
        "subnormals must flush to zero on the render thread (got {prod:e})"
    );
}

/// A layout without back floor speakers (5.1.4-style: sides + four tops) must
/// still render content placed at the back floor corners — the direction sits
/// outside the speaker hull (below the SL↔TBL / SR↔TBR faces) and must fold
/// onto the closest hull face, never to silence (issue #169). Mirrors the field
/// config: precomputed cartesian table, zero-length ramp, steady state.
#[test]
fn back_floor_position_renders_on_layout_without_back_speakers() {
    const LAYOUT_5_1_4: &str = r#"
radius_m: 1.0
speakers:
- { name: FL,  coord_mode: cartesian, x: -1.0, y:  1.0, z: 0.0, spatialize: true }
- { name: FR,  coord_mode: cartesian, x:  1.0, y:  1.0, z: 0.0, spatialize: true }
- { name: C,   coord_mode: cartesian, x:  0.0, y:  1.0, z: 0.0, spatialize: true }
- { name: LFE, coord_mode: cartesian, x:  1.0, y:  1.0, z: -1.0, spatialize: false }
- { name: SL,  coord_mode: cartesian, x: -1.0, y:  0.0, z: 0.0, spatialize: true }
- { name: SR,  coord_mode: cartesian, x:  1.0, y:  0.0, z: 0.0, spatialize: true }
- { name: TFL, coord_mode: cartesian, x: -1.0, y:  1.0, z: 1.0, spatialize: true }
- { name: TFR, coord_mode: cartesian, x:  1.0, y:  1.0, z: 1.0, spatialize: true }
- { name: TBL, coord_mode: cartesian, x: -1.0, y: -1.0, z: 1.0, spatialize: true }
- { name: TBR, coord_mode: cartesian, x:  1.0, y: -1.0, z: 1.0, spatialize: true }
"#;

    fn build() -> SpatialRenderer {
        SpatialRenderer::new(
            SpeakerLayout::from_yaml_str(LAYOUT_5_1_4).unwrap(),
            48_000,
            1,
            90,
            0.25,
            2.0,
            VbapTableMode::Cartesian {
                x_size: 63,
                y_size: 63,
                z_size: 16,
                z_neg_size: 0,
            },
            false,
            true,
            DistanceModel::None,
            false,
            1.0,
            1.0,
            0.0,
            1.0,
            false,
            [2.0, 2.0, 1.0],
            1.0,
            0.466667,
            0.5,
            0.0,
            false,
            true,
            true,
            1.0,
            1.0,
            PreferredEvaluationMode::PrecomputedCartesian,
            LiveEvaluationMode::PrecomputedCartesian,
            63,
            63,
            16,
            0,
        )
        .unwrap()
    }

    let pcm: Vec<f32> = (0..40)
        .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
        .collect();
    // Steady-state per-speaker peaks: apply the event, then measure a second
    // frame so a ramp (if any) cannot mask a zero steady-state gain.
    let peaks_at = |pos: [f64; 3]| -> Vec<f32> {
        let event = vec![SpatialChannelEvent {
            channel_idx: 0,
            is_bed: false,
            gain_db: Some(0),
            ramp_length: Some(0),
            size: None,
            position: Some(pos),
            sample_pos: Some(0),
        }];
        let mut r = build();
        r.render_frame(&pcm, 1, &event, Vec::new(), false).unwrap();
        let out = r.render_frame(&pcm, 1, &[], Vec::new(), false).unwrap();
        let n = 10usize;
        let mut peaks = vec![0.0f32; n];
        for (k, &s) in out.samples.iter().enumerate() {
            let c = k % n;
            peaks[c] = peaks[c].max(s.abs());
        }
        peaks
    };

    // Control: the exact side position renders on SL.
    let side = peaks_at([-1.0, 0.0, 0.0]);
    assert!(
        side[4] > 1e-3,
        "side-left content must render on SL (peaks {side:?})"
    );

    // Back floor corners: outside the hull; must fold onto the nearest face
    // (side surround and/or top back of that side), never to silence.
    for (pos, near) in [
        ([-1.0f64, -1.0, 0.0], [4usize, 8]), // SL / TBL
        ([1.0, -1.0, 0.0], [5, 9]),          // SR / TBR
    ] {
        let peaks = peaks_at(pos);
        let near_peak = near.iter().map(|&c| peaks[c]).fold(0.0f32, f32::max);
        let total: f32 = peaks.iter().sum();
        assert!(
            near_peak > 1e-3,
            "back-floor content at {pos:?} must fold onto the near speakers (peaks {peaks:?})"
        );
        assert!(
            near_peak >= total * 0.5,
            "back-floor fold at {pos:?} should stay local (peaks {peaks:?})"
        );
    }
}

// TODO: Add integration test with real spatial metadata
// For now, testing is done via real spatial audio content decoding
