//! SAF `saf_vbap` (Vector-Based Amplitude Panning) wrapper
//!
//! This module provides a safe Rust wrapper around the Spatial_Audio_Framework
//! (SAF) VBAP implementation. VBAP is the academic standard for spatial audio
//! panning (Pulkki 1997).
//!
//! Features:
//! - Pre-computed gain tables for O(1) lookup performance
//! - Delaunay triangulation for any speaker layout
//! - Frequency-dependent processing support (133 bands)
//! - Room adaptation via DTT parameter
//!
//! # Example
//!
//! ```ignore
//! use omniphony_renderer::spatial_vbap::VbapPanner;
//!
//! // Define 7.1.4 speaker layout (11 speakers)
//! let speakers = vec![
//!     [0.0, 0.0],      // Front Center
//!     [-30.0, 0.0],    // Front Left
//!     [30.0, 0.0],     // Front Right
//!     [-110.0, 0.0],   // Side Left
//!     [110.0, 0.0],    // Side Right
//!     [-145.0, 0.0],   // Rear Left
//!     [145.0, 0.0],    // Rear Right
//!     [0.0, 0.0],      // LFE (same as center, will be filtered)
//!     [-45.0, 45.0],   // Top Front Left
//!     [45.0, 45.0],    // Top Front Right
//!     [-135.0, 45.0],  // Top Rear Left
//!     [135.0, 45.0],   // Top Rear Right
//! ];
//!
//! // Create panner with 1° resolution and no spreading
//! let panner = VbapPanner::new(&speakers, 1, 1, 0.0, Default::default())?;
//!
//! // Get gains for object at azimuth=30°, elevation=15°
//! let gains = panner.get_gains(30.0, 15.0);
//! ```

// SAF bindings - only available with the historical "saf_vbap" feature flag
use super::coords::adm_to_spherical;

// Include the generated FFI bindings
// Note: For Rust 2024 edition, we need to manually mark extern blocks as unsafe
#[cfg(feature = "saf_vbap")]
#[allow(non_upper_case_globals)]
#[allow(non_camel_case_types)]
#[allow(non_snake_case)]
#[allow(dead_code)]
#[allow(unsafe_code)]
mod saf_ffi {
    // The generated bindings have extern blocks that need to be unsafe in Rust 2024
    // We'll manually wrap them here
    use core::ffi::c_int;

    unsafe extern "C" {
        pub fn generateVBAPgainTable3D(
            ls_dirs_deg: *mut f32,
            L: c_int,
            az_res_deg: c_int,
            el_res_deg: c_int,
            omitLargeTriangles: c_int,
            enableDummies: c_int,
            spread: f32,
            gtable: *mut *mut f32,
            N_gtable: *mut c_int,
            nTriangles: *mut c_int,
        );

        pub fn findLsTriplets(
            ls_dirs_deg: *mut f32,
            ls_num: c_int,
            omitLargeTriangles: c_int,
            U_spkr: *mut *mut f32,
            numVert: *mut c_int,
            ls_groups: *mut *mut c_int,
            nFaces: *mut c_int,
        );

        pub fn invertLsMtx3D(
            U_spkr: *mut f32,
            ls_groups: *mut c_int,
            nFaces: c_int,
            layoutInvMtx: *mut *mut f32,
        );

        pub fn vbap3D(
            src_dirs: *mut f32,
            src_num: c_int,
            ls_num: c_int,
            ls_groups: *mut c_int,
            nFaces: c_int,
            spread: f32,
            layoutInvMtx: *mut f32,
            GainMtx: *mut *mut f32,
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VbapTableMode {
    Polar,
    Cartesian {
        x_size: usize,
        y_size: usize,
        // Positive-Z grid point count, including zero.
        z_size: usize,
        // Negative-Z interval count below zero. Zero means no negative-Z table region.
        z_neg_size: usize,
    },
}

/// VBAP panner — geometry only.
///
/// Holds the triangulated speaker layout and computes panning gains directly for
/// a given source position. It owns NO precomputed gain tables: all
/// precomputation (the polar/cartesian lookup tables) lives in the evaluation
/// layer (`render_backend::Sampled*Evaluator`), which samples this panner. The
/// panner just answers "gains at this position".
pub struct VbapPanner {
    /// Number of speaker triangles in the triangulation.
    n_triangles: usize,

    /// Number of virtual loudspeakers at the centre of coplanar hull faces
    /// (`vbap_native::Triangulation`); 0 under `saf_vbap`.
    n_virtual_centres: usize,

    /// Number of speakers in the layout.
    n_speakers: usize,

    /// Whether sources below the horizontal plane keep their negative Z (else Z
    /// is clamped to 0 before the spherical conversion).
    allow_negative_z: bool,

    /// Triangulated layout used for direct gain computation. The native backend
    /// is plain data (`Send + Sync`), so it is built once and stored. The SAF FFI
    /// layout owns raw pointers and is not `Sync`; under `saf_vbap` we keep the
    /// speaker directions and rebuild the layout per call instead.
    #[cfg(not(feature = "saf_vbap"))]
    source: native_backend::NativeVbapLayout,
    #[cfg(feature = "saf_vbap")]
    speaker_dirs_deg: Vec<[f32; 2]>,
}

/// Maximum spread in degrees the VBAP spreading accepts (SAF's `vbap3D` and
/// its native port alike). The public API is normalised to `[0, 1]`; this
/// maps 1.0 → 180°.
const NORMALIZED_SPREAD_MAX_DEG: f32 = 180.0;

/// Normalised spread `[0, 1]` → the degrees `vbap3D` takes.
#[inline]
fn normalized_spread_to_degrees(spread: f32) -> f32 {
    spread.clamp(0.0, 1.0) * NORMALIZED_SPREAD_MAX_DEG
}

#[cfg(not(feature = "saf_vbap"))]
pub(crate) mod native_backend;
#[cfg(feature = "saf_vbap")]
pub(crate) mod saf_backend;

mod runtime;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod native_validation;
