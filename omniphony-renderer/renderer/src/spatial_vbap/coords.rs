//! ADM coordinate conversions.
//!
//! The implementations live in `omniphony-geometry`, which the Studio backend
//! shares, so both ends of the OSC link agree on what an azimuth means.
//!
//! ADM:
//! - X: left(-) -> right(+)
//! - Y: back(-) -> front(+)
//! - Z: floor(-) -> ceiling(+)

/// Convert ADM coordinates to spherical angles + distance.
pub use omniphony_geometry::f32::to_spherical as adm_to_spherical;

/// Convert spherical angles + distance to ADM coordinates.
pub use omniphony_geometry::f32::from_spherical as spherical_to_adm;
