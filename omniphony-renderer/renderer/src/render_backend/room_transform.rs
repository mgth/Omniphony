//! Shared mapping from ADM object coordinates into the room-scaled "effect
//! space" that every gain model pans in (and that distance attenuation measures
//! distance from). Previously each backend carried its own private copy of this
//! transform.

/// Non-linear depth warp applied to the Y (front/back) axis using the front,
/// rear and centre-blend room ratios.
#[inline]
pub(crate) fn map_depth_with_room_ratios(
    depth: f32,
    front_ratio: f32,
    rear_ratio: f32,
    center_blend: f32,
) -> f32 {
    let d = depth.clamp(-1.0, 1.0);
    let blend = center_blend.clamp(0.0, 1.0);
    let center_ratio = rear_ratio + (front_ratio - rear_ratio) * blend;
    if d >= 0.0 {
        let t = d;
        let a = center_ratio - front_ratio;
        let b = 2.0 * (front_ratio - center_ratio);
        a * t * t * t + b * t * t + center_ratio * t
    } else {
        let t = -d;
        let a = center_ratio - rear_ratio;
        let b = 2.0 * (rear_ratio - center_ratio);
        -(a * t * t * t + b * t * t + center_ratio * t)
    }
}

/// Scale an ADM position into room-relative effect space: the coordinates the
/// gain models pan in and from which distance attenuation measures distance.
#[inline]
pub(crate) fn room_scaled_position(
    position: [f32; 3],
    room_ratio: [f32; 3],
    room_ratio_rear: f32,
    room_ratio_lower: f32,
    room_ratio_center_blend: f32,
) -> [f32; 3] {
    [
        position[0] * room_ratio[0],
        map_depth_with_room_ratios(
            position[1],
            room_ratio[1],
            room_ratio_rear,
            room_ratio_center_blend,
        ),
        if position[2] >= 0.0 {
            position[2] * room_ratio[2]
        } else {
            position[2] * room_ratio_lower
        },
    ]
}
