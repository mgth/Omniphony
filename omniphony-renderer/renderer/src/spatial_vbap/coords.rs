/// Convert ADM coordinates to spherical angles + distance.
///
/// ADM:
/// - X: left(-) -> right(+)
/// - Y: back(-) -> front(+)
/// - Z: floor(-) -> ceiling(+)
pub fn adm_to_spherical(x: f32, y: f32, z: f32) -> (f32, f32, f32) {
    let distance = (x * x + y * y + z * z).sqrt();
    let r_horizontal = (x * x + y * y).sqrt();

    let azimuth_deg = x.atan2(y).to_degrees();

    // Keep exact vertical directions stable:
    // (x,y) ~= (0,0), z>0 => +90°, z<0 => -90°, origin => 0°.
    let elevation_deg = if r_horizontal < 1e-6 {
        if z > 0.0 {
            90.0
        } else if z < 0.0 {
            -90.0
        } else {
            0.0
        }
    } else {
        z.atan2(r_horizontal).to_degrees()
    };

    (azimuth_deg, elevation_deg, distance)
}

/// Convert spherical angles + distance to ADM coordinates.
pub fn spherical_to_adm(azimuth_deg: f32, elevation_deg: f32, distance: f32) -> (f32, f32, f32) {
    let az_rad = azimuth_deg.to_radians();
    let el_rad = elevation_deg.to_radians();

    let r_horizontal = distance * el_rad.cos();

    let x = r_horizontal * az_rad.sin();
    let y = r_horizontal * az_rad.cos();
    let z = distance * el_rad.sin();

    (x, y, z)
}
