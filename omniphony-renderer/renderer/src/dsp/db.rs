//! Decibel ↔ linear amplitude conversions.

/// Linear amplitude of `db` decibels: `10^(db/20)`. No floor: callers with a
/// mute sentinel (see `spatial_renderer::gain_db_to_linear`) test it first.
#[inline]
pub fn db_to_linear(db: f32) -> f32 {
    10.0_f32.powf(db / 20.0)
}

/// Decibels of a linear amplitude: `20·log10(v)`. No floor: `0` gives `−∞`,
/// callers that display it clamp.
#[inline]
pub fn linear_to_db(v: f32) -> f32 {
    20.0 * v.log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_anchors() {
        assert_eq!(db_to_linear(0.0), 1.0);
        assert!((db_to_linear(-20.0) - 0.1).abs() < 1e-7);
        assert_eq!(linear_to_db(1.0), 0.0);
        assert!((linear_to_db(db_to_linear(-6.0)) + 6.0).abs() < 1e-5);
    }
}
