use std::f32::consts::FRAC_1_SQRT_2;

use anyhow::Result;

use super::room_transform::room_scaled_position;
use super::{BackendCapabilities, GainModel, RenderRequest, RenderResponse};
use crate::spatial_vbap::{Gains, spherical_to_adm};
use crate::speaker_layout::SpeakerLayout;

/// Triangulation-free VBAP for degenerate geometry — any speaker set the full
/// panner cannot triangulate (`find_ls_triplets` fails): 1–2 speakers, or larger
/// sets whose directions are collinear / coplanar / include a speaker at the
/// listener. Used for crossover sub-bands and stereo/mono main layouts, and as
/// the fallback when the full panner build fails (see `backend_registry` /
/// `spatial_renderer::construction`), so a degenerate layout degrades the pan
/// instead of killing all audio.
///
/// Same model as VBAP — distance is ignored, only the projection of the speaker
/// and object **directions** onto the unit sphere matters — so an object crossing
/// from a degenerate region into a triangulated one stays consistent. Directional
/// speakers are panned by local pairwise VBAP between the two nearest directions
/// (well-defined even when the set can't be triangulated); speakers flagged
/// `omni` (sitting at the listener, e.g. a spatialised LFE — no usable direction)
/// always take a constant baseline share. Like `VbapBackend` it returns pure
/// panning gains; distance attenuation and diffuse blending are applied by the
/// shared decorators.
pub struct DegenerateVbapBackend {
    /// Speaker unit direction vectors (ADM), precomputed once from the az/el
    /// positions.
    speaker_dirs: Vec<[f32; 3]>,
    /// Per-speaker flag: `true` when the speaker sits at the listener (no usable
    /// direction) and should take a constant omnidirectional baseline share
    /// instead of being panned. Same length as `speaker_dirs`.
    omni: Vec<bool>,
}

#[inline]
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Each omni speaker's raw (pre-normalisation) gain. The directional pair carries
/// unit power, so at weight 0.5 a single omni speaker ends up with ~20% of the
/// power — present and audible (sub-bass / LFE) without dominating the pan.
const OMNI_WEIGHT: f32 = 0.5;

impl DegenerateVbapBackend {
    /// `positions` are speaker `[azimuth, elevation]` in degrees (the same
    /// room-adjusted directions VBAP receives). No speaker is treated as omni.
    pub fn new(positions: Vec<[f32; 2]>) -> Self {
        let omni = vec![false; positions.len()];
        Self::with_omni(positions, omni)
    }

    /// As [`Self::new`], but `omni[i]` marks speakers at the listener (no usable
    /// direction) that should take a constant baseline share. A length mismatch is
    /// treated as "no omni speakers".
    pub fn with_omni(positions: Vec<[f32; 2]>, mut omni: Vec<bool>) -> Self {
        if omni.len() != positions.len() {
            omni = vec![false; positions.len()];
        }
        let speaker_dirs = positions
            .iter()
            .map(|&[az, el]| {
                let (x, y, z) = spherical_to_adm(az, el, 1.0);
                [x, y, z]
            })
            .collect();
        Self { speaker_dirs, omni }
    }

    fn equal_power(n: usize) -> Gains {
        let mut g = Gains::zeroed(n);
        if n > 0 {
            let v = 1.0 / (n as f32).sqrt();
            for i in 0..n {
                g.set(i, v);
            }
        }
        g
    }

    /// Local pairwise VBAP between two speaker directions: solve g0*l0 + g1*l1 ≈ p
    /// (least squares over the speaker-pair plane), clamp negatives (out-of-arc →
    /// nearer speaker), then constant-power normalise. `a`/`b` are the precomputed
    /// dots `dot(l0, p)` / `dot(l1, p)`.
    fn pair_gains(l0: [f32; 3], l1: [f32; 3], a: f32, b: f32) -> (f32, f32) {
        let c = dot(l0, l1);
        let det = 1.0 - c * c;
        let (mut g0, mut g1) = if det < 1e-6 {
            // Coincident or antipodal speakers: no usable pair plane.
            (FRAC_1_SQRT_2, FRAC_1_SQRT_2)
        } else {
            ((a - c * b) / det, (b - c * a) / det)
        };
        g0 = g0.max(0.0);
        g1 = g1.max(0.0);
        let norm = (g0 * g0 + g1 * g1).sqrt();
        if norm > 1e-6 {
            (g0 / norm, g1 / norm)
        } else {
            (FRAC_1_SQRT_2, FRAC_1_SQRT_2)
        }
    }

    pub fn speaker_count(&self) -> usize {
        self.speaker_dirs.len()
    }

    pub fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        let n = self.speaker_dirs.len();
        if n == 0 {
            return RenderResponse {
                gains: Gains::zeroed(0),
            };
        }
        // One speaker: it gets all the energy regardless of position.
        if n == 1 {
            let mut g = Gains::zeroed(1);
            g.set(0, 1.0);
            return RenderResponse { gains: g };
        }

        // Object direction on the unit sphere (distance dropped via normalisation).
        let scaled = room_scaled_position(
            req.adm_position.map(|v| v as f32),
            req.room_ratio,
            req.room_ratio_rear,
            req.room_ratio_lower,
            req.room_ratio_center_blend,
        );
        let norm_p = dot(scaled, scaled).sqrt();
        if norm_p < 1e-9 {
            // Object at the listener: no direction → split equally.
            return RenderResponse {
                gains: Self::equal_power(n),
            };
        }
        let p = [scaled[0] / norm_p, scaled[1] / norm_p, scaled[2] / norm_p];

        // Build raw (un-normalised) gains: directional speakers are panned by the
        // two nearest directions; omni speakers get a constant baseline. A final
        // constant-power normalise scales the whole vector to unit power.
        let mut raw = Gains::zeroed(n);

        // Two nearest directional speakers to the object direction.
        let mut best = usize::MAX;
        let mut best_dot = f32::NEG_INFINITY;
        let mut second = usize::MAX;
        let mut second_dot = f32::NEG_INFINITY;
        for i in 0..n {
            if self.omni[i] {
                continue;
            }
            let d = dot(self.speaker_dirs[i], p);
            if d > best_dot {
                second = best;
                second_dot = best_dot;
                best = i;
                best_dot = d;
            } else if d > second_dot {
                second = i;
                second_dot = d;
            }
        }

        if best != usize::MAX {
            const COINCIDENT: f32 = 1.0 - 1e-6;
            if best_dot >= COINCIDENT || second == usize::MAX {
                // Object aligned with a speaker, or only one directional speaker:
                // 100% on it (before omni mix + normalise).
                raw.set(best, 1.0);
            } else {
                let (gb, gs) = Self::pair_gains(
                    self.speaker_dirs[best],
                    self.speaker_dirs[second],
                    best_dot,
                    second_dot,
                );
                raw.set(best, gb);
                raw.set(second, gs);
            }
        }

        // Omni baseline: speakers at the listener always get a constant share.
        for i in 0..n {
            if self.omni[i] {
                raw.set(i, OMNI_WEIGHT);
            }
        }

        // Constant-power normalise the assembled vector.
        let mut sumsq = 0.0;
        for i in 0..n {
            let v = raw[i];
            sumsq += v * v;
        }
        if sumsq <= 1e-12 {
            // No directional and no omni speakers contributed: split equally.
            return RenderResponse {
                gains: Self::equal_power(n),
            };
        }
        let inv = 1.0 / sumsq.sqrt();
        for i in 0..n {
            let v = raw[i] * inv;
            raw.set(i, v);
        }
        RenderResponse { gains: raw }
    }

    pub fn save_to_file(
        &self,
        path: &std::path::Path,
        speaker_layout: &SpeakerLayout,
    ) -> Result<()> {
        let _ = (path, speaker_layout);
        // Geometry-only, like VbapBackend: precomputed tables are exported from the
        // evaluation layer, not from the backend.
        anyhow::bail!(
            "degenerate-VBAP backend is geometry-only; export the precomputed table via the evaluator"
        )
    }
}

impl GainModel for DegenerateVbapBackend {
    // Degenerate VBAP — reuses the VBAP identity (no separate UI backend).
    fn backend_id(&self) -> &'static str {
        "vbap"
    }

    fn backend_label(&self) -> &'static str {
        "VBAP"
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supports_realtime: true,
            supports_precomputed_polar: true,
            supports_precomputed_cartesian: true,
            supports_position_interpolation: true,
            supports_distance_model: true,
            supports_spread: false,
            supports_spread_from_distance: false,
            supports_event_size: false,
            supports_distance_diffuse: true,
            supports_table_export: true,
        }
    }

    fn speaker_count(&self) -> usize {
        DegenerateVbapBackend::speaker_count(self)
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        DegenerateVbapBackend::compute_gains(self, req)
    }

    fn save_to_file(&self, path: &std::path::Path, speaker_layout: &SpeakerLayout) -> Result<()> {
        DegenerateVbapBackend::save_to_file(self, path, speaker_layout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Neutral request: identity room ratios, so the scaled position equals the
    /// ADM position (the backend then only depends on direction).
    fn req(pos: [f64; 3]) -> RenderRequest {
        RenderRequest {
            adm_position: pos,
            event_size: [0.0; 3],
            room_ratio: [1.0, 1.0, 1.0],
            room_ratio_rear: 1.0,
            room_ratio_lower: 1.0,
            room_ratio_center_blend: 0.5,
            use_distance_diffuse: false,
            diffuse_mirror_axes: crate::spatial_vbap::MirrorAxes::default(),
            distance_diffuse_threshold: 1.0,
            distance_diffuse_curve: 1.0,
            distance_model: crate::spatial_vbap::DistanceModel::default(),
        }
    }

    fn adm(az: f32, el: f32, dist: f32) -> [f64; 3] {
        let (x, y, z) = spherical_to_adm(az, el, dist);
        [x as f64, y as f64, z as f64]
    }

    #[test]
    fn single_speaker_is_unity_everywhere() {
        let b = DegenerateVbapBackend::new(vec![[30.0, 0.0]]);
        for pos in [adm(0.0, 0.0, 1.0), adm(120.0, 45.0, 3.0), [0.0, 0.0, 0.0]] {
            let g = b.compute_gains(&req(pos)).gains;
            assert_eq!(g.len(), 1);
            assert!((g[0] - 1.0).abs() < 1e-6, "got {:?}", &g[..]);
        }
    }

    #[test]
    fn object_on_speaker_direction_is_full_on_that_speaker() {
        let b = DegenerateVbapBackend::new(vec![[-30.0, 0.0], [30.0, 0.0]]);
        let g0 = b.compute_gains(&req(adm(-30.0, 0.0, 1.0))).gains;
        assert!(
            (g0[0] - 1.0).abs() < 1e-6 && g0[1].abs() < 1e-6,
            "{:?}",
            &g0[..]
        );
        let g1 = b.compute_gains(&req(adm(30.0, 0.0, 1.0))).gains;
        assert!(
            g1[0].abs() < 1e-6 && (g1[1] - 1.0).abs() < 1e-6,
            "{:?}",
            &g1[..]
        );
    }

    #[test]
    fn midpoint_is_balanced_constant_power() {
        let b = DegenerateVbapBackend::new(vec![[-30.0, 0.0], [30.0, 0.0]]);
        let g = b.compute_gains(&req(adm(0.0, 0.0, 1.0))).gains;
        assert!((g[0] - g[1]).abs() < 1e-5, "{:?}", &g[..]);
        assert!((g[0] - FRAC_1_SQRT_2).abs() < 1e-4, "{:?}", &g[..]);
    }

    #[test]
    fn constant_power_across_the_arc() {
        let b = DegenerateVbapBackend::new(vec![[-30.0, 0.0], [30.0, 0.0]]);
        for az in [-30.0, -15.0, -5.0, 0.0, 7.0, 20.0, 30.0] {
            let g = b.compute_gains(&req(adm(az, 0.0, 1.0))).gains;
            let power = g[0] * g[0] + g[1] * g[1];
            assert!((power - 1.0).abs() < 1e-4, "az={az} power={power}");
        }
    }

    #[test]
    fn out_of_arc_collapses_to_nearest() {
        let b = DegenerateVbapBackend::new(vec![[-30.0, 0.0], [30.0, 0.0]]);
        // Far to the left, well outside [-30, 30]: only the left speaker survives.
        let g = b.compute_gains(&req(adm(-90.0, 0.0, 1.0))).gains;
        assert!(
            (g[0] - 1.0).abs() < 1e-6 && g[1].abs() < 1e-6,
            "{:?}",
            &g[..]
        );
    }

    #[test]
    fn distance_is_ignored() {
        let b = DegenerateVbapBackend::new(vec![[-30.0, 0.0], [30.0, 0.0]]);
        // Keep both probes inside the unit box (the room depth warp clamps |y|≤1,
        // like VBAP): within it the gains depend only on direction, not distance.
        let near = b.compute_gains(&req(adm(12.0, 10.0, 0.4))).gains;
        let far = b.compute_gains(&req(adm(12.0, 10.0, 0.8))).gains;
        assert!((near[0] - far[0]).abs() < 1e-4 && (near[1] - far[1]).abs() < 1e-4);
    }

    #[test]
    fn antipodal_speakers_do_not_nan() {
        let b = DegenerateVbapBackend::new(vec![[0.0, 0.0], [180.0, 0.0]]);
        let g = b.compute_gains(&req(adm(90.0, 0.0, 1.0))).gains;
        assert!(g[0].is_finite() && g[1].is_finite(), "{:?}", &g[..]);
        let power = g[0] * g[0] + g[1] * g[1];
        assert!((power - 1.0).abs() < 1e-4, "power={power}");
    }

    #[test]
    fn three_collinear_speakers_pan_along_the_arc() {
        // Three coplanar speakers on the horizontal circle — the full panner can't
        // triangulate this, but the degenerate backend still pans between the two
        // nearest directions and stays constant-power.
        let b = DegenerateVbapBackend::new(vec![[-60.0, 0.0], [0.0, 0.0], [60.0, 0.0]]);
        for az in [-60.0, -30.0, 0.0, 30.0, 60.0] {
            let g = b.compute_gains(&req(adm(az, 0.0, 1.0))).gains;
            let power: f32 = (0..3).map(|i| g[i] * g[i]).sum();
            assert!((power - 1.0).abs() < 1e-4, "az={az} power={power}");
        }
        // Aligned with the middle speaker: full on it.
        let g = b.compute_gains(&req(adm(0.0, 0.0, 1.0))).gains;
        assert!(
            (g[1] - 1.0).abs() < 1e-4 && g[0].abs() < 1e-4 && g[2].abs() < 1e-4,
            "{:?}",
            &g[..]
        );
    }

    #[test]
    fn omni_speaker_always_gets_a_share() {
        // Speaker 0 is at the listener (omni, e.g. a spatialised LFE); 1 and 2 are
        // real surrounds. The omni speaker keeps a baseline share regardless of
        // where the object is, while 1/2 pan by direction.
        let b = DegenerateVbapBackend::with_omni(
            vec![[0.0, 0.0], [-90.0, 0.0], [90.0, 0.0]],
            vec![true, false, false],
        );
        for az in [-90.0, -45.0, 0.0, 45.0, 90.0] {
            let g = b.compute_gains(&req(adm(az, 0.0, 1.0))).gains;
            assert!(g[0] > 0.1, "omni share too small at az={az}: {:?}", &g[..]);
            let power: f32 = (0..3).map(|i| g[i] * g[i]).sum();
            assert!((power - 1.0).abs() < 1e-4, "az={az} power={power}");
        }
    }
}
