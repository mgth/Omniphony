//! Volumetric panning: VBAP for the direction, depth for the rest.
//!
//! VBAP pans by direction alone: an object on the front wall and the same
//! object halfway to the listener get the same gains, and only the distance
//! attenuation tells them apart. Atmos positions are room-relative, so that
//! loses the one thing the format says about an object inside the room.
//!
//! This backend keeps VBAP's gains for the direction and measures, along the
//! ray from the listener through the object, how deep inside the loudspeaker
//! surface the object sits: `0` on the surface (and beyond it), `1` at the
//! listener. The deeper it is, the more of its energy moves from the face
//! VBAP picked to a *central distribution* that stands for "at the listener":
//! equal power over every loudspeaker, or the loudspeakers facing the object
//! across the listener (VBAP at the opposite direction).
//!
//! Geometrically this is the triangulation of the loudspeaker *directions*
//! lifted to the loudspeakers' real distances, with the listener as one more
//! vertex every face is joined to: a fan of tetrahedra, consistent by
//! construction, symmetric when the layout is, with no sliver cells. The
//! weight of the listener vertex is what the central distribution plays. On
//! the surface the gains are VBAP's, bit for bit, so there is no seam between
//! inside and outside the hull; outside, the out-of-hull modes apply
//! unchanged.
//!
//! Virtual loudspeakers (the poles that close an open hull, and whatever else
//! the triangulation adds) have no distance of their own. Each is placed on
//! the plane of the real loudspeakers it downmixes onto when those are
//! coplanar and that plane does not run through the listener, else at those
//! loudspeakers' mean distance. A layout without floor speakers thus gets a
//! cone of a floor hanging from its bed ring, and a wall closed around a
//! virtual centre keeps being a flat wall.

use anyhow::Result;

use super::room_transform::room_scaled_position;
use super::{BackendCapabilities, GainModel, RenderRequest, RenderResponse, VbapBackend};
use crate::spatial_vbap::vbap_native::{
    FACE_HIT_TOLERANCE, TriangulationView, compute_dummy_rings, unit_direction_deg,
};
use crate::spatial_vbap::{Gains, adm_to_spherical};
use crate::speaker_layout::SpeakerLayout;
use omniphony_geometry::f32::vec3::{cross, dot, length, sub, try_normalize};

/// What plays the share of an object that its depth takes off the VBAP face.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CentralDistribution {
    /// Equal power over every spatialized loudspeaker: the image dissolves
    /// into the whole array as the object nears the listener. Continuous
    /// through the listener's position.
    Uniform,
    /// VBAP at the opposite direction: the loudspeakers facing the object
    /// across the listener. The object stays a pair of images, the near wall
    /// and the far wall, that meet at equal level at the listener. The
    /// opposite direction is undefined at the listener's exact position,
    /// where the uniform distribution takes over.
    #[default]
    Antipode,
}

impl CentralDistribution {
    pub const UNIFORM: &'static str = "uniform";
    pub const ANTIPODE: &'static str = "antipode";

    /// The bag spelling of this choice.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uniform => Self::UNIFORM,
            Self::Antipode => Self::ANTIPODE,
        }
    }

    /// The choice a bag value names, or `None` for an unknown spelling.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            Self::UNIFORM => Some(Self::Uniform),
            Self::ANTIPODE => Some(Self::Antipode),
            _ => None,
        }
    }
}

/// The volumetric tuning, baked at construction from the param bag. Changing
/// it rebuilds the topology, so the hot path reads it from the model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VolumetricParams {
    pub central: CentralDistribution,
    /// Exponent applied to the depth before it weights the central
    /// distribution: `1` is linear in the distance along the ray, below `1`
    /// the centre takes over sooner, above `1` the wall holds on longer.
    pub depth_curve: f32,
}

impl VolumetricParams {
    pub const DEPTH_CURVE_MIN: f32 = 0.25;
    pub const DEPTH_CURVE_MAX: f32 = 4.0;
    pub const DEPTH_CURVE_DEFAULT: f32 = 1.0;
}

impl Default for VolumetricParams {
    fn default() -> Self {
        Self {
            central: CentralDistribution::default(),
            depth_curve: Self::DEPTH_CURVE_DEFAULT,
        }
    }
}

/// Below this distance from the listener an object has no direction: it is
/// at the listener, depth `1`, and the antipode is undefined.
const LISTENER_EPS: f32 = 1e-6;

/// A plane this close to the listener (or a ray this nearly parallel to a
/// plane) gives no usable distance.
const PLANE_EPS: f32 = 1e-6;

/// A depth below this is rounding: an object authored on a wall measures a
/// few ulps inside it, and must still get VBAP's gains bit for bit.
const DEPTH_EPS: f32 = 1e-5;

/// How far off a fitted plane a real loudspeaker may sit for its ring to
/// count as coplanar, relative to the ring's mean distance.
const COPLANAR_TOLERANCE: f32 = 1e-3;

/// One face of the loudspeaker surface: a triangulation face lifted to its
/// loudspeakers' real distances.
#[derive(Clone, Copy)]
struct SurfaceFace {
    /// Inverse loudspeaker matrix of the face, for `vbap3d`'s hit test.
    inverse: [f32; 9],
    /// Unit normal, pointing away from the listener.
    normal: [f32; 3],
    /// Distance from the listener to the plane along `normal`; `0` marks a
    /// face whose plane runs through the listener (or has no area), which
    /// cannot measure depth.
    offset: f32,
}

/// The surface the loudspeakers span, as the fan of planes depth is measured
/// against. Built once per topology; the hot path only sweeps it.
pub struct LoudspeakerSurface {
    faces: Vec<SurfaceFace>,
}

impl LoudspeakerSurface {
    /// The surface of `view`'s triangulation with every real loudspeaker `i`
    /// at `radii[i]` from the listener, in the room the triangulation was
    /// built in. Virtual loudspeakers get the distance described in the
    /// module docs.
    pub(crate) fn new(view: &TriangulationView<'_>, radii: &[f32]) -> Result<Self> {
        let n_real = radii.len();
        anyhow::ensure!(
            view.is_virtual.iter().filter(|virt| !**virt).count() == n_real
                && view.is_virtual[..n_real].iter().all(|virt| !virt),
            "the loudspeaker surface needs one distance per real loudspeaker of the \
             triangulation, in its order"
        );
        let rings = compute_dummy_rings(view.faces, view.is_virtual);
        let mut vertex_radius: Vec<f32> = radii.to_vec();
        vertex_radius.resize(view.unit_dirs.len(), 0.0);
        for ring in &rings {
            vertex_radius[ring.dummy] = dummy_radius(view, radii, ring.dummy, &ring.ring);
        }

        let faces = view
            .faces
            .iter()
            .zip(view.inverse)
            .map(|(face, inverse)| {
                let p = face.map(|vertex| scale(view.unit_dirs[vertex], vertex_radius[vertex]));
                let (normal, offset) = plane_through(p[0], p[1], p[2]);
                SurfaceFace {
                    inverse: *inverse,
                    normal,
                    offset,
                }
            })
            .collect();
        Ok(Self { faces })
    }

    /// Distance from the listener to the surface along `direction` (a unit
    /// vector in the triangulation's frame), or `None` when no face contains
    /// the direction (outside an open hull) or every face that does runs
    /// through the listener. Where faces overlap within the hit tolerance the
    /// nearest plane wins, so the answer does not depend on face order.
    pub fn distance_along(&self, direction: [f32; 3]) -> Option<f32> {
        let [x, y, z] = direction;
        let mut nearest: Option<f32> = None;
        for face in &self.faces {
            let inv = &face.inverse;
            let g0 = inv[0] * x + inv[1] * y + inv[2] * z;
            let g1 = inv[3] * x + inv[4] * y + inv[5] * z;
            let g2 = inv[6] * x + inv[7] * y + inv[8] * z;
            if g0.min(g1).min(g2) <= FACE_HIT_TOLERANCE || face.offset <= 0.0 {
                continue;
            }
            let along = dot(face.normal, direction);
            if along <= PLANE_EPS {
                continue;
            }
            let distance = face.offset / along;
            nearest = Some(nearest.map_or(distance, |best| best.min(distance)));
        }
        nearest
    }
}

#[inline]
fn scale(direction: [f32; 3], radius: f32) -> [f32; 3] {
    [
        direction[0] * radius,
        direction[1] * radius,
        direction[2] * radius,
    ]
}

/// The plane through three points as `(unit normal away from the listener,
/// offset)`; offset `0` when the points have no area or the plane runs
/// through the listener.
fn plane_through(a: [f32; 3], b: [f32; 3], c: [f32; 3]) -> ([f32; 3], f32) {
    let Some(mut normal) = try_normalize(cross(sub(b, a), sub(c, a)), 1e-12) else {
        return ([0.0; 3], 0.0);
    };
    let mut offset = dot(normal, a);
    if offset < 0.0 {
        normal = [-normal[0], -normal[1], -normal[2]];
        offset = -offset;
    }
    if offset <= PLANE_EPS {
        return (normal, 0.0);
    }
    (normal, offset)
}

/// Where a virtual loudspeaker stands: on the plane of its ring when the ring
/// is coplanar and that plane is off the listener, else at the ring's mean
/// distance. A ring too small for a plane takes the mean too.
fn dummy_radius(view: &TriangulationView<'_>, radii: &[f32], dummy: usize, ring: &[usize]) -> f32 {
    debug_assert!(!ring.is_empty(), "a virtual loudspeaker with no ring");
    let mean = ring.iter().map(|&i| radii[i]).sum::<f32>() / ring.len().max(1) as f32;
    if ring.len() < 3 {
        return mean;
    }
    let points: Vec<[f32; 3]> = ring
        .iter()
        .map(|&i| scale(view.unit_dirs[i], radii[i]))
        .collect();
    let Some((normal, offset)) = coplanar_plane(&points, mean) else {
        return mean;
    };
    let along = dot(normal, view.unit_dirs[dummy]);
    if along <= PLANE_EPS {
        return mean;
    }
    offset / along
}

/// The plane the points share, as `(unit normal away from the listener,
/// offset)`, when they are coplanar within `COPLANAR_TOLERANCE × scale` and
/// the plane is off the listener.
fn coplanar_plane(points: &[[f32; 3]], scale_hint: f32) -> Option<([f32; 3], f32)> {
    let n = points.len() as f32;
    let centroid = points.iter().fold([0.0f32; 3], |acc, p| {
        [acc[0] + p[0] / n, acc[1] + p[1] / n, acc[2] + p[2] / n]
    });
    // The best-defined normal among the pairs: the longest cross product.
    let mut best: Option<([f32; 3], f32)> = None;
    for (i, a) in points.iter().enumerate() {
        for b in &points[i + 1..] {
            let candidate = cross(sub(*a, centroid), sub(*b, centroid));
            let len = length(candidate);
            if best.is_none_or(|(_, best_len)| len > best_len) {
                best = Some((candidate, len));
            }
        }
    }
    let (normal, _) = best?;
    let mut normal = try_normalize(normal, 1e-12)?;
    let tolerance = COPLANAR_TOLERANCE * scale_hint.max(1e-3);
    if points
        .iter()
        .any(|p| dot(normal, sub(*p, centroid)).abs() > tolerance)
    {
        return None;
    }
    let mut offset = dot(normal, centroid);
    if offset < 0.0 {
        normal = [-normal[0], -normal[1], -normal[2]];
        offset = -offset;
    }
    (offset > PLANE_EPS).then_some((normal, offset))
}

/// VBAP on the loudspeaker surface, crossfaded towards a central distribution
/// by the object's depth inside that surface. See the module docs.
pub struct VolumetricBackend {
    vbap: VbapBackend,
    surface: LoudspeakerSurface,
    params: VolumetricParams,
}

impl VolumetricBackend {
    /// Wrap `vbap`, whose real loudspeaker `i` stands `speaker_radii[i]` from
    /// the listener in the room its triangulation was built in.
    pub fn new(vbap: VbapBackend, speaker_radii: &[f32], params: VolumetricParams) -> Result<Self> {
        anyhow::ensure!(
            speaker_radii.len() == vbap.speaker_count(),
            "the volumetric backend got {} loudspeaker distances for {} loudspeakers",
            speaker_radii.len(),
            vbap.speaker_count()
        );
        let view = vbap.panner().triangulation().ok_or_else(|| {
            anyhow::anyhow!(
                "the volumetric backend needs the native VBAP triangulation; this build \
                 pans with the SAF library, which keeps its own"
            )
        })?;
        let surface = LoudspeakerSurface::new(&view, speaker_radii)?;
        let depth_curve = if params.depth_curve.is_finite() {
            params.depth_curve.clamp(
                VolumetricParams::DEPTH_CURVE_MIN,
                VolumetricParams::DEPTH_CURVE_MAX,
            )
        } else {
            VolumetricParams::DEPTH_CURVE_DEFAULT
        };
        Ok(Self {
            vbap,
            surface,
            params: VolumetricParams {
                central: params.central,
                depth_curve,
            },
        })
    }

    pub fn speaker_count(&self) -> usize {
        self.vbap.speaker_count()
    }

    pub fn params(&self) -> VolumetricParams {
        self.params
    }

    /// The object's position in the room, as VBAP pans it (below the horizon
    /// clamped to it when the panner does not allow negative z).
    fn scaled_position(&self, req: &RenderRequest) -> [f32; 3] {
        let [x, y, z] = room_scaled_position(
            req.adm_position.map(|value| value as f32),
            req.room_ratio,
            req.room_ratio_rear,
            req.room_ratio_lower,
            req.room_ratio_center_blend,
        );
        let z = if self.vbap.panner().allow_negative_z() {
            z
        } else {
            z.max(0.0)
        };
        [x, y, z]
    }

    /// How deep inside the loudspeaker surface the request's object sits:
    /// `0` on the surface and beyond, `1` at the listener, linear in the
    /// distance along the ray between them, then bent by the depth curve.
    pub fn depth(&self, req: &RenderRequest) -> f32 {
        self.depth_and_reach(self.scaled_position(req)).0
    }

    /// The depth of `position`, and how far the surface is behind the
    /// listener in the opposite direction (`None` when that is unknown or
    /// irrelevant).
    fn depth_and_reach(&self, position: [f32; 3]) -> (f32, Option<(f32, f32)>) {
        let (azimuth, elevation, radius) = adm_to_spherical(position[0], position[1], position[2]);
        if radius <= LISTENER_EPS {
            return (1.0, None);
        }
        let direction = unit_direction_deg(azimuth, elevation);
        let Some(ahead) = self.surface.distance_along(direction) else {
            return (0.0, None);
        };
        if ahead <= PLANE_EPS {
            return (0.0, None);
        }
        let linear = (1.0 - radius / ahead).clamp(0.0, 1.0);
        if linear <= DEPTH_EPS {
            return (0.0, None);
        }
        let depth = if self.params.depth_curve == 1.0 {
            linear
        } else {
            linear.powf(self.params.depth_curve)
        };
        let behind = self
            .surface
            .distance_along([-direction[0], -direction[1], -direction[2]]);
        (depth, Some((ahead, behind.unwrap_or(ahead))))
    }

    /// The share of the object's power the central distribution gets. For the
    /// uniform distribution, whose centre of power is the listener, it is the
    /// depth itself. For the antipode it is the depth scaled so the power
    /// centroid of the near and far faces lands on the object: the two faces
    /// meet at equal level at the listener, not before.
    fn central_weight(&self, depth: f32, reach: Option<(f32, f32)>) -> f32 {
        match (self.params.central, reach) {
            (CentralDistribution::Antipode, Some((ahead, behind))) if ahead + behind > 0.0 => {
                depth * ahead / (ahead + behind)
            }
            // At the listener the antipode is undefined and the uniform
            // distribution plays alone.
            _ => depth,
        }
    }

    fn uniform_gains(&self) -> Gains {
        let n = self.speaker_count();
        let mut gains = Gains::zeroed(n);
        let gain = 1.0 / (n.max(1) as f32).sqrt();
        for index in 0..n {
            gains.set(index, gain);
        }
        gains
    }

    fn central_gains(&self, req: &RenderRequest, at_listener: bool) -> Gains {
        match self.params.central {
            CentralDistribution::Uniform => self.uniform_gains(),
            CentralDistribution::Antipode if at_listener => self.uniform_gains(),
            CentralDistribution::Antipode => {
                // Mirrored on the authored position, like the diffuse decorator:
                // the warp of an asymmetric room is not an odd function.
                let mut mirror = *req;
                mirror.adm_position = req.adm_position.map(|value| -value);
                self.vbap.compute_gains(&mirror).gains
            }
        }
    }

    pub fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        let face = self.vbap.compute_gains(req).gains;
        let position = self.scaled_position(req);
        let (depth, reach) = self.depth_and_reach(position);
        if depth <= 0.0 {
            // On the surface and beyond: VBAP, untouched.
            return RenderResponse { gains: face };
        }
        let weight = self.central_weight(depth, reach).clamp(0.0, 1.0);
        let central = self.central_gains(req, reach.is_none());

        // Equal-power crossfade, then renormalised: the two sets share
        // loudspeakers, so their coherent sum is not unit power.
        let w_face = (1.0 - weight).sqrt();
        let w_central = weight.sqrt();
        let n = face.len().min(central.len());
        let mut gains = Gains::zeroed(self.speaker_count());
        let mut energy = 0.0f32;
        for index in 0..n {
            let gain = w_face * face[index] + w_central * central[index];
            gains.set(index, gain);
            energy += gain * gain;
        }
        gains.normalize_to_unit_energy(energy);
        RenderResponse { gains }
    }

    pub fn save_to_file(
        &self,
        path: &std::path::Path,
        speaker_layout: &SpeakerLayout,
    ) -> Result<()> {
        self.vbap.save_to_file(path, speaker_layout)
    }
}

impl GainModel for VolumetricBackend {
    fn backend_id(&self) -> &'static str {
        "volumetric"
    }

    fn backend_label(&self) -> &'static str {
        "Volumetric"
    }

    fn capabilities(&self) -> BackendCapabilities {
        // Everything VBAP offers: the depth is a pure function of position,
        // so both sampled tables hold it (the polar one on its distance axis).
        self.vbap.capabilities()
    }

    fn speaker_count(&self) -> usize {
        VolumetricBackend::speaker_count(self)
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        VolumetricBackend::compute_gains(self, req)
    }

    fn save_to_file(&self, path: &std::path::Path, speaker_layout: &SpeakerLayout) -> Result<()> {
        VolumetricBackend::save_to_file(self, path, speaker_layout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live_params::RoomRatios;
    use crate::render_backend::VbapSpreadParams;
    use crate::spatial_vbap::{DistanceModel, MirrorAxes, OutOfHullMode, VbapPanner};
    use crate::speaker_layout::{Speaker, SpeakerLayout};

    fn request(position: [f64; 3]) -> RenderRequest {
        RenderRequest {
            adm_position: position,
            event_size: [0.0; 3],
            room_ratio: [1.0, 1.0, 1.0],
            room_ratio_rear: 1.0,
            room_ratio_lower: 1.0,
            room_ratio_center_blend: 0.0,
            use_distance_diffuse: false,
            distance_diffuse_threshold: 1.0,
            distance_diffuse_curve: 1.0,
            diffuse_mirror_axes: MirrorAxes::default(),
            distance_model: DistanceModel::None,
        }
    }

    fn vbap(layout: &SpeakerLayout) -> VbapBackend {
        let room = RoomRatios::UNIT;
        let dirs = layout
            .spatializable_positions_for_room(room.ratio, room.rear, room.lower, room.center_blend)
            .0;
        let panner = VbapPanner::new(&dirs, 5, 5, 0.0, OutOfHullMode::default())
            .expect("triangulation")
            .with_negative_z(true);
        VbapBackend::new(panner, VbapSpreadParams::default())
    }

    fn volumetric(layout: &SpeakerLayout, params: VolumetricParams) -> VolumetricBackend {
        let room = RoomRatios::UNIT;
        let radii = layout.spatializable_radii_for_room(
            room.ratio,
            room.rear,
            room.lower,
            room.center_blend,
        );
        VolumetricBackend::new(vbap(layout), &radii, params).expect("volumetric backend")
    }

    fn with_central(central: CentralDistribution) -> VolumetricParams {
        VolumetricParams {
            central,
            ..VolumetricParams::default()
        }
    }

    /// The shipped 7.1.4: eleven loudspeakers on the walls and ceiling of the
    /// unit cube, no floor.
    fn layout_714() -> SpeakerLayout {
        SpeakerLayout::preset_7_1_4().expect("7.1.4 preset")
    }

    /// Six loudspeakers on the axes: every hull face is a triangle of its
    /// own, so the gains have no arbitrary diagonal to break their symmetry.
    fn octahedron() -> SpeakerLayout {
        let speaker =
            |name: &str, x: f32, y: f32, z: f32| Speaker::from_cartesian(name, x, y, z, true, 0.0);
        SpeakerLayout::from_speakers(vec![
            speaker("L", -1.0, 0.0, 0.0),
            speaker("R", 1.0, 0.0, 0.0),
            speaker("C", 0.0, 1.0, 0.0),
            speaker("B", 0.0, -1.0, 0.0),
            speaker("T", 0.0, 0.0, 1.0),
            speaker("D", 0.0, 0.0, -1.0),
        ])
        .expect("octahedron")
    }

    /// Five loudspeakers at ear level and nothing else: the hull is flat and
    /// both poles are virtual.
    fn flat_ring() -> SpeakerLayout {
        SpeakerLayout::from_speakers(
            [0.0f32, -30.0, 30.0, -110.0, 110.0]
                .iter()
                .enumerate()
                .map(|(i, &az)| Speaker::new(format!("S{i}"), az, 0.0))
                .collect(),
        )
        .expect("flat ring")
    }

    fn rms(gains: &Gains) -> f32 {
        gains.iter().map(|g| g * g).sum::<f32>().sqrt()
    }

    fn index_of(layout: &SpeakerLayout, name: &str) -> usize {
        layout
            .speakers
            .iter()
            .filter(|s| s.spatialize)
            .position(|s| s.name == name)
            .unwrap_or_else(|| panic!("no speaker {name}"))
    }

    #[test]
    fn on_the_surface_and_beyond_the_gains_are_vbap_bit_for_bit() {
        let layout = layout_714();
        let plain = vbap(&layout);
        for central in [CentralDistribution::Uniform, CentralDistribution::Antipode] {
            let model = volumetric(&layout, with_central(central));
            for position in [
                [0.3, 1.0, 0.2],
                [-1.0, 0.4, 0.7],
                [0.5, -1.0, 0.0],
                [0.0, 0.2, 1.0],
                [-0.95, 1.0, 0.95],
                [1.4, 1.4, 0.3],
                [0.0, -2.0, -1.0],
            ] {
                let req = request(position);
                assert_eq!(
                    model.depth(&req),
                    0.0,
                    "{position:?} is on or beyond the surface"
                );
                let expected = plain.compute_gains(&req).gains;
                let got = model.compute_gains(&req).gains;
                assert_eq!(&got[..], &expected[..], "{position:?} with {central:?}");
            }
        }
    }

    #[test]
    fn depth_runs_linearly_from_the_wall_to_the_listener() {
        let model = volumetric(&layout_714(), VolumetricParams::default());
        let wall = [-0.6, 1.0, 0.3];
        for step in 0..=10 {
            let s = step as f64 / 10.0;
            let depth = model.depth(&request(wall.map(|v| v * s)));
            let expected = 1.0 - s as f32;
            assert!(
                (depth - expected).abs() < 1e-4,
                "at {s} of the way to the wall: depth {depth}, expected {expected}"
            );
        }
        assert_eq!(model.depth(&request(wall.map(|v| v * 1.3))), 0.0);
    }

    #[test]
    fn depth_is_mirror_symmetric() {
        let model = volumetric(&layout_714(), VolumetricParams::default());
        for x in [0.2, 0.5, 0.8] {
            for y in [-0.7, 0.0, 0.6] {
                for z in [0.0, 0.4, 0.9] {
                    let right = model.depth(&request([x, y, z]));
                    let left = model.depth(&request([-x, y, z]));
                    assert!(
                        (right - left).abs() < 1e-5,
                        "({x}, {y}, {z}): depth {right} right, {left} left"
                    );
                }
            }
        }
    }

    #[test]
    fn gains_are_mirror_symmetric_on_an_unambiguous_layout() {
        let layout = octahedron();
        let (l, r) = (index_of(&layout, "L"), index_of(&layout, "R"));
        for central in [CentralDistribution::Uniform, CentralDistribution::Antipode] {
            let model = volumetric(&layout, with_central(central));
            for position in [
                [0.3, 0.4, 0.2],
                [0.5, -0.2, 0.3],
                [0.1, 0.1, 0.6],
                [0.05, 0.0, 0.0],
            ] {
                let gains = model.compute_gains(&request(position)).gains;
                let mirrored = model
                    .compute_gains(&request([-position[0], position[1], position[2]]))
                    .gains;
                for index in 0..gains.len() {
                    let twin = if index == l {
                        r
                    } else if index == r {
                        l
                    } else {
                        index
                    };
                    assert!(
                        (gains[index] - mirrored[twin]).abs() < 1e-5,
                        "{position:?} with {central:?}: speaker {index} {} vs its mirror {}",
                        gains[index],
                        mirrored[twin]
                    );
                }
            }
        }
    }

    #[test]
    fn every_depth_keeps_unit_power() {
        let layout = layout_714();
        for central in [CentralDistribution::Uniform, CentralDistribution::Antipode] {
            let model = volumetric(&layout, with_central(central));
            for wall in [
                [-0.6, 1.0, 0.3],
                [1.0, -0.4, 0.8],
                [0.2, 0.3, 1.0],
                [0.0, -1.0, 0.0],
            ] {
                for step in 1..20 {
                    let s = step as f64 / 20.0;
                    let gains = model.compute_gains(&request(wall.map(|v| v * s))).gains;
                    let power = rms(&gains);
                    assert!(
                        (power - 1.0).abs() < 1e-4,
                        "{wall:?} at {s} with {central:?}: rms {power}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_uniform_rule_dissolves_the_image_at_the_listener() {
        let layout = layout_714();
        let model = volumetric(&layout, with_central(CentralDistribution::Uniform));
        let n = model.speaker_count();
        let uniform = 1.0 / (n as f32).sqrt();
        let at_listener = model.compute_gains(&request([0.0, 0.0, 0.0])).gains;
        for gain in at_listener.iter() {
            assert!((gain - uniform).abs() < 1e-6, "{gain} vs {uniform}");
        }
        let nearly = model
            .compute_gains(&request([-0.0006, 0.001, 0.0003]))
            .gains;
        for gain in nearly.iter() {
            assert!((gain - uniform).abs() < 0.05, "{gain} vs {uniform}");
        }
    }

    #[test]
    fn the_antipode_rule_places_the_power_centroid_on_the_object() {
        let layout = octahedron();
        let model = volumetric(&layout, with_central(CentralDistribution::Antipode));
        let (c, b) = (index_of(&layout, "C"), index_of(&layout, "B"));

        // Halfway to the front wall: three quarters of the power ahead, one
        // quarter behind, so the power centroid sits at 0.5.
        let gains = model.compute_gains(&request([0.0, 0.5, 0.0])).gains;
        assert!(
            (gains[c] * gains[c] - 0.75).abs() < 1e-3,
            "front {}",
            gains[c]
        );
        assert!(
            (gains[b] * gains[b] - 0.25).abs() < 1e-3,
            "back {}",
            gains[b]
        );
        for (index, gain) in gains.iter().enumerate() {
            if index != c && index != b {
                assert!(
                    gain.abs() < 1e-4,
                    "speaker {index} should be silent, got {gain}"
                );
            }
        }

        // At the listener the two walls meet at equal level.
        let gains = model.compute_gains(&request([0.0, 1e-4, 0.0])).gains;
        assert!(
            (gains[c] - gains[b]).abs() < 1e-3,
            "front {} vs back {}",
            gains[c],
            gains[b]
        );
    }

    #[test]
    fn depth_is_continuous_across_a_face_edge() {
        let model = volumetric(&layout_714(), VolumetricParams::default());
        // A horizontal arc at half radius, sweeping through the front-left
        // corner: the front and left walls meet there at the same distance.
        let mut previous: Option<f32> = None;
        for step in 0..=100 {
            let az = (-60.0 + step as f64 * 0.5).to_radians();
            let depth = model.depth(&request([0.5 * az.sin(), 0.5 * az.cos(), 0.0]));
            if let Some(previous) = previous {
                assert!(
                    (depth - previous).abs() < 0.01,
                    "depth stepped from {previous} to {depth} at {}°",
                    az.to_degrees()
                );
            }
            previous = Some(depth);
        }
    }

    #[test]
    fn below_an_open_hull_the_virtual_pole_hangs_a_finite_floor() {
        let layout = layout_714();
        for central in [CentralDistribution::Uniform, CentralDistribution::Antipode] {
            let model = volumetric(&layout, with_central(central));
            for position in [[0.2, 0.3, -0.5], [0.0, 0.0, -1.0], [-0.4, -0.4, -0.2]] {
                let req = request(position);
                let depth = model.depth(&req);
                assert!(
                    depth.is_finite() && depth > 0.0 && depth < 1.0,
                    "{position:?}: depth {depth}"
                );
                let gains = model.compute_gains(&req).gains;
                assert!(
                    gains.iter().all(|g| g.is_finite()),
                    "{position:?}: {:?}",
                    &gains[..]
                );
                assert!(
                    (rms(&gains) - 1.0).abs() < 1e-4,
                    "{position:?}: rms {}",
                    rms(&gains)
                );
            }
        }
    }

    #[test]
    fn the_depth_curve_bends_the_depth() {
        let layout = layout_714();
        let linear = volumetric(&layout, VolumetricParams::default());
        let bent = volumetric(
            &layout,
            VolumetricParams {
                depth_curve: 2.0,
                ..VolumetricParams::default()
            },
        );
        let req = request([-0.3, 0.5, 0.15]);
        let depth = linear.depth(&req);
        assert!((depth - 0.5).abs() < 1e-4, "linear depth {depth}");
        assert!(
            (bent.depth(&req) - 0.25).abs() < 1e-4,
            "bent depth {}",
            bent.depth(&req)
        );
        // Out-of-range and non-finite curves fall back to the bounds and the default.
        let clamped = volumetric(
            &layout,
            VolumetricParams {
                depth_curve: 100.0,
                ..VolumetricParams::default()
            },
        );
        assert_eq!(
            clamped.params().depth_curve,
            VolumetricParams::DEPTH_CURVE_MAX
        );
        let nan = volumetric(
            &layout,
            VolumetricParams {
                depth_curve: f32::NAN,
                ..VolumetricParams::default()
            },
        );
        assert_eq!(
            nan.params().depth_curve,
            VolumetricParams::DEPTH_CURVE_DEFAULT
        );
    }

    #[test]
    fn a_flat_layout_hangs_its_surface_from_both_poles() {
        let layout = flat_ring();
        for central in [CentralDistribution::Uniform, CentralDistribution::Antipode] {
            let model = volumetric(&layout, with_central(central));
            for position in [
                [0.0, 0.5, 0.0],
                [0.3, -0.2, 0.4],
                [0.0, 0.0, 0.5],
                [0.1, 0.1, -0.6],
            ] {
                let req = request(position);
                let depth = model.depth(&req);
                assert!(
                    depth.is_finite() && depth > 0.0 && depth < 1.0,
                    "{position:?}: depth {depth}"
                );
                let gains = model.compute_gains(&req).gains;
                assert!(
                    (rms(&gains) - 1.0).abs() < 1e-4,
                    "{position:?}: rms {}",
                    rms(&gains)
                );
            }
        }
    }

    #[test]
    fn the_surface_refuses_mismatched_radii() {
        let layout = layout_714();
        let err = VolumetricBackend::new(vbap(&layout), &[1.0; 3], VolumetricParams::default())
            .err()
            .expect("three distances for eleven loudspeakers");
        assert!(err.to_string().contains("3 loudspeaker distances"), "{err}");
    }

    /// A ring of real loudspeakers on one wall, as the triangulation closes a
    /// coplanar face around a virtual centre: the centre lands on that wall,
    /// so the wall stays flat and an object on it reads depth 0.
    #[test]
    fn a_virtual_loudspeaker_on_a_coplanar_ring_lands_on_its_plane() {
        let corners = [
            [-1.0f32, -1.0, 0.0],
            [1.0, -1.0, 0.0],
            [1.0, -1.0, 1.0],
            [-1.0, -1.0, 1.0],
        ];
        let centre = [0.0f32, -1.0, 0.5];
        let mut unit_dirs: Vec<[f32; 3]> = corners
            .iter()
            .map(|p| try_normalize(*p, 1e-9).unwrap())
            .collect();
        unit_dirs.push(try_normalize(centre, 1e-9).unwrap());
        let radii: Vec<f32> = corners.iter().map(|p| length(*p)).collect();
        let is_virtual = [false, false, false, false, true];
        let view = TriangulationView {
            unit_dirs: &unit_dirs,
            faces: &[],
            inverse: &[],
            is_virtual: &is_virtual,
        };
        let radius = dummy_radius(&view, &radii, 4, &[0, 1, 2, 3]);
        assert!(
            (radius - length(centre)).abs() < 1e-4,
            "the centre should sit on the wall at {}, got {radius}",
            length(centre)
        );

        // A ring on a plane through the listener (a bed ring under a nadir
        // pole) offers no plane: the pole takes the ring's mean distance.
        let bed = [
            [-1.0f32, 1.0, 0.0],
            [1.0, 1.0, 0.0],
            [1.0, -1.0, 0.0],
            [-1.0, -1.0, 0.0],
        ];
        let mut unit_dirs: Vec<[f32; 3]> = bed
            .iter()
            .map(|p| try_normalize(*p, 1e-9).unwrap())
            .collect();
        unit_dirs.push([0.0, 0.0, -1.0]);
        let radii: Vec<f32> = bed.iter().map(|p| length(*p)).collect();
        let view = TriangulationView {
            unit_dirs: &unit_dirs,
            faces: &[],
            inverse: &[],
            is_virtual: &is_virtual,
        };
        let radius = dummy_radius(&view, &radii, 4, &[0, 1, 2, 3]);
        assert!(
            (radius - 2.0f32.sqrt()).abs() < 1e-5,
            "mean distance, got {radius}"
        );
    }
}
