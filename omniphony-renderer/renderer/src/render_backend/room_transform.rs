//! Shared mapping from ADM object coordinates into the room-scaled "effect
//! space" that every gain model pans in (and that distance attenuation measures
//! distance from). Previously each backend carried its own private copy of this
//! transform.

/// Scale an ADM position into room-relative effect space: the coordinates the
/// gain models pan in and from which distance attenuation measures distance.
///
/// The depth warp it applies to the Y axis is
/// `omniphony_geometry::f32::map_depth`.
pub use omniphony_geometry::f32::room_scaled_position;
