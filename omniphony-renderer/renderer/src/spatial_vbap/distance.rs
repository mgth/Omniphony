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

pub fn calculate_distance_attenuation(distance: f32, model: DistanceModel) -> f32 {
    match model {
        DistanceModel::None => 1.0,
        DistanceModel::Linear => 1.0 / (1.0 + distance),
        DistanceModel::Quadratic => 1.0 / (1.0 + distance * distance),
        DistanceModel::InverseSquare => {
            const MIN_DISTANCE: f32 = 0.1;
            let clamped = distance.max(MIN_DISTANCE);
            1.0 / (clamped * clamped)
        }
    }
}
