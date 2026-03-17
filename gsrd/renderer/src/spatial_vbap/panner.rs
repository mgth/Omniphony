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
//! ```no_run
//! use gsrd::spatial_vbap::VbapPanner;
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
//! let panner = VbapPanner::new(&speakers, 1, 1, 0.0)?;
//!
//! // Get gains for object at azimuth=30°, elevation=15°
//! let gains = panner.get_gains(30.0, 15.0);
//! ```

// SAF bindings - only available with the historical "saf_vbap" feature flag
use super::coords::{adm_to_spherical, spherical_to_adm};
use super::distance::{DistanceModel, calculate_distance_attenuation};
#[cfg(feature = "saf_vbap")]
use std::ffi::c_int;

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

/// Maximum number of speakers supported without heap allocation.
/// Covers all standard immersive audio layouts (up to 22.2).
pub const MAX_SPEAKERS: usize = 24;

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

#[derive(Clone)]
struct CartesianCache {
    x_size: usize,
    y_size: usize,
    // Positive-Z grid point count, including zero.
    z_size: usize,
    z_neg_size: usize,
    // One flattened XYZ gain table per spread table.
    // Layout per table: [z][y][x][speaker]
    tables: Vec<Vec<f32>>,
}

#[derive(Clone)]
struct PolarDistanceCache {
    d_size: usize,
    d_step: f32,
    d_max: f32,
    // Flattened: [d][el][az][speaker]
    table: Vec<f32>,
}

/// Stack-allocated gain vector, replacing `Vec<f32>` in the VBAP hot path.
///
/// Eliminates ~8-10 heap allocations per object per sample in the rendering loop.
/// Implements `Deref<Target=[f32]>` so callers can use `.iter()`, `.enumerate()`,
/// indexing, etc. transparently.
#[derive(Clone)]
pub struct Gains {
    data: [f32; MAX_SPEAKERS],
    len: usize,
}

impl Gains {
    /// Create a new zeroed Gains with the given length.
    #[inline]
    fn new(len: usize) -> Self {
        debug_assert!(
            len <= MAX_SPEAKERS,
            "speaker count {} exceeds MAX_SPEAKERS {}",
            len,
            MAX_SPEAKERS
        );
        Gains {
            data: [0.0; MAX_SPEAKERS],
            len,
        }
    }

    /// Public constructor: zeroed Gains with the given length.
    #[inline]
    pub fn zeroed(len: usize) -> Self {
        Self::new(len)
    }

    /// Write a single gain value by index (no bounds-check in release builds).
    #[inline]
    pub fn set(&mut self, i: usize, v: f32) {
        debug_assert!(i < self.len);
        self.data[i] = v;
    }

    /// Create Gains by copying from a slice.
    #[inline]
    fn from_slice(src: &[f32]) -> Self {
        debug_assert!(src.len() <= MAX_SPEAKERS);
        let mut g = Gains::new(src.len());
        g.data[..src.len()].copy_from_slice(src);
        g
    }
}

impl std::ops::Deref for Gains {
    type Target = [f32];

    #[inline]
    fn deref(&self) -> &[f32] {
        &self.data[..self.len]
    }
}

impl std::ops::DerefMut for Gains {
    #[inline]
    fn deref_mut(&mut self) -> &mut [f32] {
        &mut self.data[..self.len]
    }
}

/// Single spread table entry
#[derive(Clone)]
struct SpreadTable {
    /// Spread value for this table (0.0 - 1.0)
    spread: f32,

    /// Pre-computed gain table for this spread
    /// Dimensions: [azimuth_index][elevation_index][speaker_index]
    gtable: Vec<f32>,
}

/// VBAP panner with pre-computed gain tables
pub struct VbapPanner {
    /// Multiple pre-computed tables for different spread values
    /// If empty, uses legacy single-table mode (spread_tables has exactly one entry)
    spread_tables: Vec<SpreadTable>,

    /// Spread resolution (step between tables), or 0.0 for single-table mode
    spread_resolution: f32,

    /// Total number of entries in gain table (per spread table)
    n_gtable: usize,

    /// Number of speaker triangles found
    n_triangles: usize,

    /// Number of speakers in the layout
    n_speakers: usize,

    /// Azimuth resolution in degrees
    az_res_deg: i32,

    /// Elevation resolution in degrees
    el_res_deg: i32,

    /// Number of azimuth grid points (360 / az_res_deg)
    n_az: usize,

    /// Number of elevation grid points over active range
    /// ([-90,+90] when `allow_negative_z`, otherwise [0,+90]).
    n_el: usize,
    table_mode: VbapTableMode,
    allow_negative_z: bool,
    cartesian_cache: Option<CartesianCache>,
    polar_distance_cache: Option<PolarDistanceCache>,
    precomputed_effects: bool,
    #[cfg(feature = "saf_vbap")]
    speaker_dirs_deg: Option<Vec<[f32; 2]>>,
}

mod io;
mod runtime;

#[cfg(test)]
mod tests;
