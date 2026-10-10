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
//! The surface is always closed: it is triangulated with the virtual poles
//! whatever out-of-hull mode the VBAP underneath renders with, so the depth
//! stays continuous when a direction leaves an open hull (where that VBAP
//! folds its gains onto the boundary, continuously too). Virtual
//! loudspeakers (the poles, and the centres closing coplanar faces) have no
//! distance of their own. Each is placed on the plane of the real
//! loudspeakers it downmixes onto when those are coplanar and that plane
//! does not run through the listener, else at those loudspeakers' mean
//! distance. A layout without floor speakers thus gets a cone of a floor
//! hanging from its bed ring, and a wall closed around a virtual centre
//! keeps being a flat wall.
//!
//! The antipode is VBAP at the direction opposite the object as it is
//! panned (below the horizon clamped to it when the panner does not allow
//! negative z): the opposite direction itself is never clamped, so an
//! object overhead has its antipode under the floor, not in front.

use anyhow::Result;

use super::room_transform::room_scaled_position;
use super::{
    BackendCapabilities, GainModel, GainScratch, RenderRequest, VbapBackend, foreign_scratch,
};
use crate::spatial_vbap::vbap_native::{
    FACE_HIT_TOLERANCE, compute_dummy_rings, invert_ls_mtx_3d, prepare_triangulation,
    unit_direction_deg,
};
use crate::spatial_vbap::{OutOfHullMode, adm_to_spherical};
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

/// A linear depth below this is rounding: an object authored on a wall
/// measures a few ulps inside it (about `1e-7`), and must still get VBAP's
/// gains bit for bit. The band is a ramp, not a step, and sits before the
/// depth curve.
const DEPTH_EPS: f32 = 1e-6;

/// The linear depth over which the central share fades in, in amplitude,
/// from the surface. The equal-power law and a depth curve below `1` both
/// have an unbounded slope at zero depth: without this, one `f32` quantum
/// of position past the rounding band is an audible step of the central
/// distribution. Over this band the step a quantum can make stays below
/// a thousandth of full scale, and past it the law applies untouched.
const FADE_IN: f32 = 1e-4;

/// Where an object sits between the surface and the listener.
#[derive(Clone, Copy)]
struct Depth {
    /// `0` on the surface and beyond, `1` at the listener, linear along the
    /// ray, after the rounding band.
    linear: f32,
    /// `linear` bent by the depth curve.
    curved: f32,
    /// How far the surface is ahead along the ray and behind the listener in
    /// the opposite direction; `None` when the object is at the listener.
    reach: Option<(f32, f32)>,
}

impl Depth {
    const SURFACE: Self = Self {
        linear: 0.0,
        curved: 0.0,
        reach: None,
    };
    const LISTENER: Self = Self {
        linear: 1.0,
        curved: 1.0,
        reach: None,
    };
}

/// Read-only view of a triangulation: what the surface is built from.
#[derive(Clone, Copy)]
struct TriangulationView<'a> {
    /// Unit direction of every effective loudspeaker: the real ones first,
    /// in layout order, then the virtual ones.
    unit_dirs: &'a [[f32; 3]],
    /// The faces, as indices into `unit_dirs`.
    faces: &'a [[usize; 3]],
    /// The inverse loudspeaker matrix of each face, row-major: `vbap3d`'s
    /// hit test is `inverse · direction` all above `FACE_HIT_TOLERANCE`.
    inverse: &'a [[f32; 9]],
    /// Per effective loudspeaker, whether it is virtual: a pole closing an
    /// open hull, or the centre of a coplanar face.
    is_virtual: &'a [bool],
}

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
    /// The surface of the loudspeakers at `dirs_deg` (`[azimuth, elevation]`
    /// in degrees, as the panner takes them), real loudspeaker `i` standing
    /// `radii[i]` from the listener. The triangulation is closed with the
    /// virtual poles whatever the panner's own out-of-hull mode, so that a
    /// direction outside an open hull still meets the surface.
    pub(crate) fn from_directions(dirs_deg: &[[f32; 2]], radii: &[f32]) -> Result<Self> {
        anyhow::ensure!(
            dirs_deg.len() == radii.len(),
            "the loudspeaker surface got {} directions for {} distances",
            dirs_deg.len(),
            radii.len()
        );
        let tri = prepare_triangulation(dirs_deg, true, true, OutOfHullMode::VirtualPoles)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "the loudspeaker surface cannot be triangulated for {} loudspeakers",
                    dirs_deg.len()
                )
            })?;
        let inverse = invert_ls_mtx_3d(&tri.u_spkr, &tri.ls_groups);
        let is_virtual: Vec<bool> = tri
            .is_dummy
            .iter()
            .zip(&tri.is_centre)
            .map(|(pole, centre)| *pole || *centre)
            .collect();
        Self::new(
            &TriangulationView {
                unit_dirs: &tri.u_spkr,
                faces: &tri.ls_groups,
                inverse: &inverse,
                is_virtual: &is_virtual,
            },
            radii,
        )
    }

    /// The surface of `view`'s triangulation with every real loudspeaker `i`
    /// at `radii[i]` from the listener, in the room the triangulation was
    /// built in. Virtual loudspeakers get the distance described in the
    /// module docs.
    fn new(view: &TriangulationView<'_>, radii: &[f32]) -> Result<Self> {
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
    /// Wrap `vbap`, built on the loudspeakers at `speaker_dirs_deg` (the
    /// directions the panner was given), real loudspeaker `i` standing
    /// `speaker_radii[i]` from the listener in the room those directions were
    /// placed in.
    pub fn new(
        vbap: VbapBackend,
        speaker_dirs_deg: &[[f32; 2]],
        speaker_radii: &[f32],
        params: VolumetricParams,
    ) -> Result<Self> {
        anyhow::ensure!(
            speaker_dirs_deg.len() == vbap.speaker_count()
                && speaker_radii.len() == vbap.speaker_count(),
            "the volumetric backend got {} loudspeaker directions and {} distances for {} \
             loudspeakers",
            speaker_dirs_deg.len(),
            speaker_radii.len(),
            vbap.speaker_count()
        );
        let surface = LoudspeakerSurface::from_directions(speaker_dirs_deg, speaker_radii)?;
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
        self.measure(self.scaled_position(req)).curved
    }

    fn measure(&self, position: [f32; 3]) -> Depth {
        let (azimuth, elevation, radius) = adm_to_spherical(position[0], position[1], position[2]);
        if radius <= LISTENER_EPS {
            return Depth::LISTENER;
        }
        let direction = unit_direction_deg(azimuth, elevation);
        let Some(ahead) = self.surface.distance_along(direction) else {
            return Depth::SURFACE;
        };
        if ahead <= PLANE_EPS {
            return Depth::SURFACE;
        }
        let linear = (1.0 - radius / ahead).clamp(0.0, 1.0);
        // The rounding band, as a ramp: zero up to `DEPTH_EPS`, then rising
        // continuously to one at the listener.
        let linear = ((linear - DEPTH_EPS) / (1.0 - DEPTH_EPS)).max(0.0);
        if linear <= 0.0 {
            return Depth::SURFACE;
        }
        let curved = if self.params.depth_curve == 1.0 {
            linear
        } else {
            linear.powf(self.params.depth_curve)
        };
        let behind = self
            .surface
            .distance_along([-direction[0], -direction[1], -direction[2]]);
        Depth {
            linear,
            curved,
            reach: Some((ahead, behind.unwrap_or(ahead))),
        }
    }

    /// The share of the object's power the central distribution gets. For the
    /// uniform distribution, whose centre of power is the listener, it is the
    /// depth itself. For the antipode it is the depth scaled so the power
    /// centroid of the near and far faces lands on the object: the two faces
    /// meet at equal level at the listener, not before. Next to the surface
    /// the share fades in over [`FADE_IN`] (in amplitude, so squared here).
    fn central_weight(&self, depth: Depth) -> f32 {
        let share = match (self.params.central, depth.reach) {
            (CentralDistribution::Antipode, Some((ahead, behind))) if ahead + behind > 0.0 => {
                depth.curved * ahead / (ahead + behind)
            }
            // At the listener the antipode is undefined and the uniform
            // distribution plays alone.
            _ => depth.curved,
        };
        let fade_in = (depth.linear / FADE_IN).min(1.0);
        (share * fade_in * fade_in).clamp(0.0, 1.0)
    }

    fn uniform_gains(&self, central: &mut [f32]) {
        central.fill(1.0 / (self.speaker_count().max(1) as f32).sqrt());
    }

    /// Write into `central` the central distribution for the request's object
    /// at `position` (as panned: room-scaled, clamped to the horizon when the
    /// panner does so).
    fn central_gains(
        &self,
        req: &RenderRequest,
        position: [f32; 3],
        at_listener: bool,
        central: &mut [f32],
    ) {
        match self.params.central {
            CentralDistribution::Uniform => self.uniform_gains(central),
            CentralDistribution::Antipode if at_listener => self.uniform_gains(central),
            CentralDistribution::Antipode => {
                // The direction opposite the object, in the space `reach` was
                // measured in, panned as a bare direction: the panner's clamp
                // applies to positions, and would send the antipode of an
                // object overhead to the front instead of under the floor.
                let (azimuth, elevation, _) =
                    adm_to_spherical(-position[0], -position[1], -position[2]);
                let spread = self.vbap.effective_spread(req, position);
                self.vbap
                    .panner()
                    .gains_spread_into(azimuth, elevation, spread, central)
            }
        }
    }

    /// The working memory of one caller: the central distribution's gains
    /// while they are blended into the VBAP face's.
    pub fn new_scratch(&self) -> GainScratch {
        GainScratch::new(vec![0.0f32; self.speaker_count()])
    }

    pub fn compute_gains(&self, req: &RenderRequest, scratch: &mut GainScratch, out: &mut [f32]) {
        let Some(central) = scratch.state::<Vec<f32>>() else {
            return foreign_scratch(out);
        };
        // The VBAP face's gains, then blended in place.
        self.vbap.compute_gains(req, out);
        let position = self.scaled_position(req);
        let depth = self.measure(position);
        if depth.curved <= 0.0 {
            // On the surface and beyond: VBAP, untouched.
            return;
        }
        let weight = self.central_weight(depth);
        self.central_gains(req, position, depth.reach.is_none(), central);

        // Equal-power crossfade, then renormalised: the two sets share
        // loudspeakers, so their coherent sum is not at the level they blend
        // to. That level is the power-weighted mean of theirs, which keeps
        // the VBAP's own at the surface (a `fade` VBAP attenuates a direction
        // outside its hull) and reaches the central distribution's at the
        // listener.
        let w_face = (1.0 - weight).sqrt();
        let w_central = weight.sqrt();
        // One gain per speaker on both sides; a buffer of another width
        // than the scratch leaves the speakers they do not share silent.
        let n = out.len().min(central.len());
        out[n..].fill(0.0);
        let mut energy = 0.0f32;
        let mut energy_face = 0.0f32;
        let mut energy_central = 0.0f32;
        for (out, &central) in out.iter_mut().zip(central.iter()) {
            let face = *out;
            let gain = w_face * face + w_central * central;
            *out = gain;
            energy += gain * gain;
            energy_face += face * face;
            energy_central += central * central;
        }
        let target = (1.0 - weight) * energy_face + weight * energy_central;
        if energy > 1e-12 {
            let scale = (target / energy).sqrt();
            for gain in out.iter_mut() {
                *gain *= scale;
            }
        }
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

    fn new_scratch(&self) -> GainScratch {
        VolumetricBackend::new_scratch(self)
    }

    fn compute_gains(&self, req: &RenderRequest, scratch: &mut GainScratch, out: &mut [f32]) {
        VolumetricBackend::compute_gains(self, req, scratch, out)
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

    fn dirs(layout: &SpeakerLayout) -> Vec<[f32; 2]> {
        let room = RoomRatios::UNIT;
        layout
            .spatializable_positions_for_room(room.ratio, room.rear, room.lower, room.center_blend)
            .0
    }

    fn vbap_with(layout: &SpeakerLayout, mode: OutOfHullMode, negative_z: bool) -> VbapBackend {
        let panner = VbapPanner::new(&dirs(layout), 5, 5, 0.0, mode)
            .expect("triangulation")
            .with_negative_z(negative_z);
        VbapBackend::new(panner, VbapSpreadParams::default())
    }

    fn vbap(layout: &SpeakerLayout) -> VbapBackend {
        vbap_with(layout, OutOfHullMode::default(), true)
    }

    fn volumetric_with(
        layout: &SpeakerLayout,
        params: VolumetricParams,
        mode: OutOfHullMode,
        negative_z: bool,
    ) -> VolumetricBackend {
        let room = RoomRatios::UNIT;
        let radii = layout.spatializable_radii_for_room(
            room.ratio,
            room.rear,
            room.lower,
            room.center_blend,
        );
        VolumetricBackend::new(
            vbap_with(layout, mode, negative_z),
            &dirs(layout),
            &radii,
            params,
        )
        .expect("volumetric backend")
    }

    fn volumetric(layout: &SpeakerLayout, params: VolumetricParams) -> VolumetricBackend {
        volumetric_with(layout, params, OutOfHullMode::default(), true)
    }

    fn l2_step(a: &[f32], b: &[f32]) -> f32 {
        a.iter()
            .zip(b.iter())
            .map(|(x, y)| (x - y) * (x - y))
            .sum::<f32>()
            .sqrt()
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

    fn rms(gains: &[f32]) -> f32 {
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
                let expected = plain.gains_at(&req);
                let got = model.gains_at(&req);
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
                let gains = model.gains_at(&request(position));
                let mirrored = model.gains_at(&request([-position[0], position[1], position[2]]));
                for (index, &gain) in gains.iter().enumerate() {
                    let twin = if index == l {
                        r
                    } else if index == r {
                        l
                    } else {
                        index
                    };
                    assert!(
                        (gain - mirrored[twin]).abs() < 1e-5,
                        "{position:?} with {central:?}: speaker {index} {gain} vs its mirror {}",
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
                    let gains = model.gains_at(&request(wall.map(|v| v * s)));
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
        let at_listener = model.gains_at(&request([0.0, 0.0, 0.0]));
        for gain in at_listener.iter() {
            assert!((gain - uniform).abs() < 1e-6, "{gain} vs {uniform}");
        }
        let nearly = model.gains_at(&request([-0.0006, 0.001, 0.0003]));
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
        let gains = model.gains_at(&request([0.0, 0.5, 0.0]));
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
        let gains = model.gains_at(&request([0.0, 1e-4, 0.0]));
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
                let gains = model.gains_at(&req);
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
                let gains = model.gains_at(&req);
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
        let err = VolumetricBackend::new(
            vbap(&layout),
            &dirs(&layout),
            &[1.0; 3],
            VolumetricParams::default(),
        )
        .err()
        .expect("three distances for eleven loudspeakers");
        assert!(err.to_string().contains("3 distances"), "{err}");
    }

    /// Review finding: with the VBAP underneath in a mode that leaves the
    /// hull open (`fade`, `blend`), a direction dipping below the horizon
    /// used to meet no surface at all, and the depth dropped to zero in one
    /// step. The surface is closed with the poles whatever the mode.
    #[test]
    fn the_depth_surface_is_closed_whatever_the_out_of_hull_mode() {
        let layout = layout_714();
        for mode in [
            OutOfHullMode::Fade,
            OutOfHullMode::Blend {
                power: OutOfHullMode::DEFAULT_BLEND_POWER,
            },
            OutOfHullMode::VirtualPoles,
        ] {
            let model = volumetric_with(
                &layout,
                with_central(CentralDistribution::Antipode),
                mode,
                true,
            );
            let plain = vbap_with(&layout, mode, true);
            let just_below = model.depth(&request([-0.3, 0.5, -0.0005]));
            assert!(
                (just_below - 0.5).abs() < 0.01,
                "{mode:?}: depth just below the horizon {just_below}, expected about 0.5"
            );
            // The step the volumetric gains take is bounded by the steps the
            // VBAP underneath takes on its own, at the object and at its
            // antipode (its fold is what it is): the depth and the antipode
            // add none of their own.
            let mut previous: Option<(Vec<f32>, Vec<f32>, Vec<f32>)> = None;
            for step in 0..=40 {
                let z = 0.01 - step as f64 * 0.0005;
                let req = request([-0.3, 0.5, z]);
                let gains = model.gains_at(&req);
                let reference = plain.gains_at(&req);
                let opposite = plain.gains_at(&request([0.3, -0.5, -z]));
                if let Some((previous, previous_reference, previous_opposite)) = &previous {
                    let jump = l2_step(&gains, previous);
                    let own = l2_step(&reference, previous_reference);
                    let across = l2_step(&opposite, previous_opposite);
                    assert!(
                        jump <= own + across + 0.01,
                        "{mode:?}: gains stepped by {jump} at z = {z}, VBAP alone steps by \
                         {own} there and {across} at the antipode"
                    );
                }
                previous = Some((gains, reference, opposite));
            }
        }
    }

    /// Review finding: the antipode of an object overhead is under the
    /// floor, even when the panner clamps *positions* to the horizon.
    #[test]
    fn the_antipode_is_the_opposite_direction_even_with_the_horizon_clamp() {
        let layout = octahedron();
        let (c, t, d) = (
            index_of(&layout, "C"),
            index_of(&layout, "T"),
            index_of(&layout, "D"),
        );
        let model = volumetric_with(
            &layout,
            with_central(CentralDistribution::Antipode),
            OutOfHullMode::default(),
            false,
        );
        let gains = model.gains_at(&request([0.0, 0.0, 0.5]));
        assert!(
            (gains[t] * gains[t] - 0.75).abs() < 1e-3,
            "top {}",
            gains[t]
        );
        assert!(
            (gains[d] * gains[d] - 0.25).abs() < 1e-3,
            "bottom {}",
            gains[d]
        );
        assert!(
            gains[c].abs() < 1e-4,
            "front should be silent, got {}",
            gains[c]
        );
    }

    /// Review finding: the rounding band used to cut the depth *before* the
    /// curve, so a curve below one turned a negligible linear depth into a
    /// large gain that vanished in one step. The band is a ramp, and the
    /// curve bends what comes out of it.
    #[test]
    fn the_rounding_band_stays_continuous_under_a_bent_curve() {
        let layout = octahedron();
        let b = index_of(&layout, "B");
        let model = volumetric(
            &layout,
            VolumetricParams {
                central: CentralDistribution::Antipode,
                depth_curve: 0.25,
            },
        );
        let inside = model.gains_at(&request([0.0, 0.999989, 0.0]));
        let nearer = model.gains_at(&request([0.0, 0.999991, 0.0]));
        let jump = l2_step(&inside, &nearer);
        assert!(jump < 0.02, "gains stepped by {jump} across two microns");
        // Past the fade-in band the curve lifts a small depth as it should:
        // a thousandth of the way in, the opposite wall is well audible.
        let lifted = model.gains_at(&request([0.0, 0.999, 0.0]));
        assert!(
            lifted[b] > 0.25,
            "a quarter-root curve lifts a small depth: back {}",
            lifted[b]
        );
    }

    /// Review finding: the ramp is continuous, but `f32` leaves one quantum
    /// of position between zero and the first depth past the band, and a
    /// curve below one made that quantum a step of 0.07. The central share
    /// fades in over the first `FADE_IN` of depth, so no quantum steps.
    #[test]
    fn the_central_share_fades_in_within_float_precision() {
        let layout = octahedron();
        let model = volumetric(
            &layout,
            VolumetricParams {
                central: CentralDistribution::Antipode,
                depth_curve: 0.25,
            },
        );
        let before = model.gains_at(&request([0.0, 0.99999896, 0.0]));
        let after = model.gains_at(&request([0.0, 0.99999902, 0.0]));
        let jump = l2_step(&before, &after);
        assert!(
            jump < 1e-3,
            "gains stepped by {jump} across one float quantum"
        );

        // Every quantum from the wall to past the band, and the law beyond.
        let mut previous: Option<Vec<f32>> = None;
        let mut largest = 0.0f32;
        for step in 0..=3000 {
            let y = 1.0 - step as f64 * 1e-7;
            let gains = model.gains_at(&request([0.0, y, 0.0]));
            if let Some(previous) = &previous {
                largest = largest.max(l2_step(&gains, previous));
            }
            previous = Some(gains);
        }
        assert!(
            largest < 2e-3,
            "largest step over the first 3e-4 of depth: {largest}"
        );
    }

    /// Review finding: a `fade` VBAP attenuates a direction outside its hull,
    /// and renormalising the blend to unit power threw that away as soon as
    /// the object came inside the (closed) surface. The level is now the
    /// blend's own: the VBAP's at the surface, the central distribution's
    /// at the listener.
    #[test]
    fn the_fade_level_is_kept_at_the_surface() {
        let layout = layout_714();
        let plain = vbap_with(&layout, OutOfHullMode::Fade, true);
        let ray = [0.2, 0.3, -0.5];
        for central in [CentralDistribution::Uniform, CentralDistribution::Antipode] {
            let model = volumetric_with(&layout, with_central(central), OutOfHullMode::Fade, true);
            // Outside, VBAP attenuates this direction; a step inside keeps
            // that level.
            let outside = rms(&plain.gains_at(&request(ray.map(|v| v * 1.45))));
            assert!(
                outside < 0.9,
                "fade attenuates below the hull: rms {outside}"
            );
            let inside = rms(&model.gains_at(&request(ray.map(|v| v * 1.40))));
            assert!(
                (inside - outside).abs() < 0.02,
                "{central:?}: rms {inside} just inside vs {outside} just outside"
            );
            // And the level moves continuously all the way to the listener.
            let mut previous: Option<f32> = None;
            for step in 0..=310 {
                let s = 1.6 - step as f64 * 0.005;
                let level = rms(&model.gains_at(&request(ray.map(|v| v * s))));
                if let Some(previous) = previous {
                    assert!(
                        (level - previous).abs() < 0.01,
                        "{central:?}: rms stepped from {previous} to {level} at {s}"
                    );
                }
                previous = Some(level);
            }
        }
    }

    /// Objects authored on the walls and the ceiling, all around: every one
    /// reads a depth of exactly zero, so the rounding band covers what the
    /// plane arithmetic leaves.
    #[test]
    fn every_wall_point_reads_zero_depth() {
        let model = volumetric(&layout_714(), VolumetricParams::default());
        for az_step in 0..36 {
            let az = (az_step as f64 * 10.0 + 3.0).to_radians();
            for z in [0.0, 0.5, 1.0] {
                // On the wall: scaled so the larger horizontal coordinate is 1;
                // on the ceiling: z = 1 and the point anywhere inside it.
                let (x, y) = (az.sin(), az.cos());
                let position = if z < 1.0 {
                    let scale = 1.0 / x.abs().max(y.abs());
                    [x * scale, y * scale, z]
                } else {
                    [x * 0.6, y * 0.6, 1.0]
                };
                let depth = model.depth(&request(position));
                assert_eq!(depth, 0.0, "{position:?} is on the surface");
            }
        }
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
