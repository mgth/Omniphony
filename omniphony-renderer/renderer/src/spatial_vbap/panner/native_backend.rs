//! Pure-Rust VBAP backend — drop-in replacement for `saf_backend.rs`.
//!
//! Used when the `saf_vbap` feature is disabled (no C FFI, no external library).

use super::Gains;
use super::gain_source::VbapGainSource;
use crate::spatial_vbap::vbap_native::{find_ls_triplets, invert_ls_mtx_3d, vbap3d};

/// Elevation threshold for dummy speaker injection.
/// If all speakers are above/below this limit, a virtual speaker is added at ±90°
/// so that 3D convex hull triangulation succeeds for near-horizontal layouts.
const ADD_DUMMY_LIMIT: f32 = 60.0;

/// Maximum spread in degrees accepted by `vbap3d`.
/// Matches `SpartaVbapLayout::NORMALIZED_SPREAD_MAX_DEG` for parity.
const NORMALIZED_SPREAD_MAX_DEG: f32 = 180.0;

#[inline]
fn normalized_spread_to_degrees(spread: f32) -> f32 {
    spread.clamp(0.0, 1.0) * NORMALIZED_SPREAD_MAX_DEG
}

/// Pure-Rust equivalent of `SpartaVbapLayout`.
///
/// Owns the triangulation and inverse speaker matrices produced by
/// `find_ls_triplets` + `invert_ls_mtx_3d`. Implements [`VbapGainSource`]
/// so it can be used interchangeably with the SAF FFI backend.
pub(crate) struct NativeVbapLayout {
    /// Number of *real* (non-dummy) speakers — the size of the returned `Gains`.
    pub(crate) n_speakers: usize,
    pub(crate) n_faces: usize,
    /// Total speaker count used for triangulation (real + dummy virtual speakers).
    n_eff: usize,
    #[allow(dead_code)]
    u_spkr: Vec<[f32; 3]>,
    ls_groups: Vec<[usize; 3]>,
    layout_inv_mtx: Vec<[f32; 9]>,
}

impl NativeVbapLayout {
    /// Build a layout from speaker directions (azimuth, elevation in degrees).
    ///
    /// When all speakers lie within ±ADD_DUMMY_LIMIT degrees of the equator,
    /// virtual speakers at ±90° elevation are injected so that the 3D convex
    /// hull triangulation succeeds. Dummy gains are stripped before returning.
    pub fn from_speaker_dirs(speaker_dirs_deg: &[[f32; 2]]) -> Result<Self, String> {
        let n_real = speaker_dirs_deg.len();

        let need_dummy_neg = speaker_dirs_deg.iter().all(|d| d[1] > -ADD_DUMMY_LIMIT);
        let need_dummy_pos = speaker_dirs_deg.iter().all(|d| d[1] < ADD_DUMMY_LIMIT);

        let effective_dirs: Vec<[f32; 2]>;
        if need_dummy_neg || need_dummy_pos {
            let mut dirs = speaker_dirs_deg.to_vec();
            if need_dummy_neg {
                dirs.push([0.0, -90.0]);
            }
            if need_dummy_pos {
                dirs.push([0.0, 90.0]);
            }
            effective_dirs = dirs;
        } else {
            effective_dirs = speaker_dirs_deg.to_vec();
        }

        let n_eff = effective_dirs.len();

        let (u_spkr, ls_groups) = find_ls_triplets(&effective_dirs, true)
            .ok_or_else(|| "find_ls_triplets failed".to_string())?;

        if ls_groups.is_empty() {
            return Err("No valid loudspeaker triangles found".to_string());
        }

        let layout_inv_mtx = invert_ls_mtx_3d(&u_spkr, &ls_groups);
        let n_faces = ls_groups.len();

        Ok(Self {
            n_speakers: n_real,
            n_faces,
            n_eff,
            u_spkr,
            ls_groups,
            layout_inv_mtx,
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
        );

        // Strip dummy speaker columns — keep only the first n_speakers entries.
        Ok(Gains::from_slice(&gain_vec[..self.n_speakers]))
    }
}

impl VbapGainSource for NativeVbapLayout {
    fn compute_gains(
        &self,
        azimuth_deg: f32,
        elevation_deg: f32,
        spread: f32,
    ) -> Result<Gains, String> {
        self.vbap_gains(azimuth_deg, elevation_deg, spread)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coplanar_speakers_position_aware() {
        // 4 speakers all at el=0 — previously would fail triangulation.
        let dirs = [
            [-90.0_f32, 0.0], // Left
            [90.0, 0.0],      // Right
            [0.0, 0.0],       // Front
            [180.0, 0.0],     // Rear
        ];
        let layout =
            NativeVbapLayout::from_speaker_dirs(&dirs).expect("should succeed with dummy speakers");

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
}
