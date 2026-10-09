//! Pure-Rust VBAP backend — drop-in replacement for `saf_backend.rs`.
//!
//! Used when the `saf_vbap` feature is disabled (no C FFI, no external library).

use super::{Gains, normalized_spread_to_degrees};
use crate::spatial_vbap::vbap_native::{
    DummyRing, OutOfHullMode, invert_ls_mtx_3d, prepare_triangulation, vbap3d,
};

/// Pure-Rust equivalent of `SpartaVbapLayout`.
///
/// Owns the triangulation and inverse speaker matrices produced by
/// `prepare_triangulation` + `invert_ls_mtx_3d`. Computes VBAP gains directly
/// via [`Self::vbap_gains`]; the panner stores one instance and samples it.
pub(crate) struct NativeVbapLayout {
    /// Number of *real* (non-dummy) speakers — the size of the returned `Gains`.
    pub(crate) n_speakers: usize,
    pub(crate) n_faces: usize,
    /// Number of virtual loudspeakers at the centre of coplanar hull faces
    /// (`vbap_native::Triangulation`).
    pub(crate) n_centres: usize,
    /// Total speaker count used for triangulation (real + virtual poles +
    /// virtual centres).
    n_eff: usize,
    #[allow(dead_code)]
    u_spkr: Vec<[f32; 3]>,
    ls_groups: Vec<[usize; 3]>,
    layout_inv_mtx: Vec<[f32; 9]>,
    /// Per-effective-speaker flag: `true` for the virtual ±90° pole(s) injected
    /// for triangulation. `vbap3d` uses this to fold dummy gain back into real
    /// speakers of the matched triangle instead of letting it be silently dropped.
    is_dummy: Vec<bool>,
    /// Out-of-hull rendering mode, baked at construction (it shapes the
    /// triangulation itself in `VirtualPoles`).
    mode: OutOfHullMode,
    /// Downmix rings of the virtual loudspeakers: the centres of coplanar
    /// faces in every mode, the poles in `VirtualPoles`.
    dummy_rings: Vec<DummyRing>,
}

impl NativeVbapLayout {
    /// Build a layout from speaker directions (azimuth, elevation in degrees).
    ///
    /// The real layout is triangulated first. If that fails, virtual speakers at
    /// ±90° elevation are injected as a fallback so the 3D convex hull can be
    /// built (in [`OutOfHullMode::VirtualPoles`], also at any pole the real
    /// hull leaves uncovered). Every coplanar hull face is closed around a
    /// virtual centre (`vbap_native::Triangulation`). Virtual gains are
    /// stripped before returning.
    pub fn from_speaker_dirs(
        speaker_dirs_deg: &[[f32; 2]],
        mode: OutOfHullMode,
    ) -> Result<Self, String> {
        let n_real = speaker_dirs_deg.len();
        let tri = prepare_triangulation(speaker_dirs_deg, true, true, mode)
            .ok_or_else(|| "find_ls_triplets failed".to_string())?;

        let n_eff = tri.dirs.len();
        debug_assert_eq!(tri.is_dummy.len(), n_eff);

        if tri.ls_groups.is_empty() {
            return Err("No valid loudspeaker triangles found".to_string());
        }

        let layout_inv_mtx = invert_ls_mtx_3d(&tri.u_spkr, &tri.ls_groups);
        let n_faces = tri.ls_groups.len();
        let n_centres = tri.centres();
        let dummy_rings = tri.downmix_rings(mode);

        Ok(Self {
            n_speakers: n_real,
            n_faces,
            n_centres,
            n_eff,
            u_spkr: tri.u_spkr,
            ls_groups: tri.ls_groups,
            layout_inv_mtx,
            is_dummy: tri.is_dummy,
            mode,
            dummy_rings,
        })
    }

    /// Compute VBAP gains for a single source direction and spread.
    /// Returns gains for real speakers only (dummy columns are stripped).
    pub fn vbap_gains(
        &self,
        azimuth_deg: f32,
        elevation_deg: f32,
        spread: f32,
    ) -> Result<Gains, String> {
        let spread_deg = normalized_spread_to_degrees(spread);
        let src_dirs = [[azimuth_deg, elevation_deg]];

        let gain_vec = vbap3d(
            &src_dirs,
            self.n_eff,
            &self.ls_groups,
            spread_deg,
            &self.layout_inv_mtx,
            &self.is_dummy,
            self.mode,
            &self.dummy_rings,
        );

        // Strip dummy speaker columns — keep only the first n_speakers entries.
        Ok(Gains::from_slice(&gain_vec[..self.n_speakers]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Standard horizontal layout (no top/bottom speakers) — exercises both
    /// dummy injections (±90°) so the redistribution path is hit at both poles.
    fn horizontal_7_layout() -> [[f32; 2]; 7] {
        [
            [0.0, 0.0],    // C
            [-30.0, 0.0],  // L
            [30.0, 0.0],   // R
            [-110.0, 0.0], // LS
            [110.0, 0.0],  // RS
            [-150.0, 0.0], // LRS
            [150.0, 0.0],  // RRS
        ]
    }

    fn rms(g: &Gains) -> f32 {
        (0..g.len()).map(|i| g[i] * g[i]).sum::<f32>().sqrt()
    }

    #[test]
    fn test_real_height_speakers_do_not_force_dummy_poles() {
        let dirs = [
            [-30.0_f32, 0.0],
            [30.0, 0.0],
            [-110.0, 0.0],
            [110.0, 0.0],
            [-45.0, 36.0],
            [45.0, 36.0],
        ];

        // Pinned to Blend: the assertion is about the *fallback* mechanism
        // (dummies only when the real layout cannot triangulate) — in
        // VirtualPoles a nadir dummy is injected by design.
        let mode = OutOfHullMode::Blend {
            power: OutOfHullMode::DEFAULT_BLEND_POWER,
        };
        let layout = NativeVbapLayout::from_speaker_dirs(&dirs, mode).unwrap();

        assert_eq!(layout.n_speakers, dirs.len());
        // No pole; the planar front and rear quads add their centres.
        assert_eq!(layout.n_centres, 2);
        assert_eq!(layout.n_eff, dirs.len() + layout.n_centres);
        assert!(layout.is_dummy.iter().all(|flag| !flag));
    }

    #[test]
    fn test_coplanar_layout_still_falls_back_to_dummy_poles() {
        let dirs = horizontal_7_layout();
        let layout = NativeVbapLayout::from_speaker_dirs(&dirs, OutOfHullMode::default()).unwrap();

        assert_eq!(layout.n_speakers, dirs.len());
        assert_eq!(layout.n_eff, dirs.len() + 2);
        assert_eq!(layout.is_dummy.iter().filter(|flag| **flag).count(), 2);
    }

    #[test]
    fn test_vertical_z_axis_no_silence() {
        let dirs = horizontal_7_layout();
        let layout = NativeVbapLayout::from_speaker_dirs(&dirs, OutOfHullMode::default()).unwrap();

        // (X=0, Y=0, Z>0) → elevation = +90° (zenith)
        let zenith = layout.vbap_gains(0.0, 90.0, 0.0).unwrap();
        assert!(
            rms(&zenith) > 0.5,
            "zenith should not be silent, got rms={}",
            rms(&zenith)
        );

        // (X=0, Y=0, Z<0) → elevation = -90° (nadir)
        let nadir = layout.vbap_gains(0.0, -90.0, 0.0).unwrap();
        assert!(
            rms(&nadir) > 0.5,
            "nadir should not be silent, got rms={}",
            rms(&nadir)
        );
    }

    #[test]
    fn test_z_sweep_continuity() {
        let dirs = horizontal_7_layout();
        let layout = NativeVbapLayout::from_speaker_dirs(&dirs, OutOfHullMode::default()).unwrap();

        // Sweep elevation from -90° to +90°, azimuth pinned (matches the
        // X=0, Y=0, Z varying trajectory after `adm_to_spherical`).
        for el_i in -9..=9 {
            let el = el_i as f32 * 10.0;
            let g = layout.vbap_gains(0.0, el, 0.0).unwrap();
            assert!(
                rms(&g) > 0.5,
                "energy dropout at elevation {el}°: rms={}",
                rms(&g)
            );
        }
    }

    #[test]
    fn test_energy_conservation_at_pole() {
        let dirs = horizontal_7_layout();
        let layout = NativeVbapLayout::from_speaker_dirs(&dirs, OutOfHullMode::default()).unwrap();

        // After redistribution, the RMS over real speakers should be ≈ 1.0
        // (energy that used to leak onto the dummy is now folded back).
        for el in [-90.0_f32, -75.0, 0.0, 75.0, 90.0] {
            let g = layout.vbap_gains(0.0, el, 0.0).unwrap();
            let r = rms(&g);
            assert!(
                (r - 1.0).abs() < 0.05,
                "energy not conserved at elevation {el}°: rms={r}"
            );
        }
    }

    #[test]
    fn test_coplanar_speakers_position_aware() {
        // 4 speakers all at el=0 — previously would fail triangulation.
        let dirs = [
            [-90.0_f32, 0.0], // Left
            [90.0, 0.0],      // Right
            [0.0, 0.0],       // Front
            [180.0, 0.0],     // Rear
        ];
        let layout = NativeVbapLayout::from_speaker_dirs(&dirs, OutOfHullMode::default())
            .expect("should succeed with dummy speakers");

        assert_eq!(layout.n_speakers, 4);

        // Source at left (az=-90) → left speaker should dominate
        let gains_left = layout.vbap_gains(-90.0, 0.0, 0.0).unwrap();
        let gains_right = layout.vbap_gains(90.0, 0.0, 0.0).unwrap();

        assert_eq!(gains_left.len(), 4);
        // Left speaker (index 0) should have highest gain when source is on the left
        let left_at_left: f32 = gains_left[0];
        let right_at_left: f32 = gains_left[1];
        assert!(
            left_at_left > right_at_left,
            "left speaker gain {left_at_left} should exceed right {right_at_left} for left source"
        );
        // Right speaker (index 1) should have highest gain when source is on the right
        let left_at_right: f32 = gains_right[0];
        let right_at_right: f32 = gains_right[1];
        assert!(
            right_at_right > left_at_right,
            "right speaker gain {right_at_right} should exceed left {left_at_right} for right source"
        );
    }

    /// 7.1.4-style layout: bed ring at 0°, four heights at +35°. The real hull
    /// covers the zenith but not the nadir.
    fn layout_714() -> [[f32; 2]; 11] {
        [
            [0.0, 0.0],
            [-30.0, 0.0],
            [30.0, 0.0],
            [-90.0, 0.0],
            [90.0, 0.0],
            [-150.0, 0.0],
            [150.0, 0.0],
            [-45.0, 35.26],
            [45.0, 35.26],
            [-135.0, 35.26],
            [135.0, 35.26],
        ]
    }

    #[test]
    fn test_virtual_poles_closes_uncovered_nadir_only() {
        let dirs = layout_714();
        let layout =
            NativeVbapLayout::from_speaker_dirs(&dirs, OutOfHullMode::VirtualPoles).unwrap();

        // Zenith is covered by the height ring — only the nadir needs a pole.
        // The rear quad and the ceiling, both planar, add their centres.
        assert_eq!(layout.n_speakers, dirs.len());
        assert_eq!(layout.is_dummy.iter().filter(|pole| **pole).count(), 1);
        assert_eq!(layout.n_centres, 2);
        assert_eq!(layout.n_eff, dirs.len() + 1 + layout.n_centres);
        let poles: Vec<&DummyRing> = layout
            .dummy_rings
            .iter()
            .filter(|ring| layout.is_dummy[ring.dummy])
            .collect();
        assert_eq!(poles.len(), 1);
        // The nadir pole downmixes onto bed-ring speakers only.
        for &s in &poles[0].ring {
            assert!(
                dirs[s][1].abs() < 1e-3,
                "nadir ring member {s} should be a bed speaker, got el={}",
                dirs[s][1]
            );
        }
    }

    #[test]
    fn test_virtual_poles_full_energy_at_poles() {
        for dirs in [&layout_714()[..], &horizontal_7_layout()[..]] {
            let layout =
                NativeVbapLayout::from_speaker_dirs(dirs, OutOfHullMode::VirtualPoles).unwrap();
            for el in [-90.0_f32, -60.0, -30.0, 0.0, 30.0, 60.0, 90.0] {
                let g = layout.vbap_gains(20.0, el, 0.0).unwrap();
                let r = rms(&g);
                assert!(
                    (r - 1.0).abs() < 0.05,
                    "energy not conserved at elevation {el}°: rms={r}"
                );
            }
        }
    }

    #[test]
    fn test_virtual_poles_diffuse_and_continuous_at_nadir() {
        let dirs = layout_714();
        let layout =
            NativeVbapLayout::from_speaker_dirs(&dirs, OutOfHullMode::VirtualPoles).unwrap();

        // At the exact nadir all the energy sits on the pole speaker, so the
        // downmix must spread it uniformly over the bed ring.
        let nadir = layout.vbap_gains(0.0, -90.0, 0.0).unwrap();
        let bed: Vec<f32> = (0..7).map(|i| nadir[i]).collect();
        let expected = 1.0 / (7.0_f32).sqrt();
        for (i, g) in bed.iter().enumerate() {
            assert!(
                (g - expected).abs() < 1e-3,
                "nadir gain on bed speaker {i} should be uniform {expected}, got {g}"
            );
        }
        for i in 7..11 {
            assert!(
                nadir[i] < 1e-3,
                "height speaker {i} should be silent at nadir, got {}",
                nadir[i]
            );
        }

        // Crossing the pole in azimuth must not step: the ring downmix does
        // not depend on which pole triangle matched.
        let mut prev = layout.vbap_gains(-0.05, -89.9, 0.0).unwrap();
        for az_i in 0..=20 {
            let az = -0.05 + az_i as f32 * 0.005;
            let g = layout.vbap_gains(az, -89.9, 0.0).unwrap();
            let l2: f32 = (0..g.len())
                .map(|i| (g[i] - prev[i]) * (g[i] - prev[i]))
                .sum::<f32>()
                .sqrt();
            assert!(
                l2 < 0.01,
                "gain-vector jump {l2} across a 0.005° azimuth step at the nadir"
            );
            prev = g;
        }
    }

    #[test]
    fn test_blend_power_is_configurable() {
        let dirs = layout_714();
        let below = |power: f32| {
            let layout =
                NativeVbapLayout::from_speaker_dirs(&dirs, OutOfHullMode::Blend { power }).unwrap();
            layout.vbap_gains(66.5, -45.0, 0.0).unwrap()
        };

        // Full level regardless of power.
        for power in [1.0_f32, 12.0, 64.0] {
            let g = below(power);
            let r = rms(&g);
            assert!(
                (r - 1.0).abs() < 0.05,
                "energy not conserved at power {power}: rms={r}"
            );
        }

        // Higher power concentrates the image: the number of speakers carrying
        // significant gain must not grow as power rises.
        let count_active = |g: &Gains| (0..g.len()).filter(|&i| g[i] > 0.05).count();
        let wide = below(1.0);
        let sharp = below(64.0);
        assert!(
            count_active(&sharp) <= count_active(&wide),
            "power 64 should localise at least as much as power 1 ({} vs {})",
            count_active(&sharp),
            count_active(&wide)
        );
    }

    /// The legacy mode must reproduce the historical measurements from the
    /// dsp-validation report: on 7.1.4 at az 66.5°, energy decays with the
    /// fold angle (−0.78 dB at −22.6°, −3.22 dB at −45°) down to silence at
    /// the nadir.
    #[test]
    fn test_fade_mode_reproduces_original_decay() {
        let dirs = layout_714();
        let layout = NativeVbapLayout::from_speaker_dirs(&dirs, OutOfHullMode::Fade).unwrap();

        let rms_at = |el: f32| {
            let g = layout.vbap_gains(66.5, el, 0.0).unwrap();
            rms(&g)
        };

        // In-hull stays unit energy.
        assert!((rms_at(0.0) - 1.0).abs() < 0.05, "in-hull must stay full");
        // Below the hull the fade applies: the historical dB figures.
        assert!(
            (rms_at(-22.6) - 0.914).abs() < 0.02,
            "-22.6°: expected ≈ -0.78 dB, got {}",
            rms_at(-22.6)
        );
        assert!(
            (rms_at(-45.0) - 0.690).abs() < 0.02,
            "-45°: expected ≈ -3.22 dB, got {}",
            rms_at(-45.0)
        );
        assert!(rms_at(-90.0) < 1e-3, "nadir must be silent in Fade mode");
    }

    /// Four loudspeakers at 0° and four over them at 35°, at ±45° and ±135°:
    /// a cube. Every side and the ceiling are planar quads.
    fn cube_4_4_layout() -> [[f32; 2]; 8] {
        [
            [45.0, 0.0],
            [-45.0, 0.0],
            [135.0, 0.0],
            [-135.0, 0.0],
            [45.0, 35.0],
            [-45.0, 35.0],
            [135.0, 35.0],
            [-135.0, 35.0],
        ]
    }

    /// The BBC System G measured set that showed the asymmetry: nine at 0°,
    /// two heights at ±45° and two at ±110°, all at 40° (SAF azimuths, left
    /// positive). Its rear quad (±135° at 0°, ±110° at 40°) and its ceiling
    /// are planar.
    fn bbc_system_g_layout() -> [[f32; 2]; 13] {
        [
            [0.0, 0.0],
            [45.0, 0.0],
            [-45.0, 0.0],
            [30.0, 0.0],
            [-30.0, 0.0],
            [90.0, 0.0],
            [-90.0, 0.0],
            [135.0, 0.0],
            [-135.0, 0.0],
            [45.0, 40.0],
            [-45.0, 40.0],
            [110.0, 40.0],
            [-110.0, 40.0],
        ]
    }

    /// The loudspeaker mirrored across the median plane: the one at the
    /// opposite azimuth and the same elevation.
    fn mirror_index(dirs: &[[f32; 2]], i: usize) -> usize {
        dirs.iter()
            .position(|d| (d[0] + dirs[i][0]).abs() < 1e-3 && (d[1] - dirs[i][1]).abs() < 1e-3)
            .unwrap_or_else(|| panic!("no mirror of {:?}", dirs[i]))
    }

    fn modes() -> [OutOfHullMode; 3] {
        [
            OutOfHullMode::VirtualPoles,
            OutOfHullMode::Blend {
                power: OutOfHullMode::DEFAULT_BLEND_POWER,
            },
            OutOfHullMode::Fade,
        ]
    }

    #[test]
    fn coplanar_faces_get_a_virtual_centre_in_every_mode() {
        for mode in modes() {
            let cube = NativeVbapLayout::from_speaker_dirs(&cube_4_4_layout(), mode).unwrap();
            assert_eq!(cube.n_centres, 5, "{mode:?}: four sides and the ceiling");
            assert!(cube.n_faces >= 20, "{mode:?}: a fan of four per face");
            let bbc = NativeVbapLayout::from_speaker_dirs(&bbc_system_g_layout(), mode).unwrap();
            assert_eq!(bbc.n_centres, 2, "{mode:?}: the rear quad and the ceiling");
            let seven_one_four = NativeVbapLayout::from_speaker_dirs(&layout_714(), mode).unwrap();
            assert_eq!(
                seven_one_four.n_centres, 2,
                "{mode:?}: the rear quad and the ceiling"
            );
            // A ring closed by two poles has no planar face.
            let ring = NativeVbapLayout::from_speaker_dirs(&horizontal_7_layout(), mode).unwrap();
            assert_eq!(ring.n_centres, 0, "{mode:?}");
        }
    }

    #[test]
    fn a_virtual_centre_plays_its_face_at_equal_power() {
        let dirs = cube_4_4_layout();
        let unit = |[az, el]: [f32; 2]| {
            let (a, e) = (az.to_radians(), el.to_radians());
            [e.cos() * a.cos(), e.cos() * a.sin(), e.sin()]
        };
        let centre_of = |face: [usize; 4]| {
            let mut sum = [0.0f32; 3];
            for i in face {
                for (acc, x) in sum.iter_mut().zip(unit(dirs[i])) {
                    *acc += x;
                }
            }
            let horiz = (sum[0] * sum[0] + sum[1] * sum[1]).sqrt();
            (
                sum[1].atan2(sum[0]).to_degrees(),
                sum[2].atan2(horiz).to_degrees(),
            )
        };
        for mode in modes() {
            let layout = NativeVbapLayout::from_speaker_dirs(&dirs, mode).unwrap();
            // The ceiling's centre is the zenith; the front face's is ahead.
            for face in [[4, 5, 6, 7], [0, 1, 4, 5]] {
                let (az, el) = centre_of(face);
                let g = layout.vbap_gains(az, el, 0.0).unwrap();
                for i in 0..dirs.len() {
                    let want = if face.contains(&i) { 0.5 } else { 0.0 };
                    assert!(
                        (g[i] - want).abs() < 1e-3,
                        "{mode:?} ({az:.1}, {el:.1}): speaker {i} got {}, wanted {want}",
                        g[i]
                    );
                }
            }
        }
    }

    /// Before: the hull split each planar quad along one diagonal, so an
    /// object dead centre at the rear of the BBC set played 0.81 / 0.30 on
    /// the two backs and 0.50 / 0 on the two top backs, and a direction and
    /// its mirror did not get mirrored gains.
    #[test]
    fn gains_are_mirror_symmetric_across_coplanar_faces() {
        let bbc = bbc_system_g_layout();
        let seven_one_four = layout_714();
        let cube = cube_4_4_layout();
        let probes: [(f32, f32); 11] = [
            (180.0, 20.0),
            (160.0, 25.0),
            (150.0, 10.0),
            (0.0, 90.0),
            (20.0, 70.0),
            (170.0, 60.0),
            (100.0, 50.0),
            (0.0, 20.0),
            (60.0, 30.0),
            (180.0, -30.0),
            (90.0, 38.0),
        ];
        for (name, dirs) in [
            ("bbc", &bbc[..]),
            ("7.1.4", &seven_one_four[..]),
            ("cube", &cube[..]),
        ] {
            for mode in modes() {
                let layout = NativeVbapLayout::from_speaker_dirs(dirs, mode).unwrap();
                for (az, el) in probes {
                    let g = layout.vbap_gains(az, el, 0.0).unwrap();
                    let m = layout.vbap_gains(-az, el, 0.0).unwrap();
                    for i in 0..dirs.len() {
                        let j = mirror_index(dirs, i);
                        assert!(
                            (g[i] - m[j]).abs() < 1e-4,
                            "{name} {mode:?} ({az}, {el}): speaker {i} {} vs its mirror {j} {}",
                            g[i],
                            m[j]
                        );
                    }
                    if !matches!(mode, OutOfHullMode::Fade) || el >= 0.0 {
                        assert!((rms(&g) - 1.0).abs() < 1e-3, "{name} {mode:?} ({az}, {el})");
                    }
                }
            }
        }
    }

    #[test]
    fn an_object_dead_centre_at_the_rear_plays_both_backs_alike() {
        let dirs = bbc_system_g_layout();
        let (bl, br, tsl, tsr) = (7, 8, 11, 12);
        for mode in modes() {
            let layout = NativeVbapLayout::from_speaker_dirs(&dirs, mode).unwrap();
            // Inside the rear quad: all four of its loudspeakers, in pairs.
            let g = layout.vbap_gains(180.0, 20.0, 0.0).unwrap();
            assert!(
                (g[bl] - g[br]).abs() < 1e-5 && g[bl] > 0.1,
                "{mode:?}: {} / {}",
                g[bl],
                g[br]
            );
            assert!(
                (g[tsl] - g[tsr]).abs() < 1e-5 && g[tsl] > 0.1,
                "{mode:?}: {} / {}",
                g[tsl],
                g[tsr]
            );
            // On its bottom edge: the pairwise VBAP as before, nothing above.
            let g = layout.vbap_gains(180.0, 0.0, 0.0).unwrap();
            let half = std::f32::consts::FRAC_1_SQRT_2;
            assert!(
                (g[bl] - half).abs() < 1e-3 && (g[br] - half).abs() < 1e-3,
                "{mode:?}"
            );
            assert!(g[tsl] < 1e-4 && g[tsr] < 1e-4, "{mode:?}");
        }
    }
}
