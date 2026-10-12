/// Distance attenuation model for spatial audio rendering.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DistanceModel {
    None,
    Linear,
    Quadratic,
    InverseSquare,
}

impl Default for DistanceModel {
    fn default() -> Self {
        DistanceModel::None
    }
}

impl std::fmt::Display for DistanceModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DistanceModel::None => write!(f, "none"),
            DistanceModel::Linear => write!(f, "linear"),
            DistanceModel::Quadratic => write!(f, "quadratic"),
            DistanceModel::InverseSquare => write!(f, "inverse-square"),
        }
    }
}

impl std::str::FromStr for DistanceModel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "none" => Ok(DistanceModel::None),
            "linear" => Ok(DistanceModel::Linear),
            "quadratic" => Ok(DistanceModel::Quadratic),
            "inverse-square" | "inversesquare" => Ok(DistanceModel::InverseSquare),
            _ => Err(format!(
                "Invalid distance model: '{}'. Valid options: none, linear, quadratic, inverse-square",
                s
            )),
        }
    }
}

/// How a 3-D position is reduced to a scalar distance for the distance model and
/// distance diffuse stages. `Spherical` is the Euclidean radius; `Chebyshev` is
/// the max-norm (L∞), i.e. 1 over the whole surface of the unit cube.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DistanceMetric {
    #[default]
    Spherical,
    Chebyshev,
}

impl DistanceMetric {
    /// Reduce a position to a scalar distance under this metric.
    #[inline]
    pub fn measure(self, position: [f32; 3]) -> f32 {
        match self {
            DistanceMetric::Spherical => {
                (position[0] * position[0] + position[1] * position[1] + position[2] * position[2])
                    .sqrt()
            }
            DistanceMetric::Chebyshev => position[0]
                .abs()
                .max(position[1].abs())
                .max(position[2].abs()),
        }
    }
}

impl std::fmt::Display for DistanceMetric {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DistanceMetric::Spherical => write!(f, "spherical"),
            DistanceMetric::Chebyshev => write!(f, "chebyshev"),
        }
    }
}

impl std::str::FromStr for DistanceMetric {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "spherical" | "euclidean" => Ok(DistanceMetric::Spherical),
            "chebyshev" | "cube" | "max" => Ok(DistanceMetric::Chebyshev),
            _ => Err(format!(
                "Invalid distance metric: '{}'. Valid options: spherical, chebyshev",
                s
            )),
        }
    }
}

/// Which ADM axes are negated to build the diffuse mirror image.
///
/// The flips compose into a *single* mirrored position, so the parity of the
/// selected set names the resulting symmetry: one flip is a reflection in the
/// plane normal to that axis, two flips a half-turn about the remaining axis,
/// and three a point inversion through the origin. With the renderer's
/// `x = right, y = front, z = up` convention, `xy` is the half-turn about the
/// vertical axis that the diffuse stage has always used, and it stays the
/// default so existing renders are unchanged.
///
/// Combining the flips into one image (rather than summing one image per axis)
/// is what keeps the historical behaviour expressible and the cost fixed at a
/// single extra backend evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MirrorAxes {
    pub x: bool,
    pub y: bool,
    pub z: bool,
}

impl MirrorAxes {
    /// No flip: the mirror coincides with the source.
    pub const NONE: Self = Self {
        x: false,
        y: false,
        z: false,
    };

    /// Half-turn about the vertical axis — the original antipodal diffuse.
    pub const VERTICAL_AXIS: Self = Self {
        x: true,
        y: true,
        z: false,
    };

    /// Point inversion through the origin — the true antipode.
    pub const ORIGIN: Self = Self {
        x: true,
        y: true,
        z: true,
    };

    /// True when no axis is flipped. The mirror is then the source itself and
    /// the blend collapses to identity after renormalization, so callers skip
    /// the second evaluation entirely.
    #[inline]
    pub fn is_identity(self) -> bool {
        !(self.x || self.y || self.z)
    }

    /// Negate the selected coordinates of a position.
    #[inline]
    pub fn reflect(self, position: [f64; 3]) -> [f64; 3] {
        [
            if self.x { -position[0] } else { position[0] },
            if self.y { -position[1] } else { position[1] },
            if self.z { -position[2] } else { position[2] },
        ]
    }

    /// Number of negated axes; its value names the symmetry (1 → plane,
    /// 2 → axis, 3 → point).
    #[inline]
    pub fn flip_count(self) -> u32 {
        u32::from(self.x) + u32::from(self.y) + u32::from(self.z)
    }
}

impl Default for MirrorAxes {
    fn default() -> Self {
        Self::VERTICAL_AXIS
    }
}

impl std::fmt::Display for MirrorAxes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_identity() {
            return write!(f, "none");
        }
        if self.x {
            write!(f, "x")?;
        }
        if self.y {
            write!(f, "y")?;
        }
        if self.z {
            write!(f, "z")?;
        }
        Ok(())
    }
}

impl std::str::FromStr for MirrorAxes {
    type Err = String;

    /// Parses an axis set written as the letters to negate, in any order and
    /// with optional `+`, `,`, `-` or space separators: `xy`, `x+y`, `z`,
    /// `none` (or the empty string) for no flip.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let normalized = s.trim().to_lowercase();
        if normalized.is_empty() || normalized == "none" {
            return Ok(Self::NONE);
        }

        let mut axes = Self::NONE;
        for c in normalized.chars() {
            match c {
                'x' => axes.x = true,
                'y' => axes.y = true,
                'z' => axes.z = true,
                '+' | ',' | '-' | '_' | ' ' => {}
                _ => {
                    return Err(format!(
                        "Invalid diffuse mirror axes: '{s}'. Expected any combination of x, y, z (e.g. 'xy'), or 'none'"
                    ));
                }
            }
        }
        Ok(axes)
    }
}

/// Amplitude factor applied to an object's gains at `distance` (layout units:
/// the speakers sit at about 1). Never above 1: distance only attenuates.
pub fn calculate_distance_attenuation(distance: f32, model: DistanceModel) -> f32 {
    match model {
        DistanceModel::None => 1.0,
        DistanceModel::Linear => 1.0 / (1.0 + distance),
        DistanceModel::Quadratic => 1.0 / (1.0 + distance * distance),
        // The inverse-square law holds for intensity; the gains are
        // amplitudes, so it is 1/d here (−6 dB per doubling), referenced to
        // the speakers: unity inside them, where panning carries the position.
        DistanceModel::InverseSquare => 1.0 / distance.max(1.0),
    }
}

#[cfg(test)]
mod attenuation_tests {
    use super::{DistanceModel, calculate_distance_attenuation};

    #[test]
    fn no_model_ever_raises_the_level() {
        for model in [
            DistanceModel::None,
            DistanceModel::Linear,
            DistanceModel::Quadratic,
            DistanceModel::InverseSquare,
        ] {
            for d in [0.0f32, 0.05, 0.1, 0.5, 0.99, 1.0, 2.0, 10.0] {
                let g = calculate_distance_attenuation(d, model);
                assert!((0.0..=1.0).contains(&g), "{model} at {d}: {g}");
            }
        }
    }

    #[test]
    fn inverse_square_is_the_amplitude_law_beyond_the_speakers() {
        let at = |d| calculate_distance_attenuation(d, DistanceModel::InverseSquare);
        // Unity from the centre out to the speakers.
        assert_eq!(at(0.0), 1.0);
        assert_eq!(at(0.5), 1.0);
        assert_eq!(at(1.0), 1.0);
        // −6.02 dB per doubling of distance (intensity falls as 1/d²).
        assert!((at(2.0) - 0.5).abs() < 1e-6);
        assert!((at(4.0) - 0.25).abs() < 1e-6);
        assert!((20.0 * at(2.0).log10() + 6.0206).abs() < 1e-3);
    }
}

#[cfg(test)]
mod mirror_axes_tests {
    use super::MirrorAxes;

    #[test]
    fn the_default_is_the_historical_half_turn_about_the_vertical_axis() {
        assert_eq!(MirrorAxes::default(), MirrorAxes::VERTICAL_AXIS);
        assert_eq!(
            MirrorAxes::default().reflect([0.3, 0.2, 0.1]),
            [-0.3, -0.2, 0.1]
        );
    }

    #[test]
    fn flip_count_names_the_symmetry() {
        assert_eq!(MirrorAxes::NONE.flip_count(), 0);
        assert_eq!(MirrorAxes::VERTICAL_AXIS.flip_count(), 2);
        assert_eq!(MirrorAxes::ORIGIN.flip_count(), 3);
        // A single flip is a plane reflection.
        assert_eq!("y".parse::<MirrorAxes>().unwrap().flip_count(), 1);
    }

    #[test]
    fn the_origin_inversion_negates_all_three_coordinates() {
        assert_eq!(
            MirrorAxes::ORIGIN.reflect([0.3, 0.2, 0.1]),
            [-0.3, -0.2, -0.1]
        );
    }

    #[test]
    fn only_the_empty_set_is_the_identity() {
        assert!(MirrorAxes::NONE.is_identity());
        assert!(!MirrorAxes::VERTICAL_AXIS.is_identity());
        assert!(!"z".parse::<MirrorAxes>().unwrap().is_identity());
    }

    #[test]
    fn parsing_accepts_any_order_and_the_usual_separators() {
        let expected = MirrorAxes::VERTICAL_AXIS;
        for input in ["xy", "yx", "x+y", "X, Y", " x-y ", "y_x"] {
            assert_eq!(input.parse::<MirrorAxes>().unwrap(), expected, "{input}");
        }
    }

    #[test]
    fn parsing_maps_an_absent_selection_to_no_flip() {
        for input in ["none", "", "  ", "NONE"] {
            assert_eq!(
                input.parse::<MirrorAxes>().unwrap(),
                MirrorAxes::NONE,
                "{input}"
            );
        }
    }

    #[test]
    fn parsing_rejects_letters_that_are_not_axes() {
        assert!("xw".parse::<MirrorAxes>().is_err());
    }

    #[test]
    fn display_round_trips_through_parsing() {
        for x in [false, true] {
            for y in [false, true] {
                for z in [false, true] {
                    let axes = MirrorAxes { x, y, z };
                    let rendered = axes.to_string();
                    assert_eq!(rendered.parse::<MirrorAxes>().unwrap(), axes, "{rendered}");
                }
            }
        }
    }
}
