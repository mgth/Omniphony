//! Abstraction for VBAP gain computation backends.
//!
//! The [`VbapGainSource`] trait lets table-generation code work identically
//! whether gains come from SAF FFI (`saf_vbap` feature) or from pre-computed
//! polar spread tables.

use super::Gains;

/// Trait abstracting how raw VBAP gains are computed for a given direction.
///
/// Two implementations exist:
///
/// - **SAF backend** (behind `saf_vbap` feature): calls SAF FFI `vbap3D`
///   for exact triangulation-based gains.
/// - [`TableGainSource`]: interpolates from pre-computed polar spread tables,
///   used when the `saf_vbap` feature is disabled.
///
/// This abstraction lets table-generation code (`build_cartesian_cache`,
/// `build_*_effect_cache`, etc.) work identically regardless of the backend,
/// eliminating duplicated `#[cfg]` branches.
pub(crate) trait VbapGainSource {
    /// Compute VBAP gains for a source at the given direction and spread.
    ///
    /// * `azimuth_deg`   — azimuth in degrees, −180 … +180
    /// * `elevation_deg` — elevation in degrees, −90 … +90
    /// * `spread`        — normalised spread coefficient, 0.0 … 1.0
    fn compute_gains(
        &self,
        azimuth_deg: f32,
        elevation_deg: f32,
        spread: f32,
    ) -> Result<Gains, String>;
}

/// Gain source backed by pre-computed polar spread tables (no SAF FFI).
///
/// Delegates to [`VbapPanner::get_gains_with_spread`], which performs bilinear
/// interpolation across azimuth, elevation, and spread dimensions.
pub(crate) struct TableGainSource<'a> {
    panner: &'a super::VbapPanner,
}

impl<'a> TableGainSource<'a> {
    pub fn new(panner: &'a super::VbapPanner) -> Self {
        Self { panner }
    }
}

impl VbapGainSource for TableGainSource<'_> {
    fn compute_gains(
        &self,
        azimuth_deg: f32,
        elevation_deg: f32,
        spread: f32,
    ) -> Result<Gains, String> {
        Ok(self
            .panner
            .get_gains_with_spread(azimuth_deg, elevation_deg, spread))
    }
}
