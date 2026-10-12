//! Per-event object size → scalar spread reduction policies.
//!
//! ETSI TS 103 420 §5.2.2 defines `object_size` as an anisotropic triplet
//! `(width, depth, height)` ∈ [0, 1]³. The VBAP backend operates on a single
//! `spread ∈ [0, 1]` value, so we need a reduction policy. Three modes are
//! exposed and chosen at runtime via Studio.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SizeToSpreadMode {
    /// `max(w, d, h)` — preserves the dominant extent. Default; most
    /// conservative, never under-estimates the radiation pattern.
    #[default]
    Max,
    /// `(w + d + h) / 3` — arithmetic mean across the three axes.
    Mean,
    /// Apparent extent perpendicular to the listener→object direction.
    /// Approximates how big the object "looks" from the listener.
    ProjectionPerpendicular,
}

impl SizeToSpreadMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Max => "max",
            Self::Mean => "mean",
            Self::ProjectionPerpendicular => "projection_perpendicular",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "max" => Some(Self::Max),
            "mean" => Some(Self::Mean),
            "projection_perpendicular" => Some(Self::ProjectionPerpendicular),
            _ => None,
        }
    }
}

/// Reduce a `(width, depth, height)` triplet ∈ [0, 1]³ to a scalar spread in
/// [0, 1], according to the selected policy.
///
/// `pos` is the cartesian position of the object (listener at origin). Only
/// used by `ProjectionPerpendicular`; ignored otherwise.
#[inline]
pub fn reduce_size_to_spread(size: [f32; 3], pos: [f32; 3], mode: SizeToSpreadMode) -> f32 {
    let [w, d, h] = size;
    let v = match mode {
        SizeToSpreadMode::Max => w.max(d).max(h),
        SizeToSpreadMode::Mean => (w + d + h) / 3.0,
        SizeToSpreadMode::ProjectionPerpendicular => {
            let plen2 = pos[0] * pos[0] + pos[1] * pos[1] + pos[2] * pos[2];
            if plen2 < 1e-12 {
                // Object at the listener: no preferred direction → fall back to Max.
                w.max(d).max(h)
            } else {
                let inv = plen2.sqrt().recip();
                let dx = pos[0] * inv;
                let dy = pos[1] * inv;
                let dz = pos[2] * inv;
                // Component of each size axis perpendicular to the listener
                // direction: `size_i · sqrt(1 - dir_i²)`. Combine via L2 and
                // normalise by √3 (worst case `(1,1,1)` from any direction has
                // perpendicular norm √3 · sin θ ≤ √3).
                let px = w * (1.0 - dx * dx).max(0.0).sqrt();
                let py = d * (1.0 - dy * dy).max(0.0).sqrt();
                let pz = h * (1.0 - dz * dz).max(0.0).sqrt();
                ((px * px + py * py + pz * pz).sqrt() / 3.0_f32.sqrt()).min(1.0)
            }
        }
    };
    v.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One case per mode, and the projection's two limits and fallback.
    #[test]
    fn each_mode_reduces_size_as_documented() {
        use SizeToSpreadMode::{Max, Mean, ProjectionPerpendicular as Projection};
        let cases = [
            (
                "max picks the dominant axis",
                [0.5, 0.1, 0.1],
                [1.0, 0.0, 0.0],
                Max,
                0.5,
            ),
            (
                "mean averages all axes",
                [0.6, 0.3, 0.0],
                [1.0, 0.0, 0.0],
                Mean,
                0.3,
            ),
            // Object on +X, size only along X: no perceived width.
            (
                "projection collapses along the axis",
                [1.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                Projection,
                0.0,
            ),
            // Object on +X, size on Y and Z: sqrt(0² + 1² + 1²) / sqrt(3).
            (
                "projection keeps the perpendicular extent",
                [0.0, 1.0, 1.0],
                [1.0, 0.0, 0.0],
                Projection,
                (2.0_f32 / 3.0).sqrt(),
            ),
            (
                "projection falls back to max at the origin",
                [0.4, 0.7, 0.2],
                [0.0, 0.0, 0.0],
                Projection,
                0.7,
            ),
        ];
        for (what, size, pos, mode, want) in cases {
            let got = reduce_size_to_spread(size, pos, mode);
            assert!(
                (got - want).abs() < 1e-6,
                "{what}: expected {want}, got {got}"
            );
        }
    }

    #[test]
    fn from_str_roundtrips() {
        for m in [
            SizeToSpreadMode::Max,
            SizeToSpreadMode::Mean,
            SizeToSpreadMode::ProjectionPerpendicular,
        ] {
            assert_eq!(SizeToSpreadMode::from_str(m.as_str()), Some(m));
        }
        assert_eq!(SizeToSpreadMode::from_str("unknown"), None);
    }

    #[test]
    fn zero_size_yields_zero_spread() {
        for m in [
            SizeToSpreadMode::Max,
            SizeToSpreadMode::Mean,
            SizeToSpreadMode::ProjectionPerpendicular,
        ] {
            let got = reduce_size_to_spread([0.0; 3], [1.0, 0.0, 0.0], m);
            assert!(got.abs() < 1e-6, "{m:?}: expected 0, got {got}");
        }
    }
}
