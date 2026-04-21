//! SAF FFI backend for VBAP gain computation.
//!
//! Wraps the Spatial Audio Framework's `vbap3D` triangulation in a safe Rust
//! struct ([`SpartaVbapLayout`]) and implements [`VbapGainSource`] so that
//! table-generation code can use it interchangeably with the table-lookup
//! backend.

use super::Gains;
use super::gain_source::VbapGainSource;
use super::saf_ffi;
use std::ffi::c_int;

/// Elevation threshold for dummy speaker injection — mirrors the native backend.
/// If all speakers are above/below this limit, virtual speakers are added at ±90°
/// so that SAF's `findLsTriplets` succeeds for near-horizontal layouts.
const ADD_DUMMY_LIMIT: f32 = 60.0;

/// Safe wrapper around SAF's speaker triangulation and VBAP gain matrices.
///
/// Owns the C-allocated `ls_groups` and `layout_inv_mtx` pointers and frees
/// them on drop.
pub(crate) struct SpartaVbapLayout {
    /// Number of *real* (non-dummy) speakers — size of the returned `Gains`.
    pub(crate) n_speakers: usize,
    pub(crate) n_faces: c_int,
    /// Total speaker count used for triangulation (real + dummy virtual speakers).
    n_eff: usize,
    ls_groups: *mut c_int,
    layout_inv_mtx: *mut f32,
}

impl SpartaVbapLayout {
    /// Maximum spread in degrees that SAF's `vbap3D` accepts.
    /// The public API uses normalised [0, 1]; this constant maps 1.0 → 180°.
    const NORMALIZED_SPREAD_MAX_DEG: f32 = 180.0;

    #[inline]
    fn normalized_spread_to_degrees(spread: f32) -> f32 {
        spread.clamp(0.0, 1.0) * Self::NORMALIZED_SPREAD_MAX_DEG
    }

    /// Build a layout from speaker directions (azimuth, elevation in degrees).
    ///
    /// When all speakers lie within ±ADD_DUMMY_LIMIT degrees of the equator,
    /// virtual speakers at ±90° are injected before calling SAF's
    /// `findLsTriplets`, mirroring what `generateVBAPgainTable3D(enableDummies=1)`
    /// does internally. Dummy gains are stripped before returning.
    pub fn from_speaker_dirs(speaker_dirs_deg: &[[f32; 2]]) -> Result<Self, String> {
        let n_real = speaker_dirs_deg.len();

        let need_dummy_neg = speaker_dirs_deg.iter().all(|d| d[1] > -ADD_DUMMY_LIMIT);
        let need_dummy_pos = speaker_dirs_deg.iter().all(|d| d[1] < ADD_DUMMY_LIMIT);

        let effective: Vec<[f32; 2]> = if need_dummy_neg || need_dummy_pos {
            let mut dirs = speaker_dirs_deg.to_vec();
            if need_dummy_neg { dirs.push([0.0, -90.0]); }
            if need_dummy_pos { dirs.push([0.0,  90.0]); }
            dirs
        } else {
            speaker_dirs_deg.to_vec()
        };

        let n_eff = effective.len();
        let mut ls_dirs: Vec<f32> = Vec::with_capacity(n_eff * 2);
        for &[az, el] in &effective {
            ls_dirs.push(az);
            ls_dirs.push(el);
        }

        let mut u_spkr: *mut f32 = std::ptr::null_mut();
        let mut num_vert: c_int = 0;
        let mut ls_groups: *mut c_int = std::ptr::null_mut();
        let mut n_faces: c_int = 0;

        unsafe {
            saf_ffi::findLsTriplets(
                ls_dirs.as_mut_ptr(),
                n_eff as c_int,
                1,
                &mut u_spkr,
                &mut num_vert,
                &mut ls_groups,
                &mut n_faces,
            );
        }

        if num_vert <= 0 || n_faces <= 0 || u_spkr.is_null() || ls_groups.is_null() {
            if !u_spkr.is_null() {
                unsafe { libc::free(u_spkr as *mut libc::c_void) };
            }
            if !ls_groups.is_null() {
                unsafe { libc::free(ls_groups as *mut libc::c_void) };
            }
            return Err("findLsTriplets failed".to_string());
        }

        let mut layout_inv_mtx: *mut f32 = std::ptr::null_mut();
        unsafe {
            saf_ffi::invertLsMtx3D(u_spkr, ls_groups, n_faces, &mut layout_inv_mtx);
            libc::free(u_spkr as *mut libc::c_void);
        }

        if layout_inv_mtx.is_null() {
            unsafe { libc::free(ls_groups as *mut libc::c_void) };
            return Err("invertLsMtx3D failed".to_string());
        }

        Ok(Self {
            n_speakers: n_real,
            n_faces,
            n_eff,
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
        let mut src_dirs = [azimuth_deg, elevation_deg];
        let spread_deg = Self::normalized_spread_to_degrees(spread);
        let mut gain_mtx: *mut f32 = std::ptr::null_mut();
        unsafe {
            saf_ffi::vbap3D(
                src_dirs.as_mut_ptr(),
                1,
                self.n_eff as c_int,
                self.ls_groups,
                self.n_faces,
                spread_deg,
                self.layout_inv_mtx,
                &mut gain_mtx,
            );
        }

        if gain_mtx.is_null() {
            return Err("vbap3D failed".to_string());
        }

        // SAF returns n_eff gains — keep only the first n_speakers (real speakers).
        let all_gains = unsafe { std::slice::from_raw_parts(gain_mtx, self.n_eff) };
        let out = Gains::from_slice(&all_gains[..self.n_speakers]);
        unsafe { libc::free(gain_mtx as *mut libc::c_void) };
        Ok(out)
    }
}

impl Drop for SpartaVbapLayout {
    fn drop(&mut self) {
        if !self.ls_groups.is_null() {
            unsafe { libc::free(self.ls_groups as *mut libc::c_void) };
        }
        if !self.layout_inv_mtx.is_null() {
            unsafe { libc::free(self.layout_inv_mtx as *mut libc::c_void) };
        }
    }
}

impl VbapGainSource for SpartaVbapLayout {
    fn compute_gains(
        &self,
        azimuth_deg: f32,
        elevation_deg: f32,
        spread: f32,
    ) -> Result<Gains, String> {
        self.vbap_gains(azimuth_deg, elevation_deg, spread)
    }
}
