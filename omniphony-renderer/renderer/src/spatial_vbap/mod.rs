//! SAF `saf_vbap` (Vector-Based Amplitude Panning) wrapper.
//!
//! Split by concern:
//! - `panner`: table generation, caches, lookup, file IO
//! - `coords`: ADM cartesian <-> spherical conversion helpers
//! - `distance`: distance model + attenuation helpers

mod coords;
mod distance;
mod panner;
pub(crate) mod convhull;
pub(crate) mod vbap_native;

pub use coords::{adm_to_spherical, spherical_to_adm};
pub use distance::{DistanceModel, calculate_distance_attenuation};
pub use panner::*;
