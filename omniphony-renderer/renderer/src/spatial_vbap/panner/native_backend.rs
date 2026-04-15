//! Pure-Rust VBAP backend — drop-in replacement for `saf_backend.rs`.
//!
//! Used when the `saf_vbap` feature is disabled (no C FFI, no external library).

use super::Gains;
use super::gain_source::VbapGainSource;
use crate::spatial_vbap::vbap_native::{find_ls_triplets, invert_ls_mtx_3d, vbap3d};

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
    pub(crate) n_speakers: usize,
    pub(crate) n_faces: usize,
    #[allow(dead_code)]
    u_spkr: Vec<[f32; 3]>,
    ls_groups: Vec<[usize; 3]>,
    layout_inv_mtx: Vec<[f32; 9]>,
}

impl NativeVbapLayout {
    /// Build a layout from speaker directions (azimuth, elevation in degrees).
    ///
    /// Calls `find_ls_triplets` → `invert_ls_mtx_3d` to prepare the data
    /// needed for per-direction VBAP gain computation.
    pub fn from_speaker_dirs(speaker_dirs_deg: &[[f32; 2]]) -> Result<Self, String> {
        let (u_spkr, ls_groups) = find_ls_triplets(speaker_dirs_deg, true)
            .ok_or_else(|| "find_ls_triplets failed".to_string())?;

        if ls_groups.is_empty() {
            return Err("No valid loudspeaker triangles found".to_string());
        }

        let layout_inv_mtx = invert_ls_mtx_3d(&u_spkr, &ls_groups);
        let n_faces = ls_groups.len();
        let n_speakers = speaker_dirs_deg.len();

        Ok(Self {
            n_speakers,
            n_faces,
            u_spkr,
            ls_groups,
            layout_inv_mtx,
        })
    }

    /// Compute VBAP gains for a single source direction and spread.
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
            self.n_speakers,
            &self.ls_groups,
            spread_deg,
            &self.layout_inv_mtx,
        );

        Ok(Gains::from_slice(&gain_vec))
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
