//! Where a rendered object is heard: the perceived image of an object, from
//! the gains the stage hands its loudspeakers and where the listener's ears
//! are.
//!
//! The stage reports, per object and per crossover band, one gain per
//! loudspeaker. A loudspeaker set calibrated at the listening position
//! delivers every loudspeaker's signal there at the same time and level, so
//! what tells the two ears apart is only their offset from the head's centre:
//! each loudspeaker reaches the near ear a little earlier and louder than the
//! far one, by an amount set by its angle from the median plane. Summing
//! localisation (Blauert) then says:
//!
//! - below about 1.5 kHz the ear follows the interaural time difference, and
//!   for signals coherent to within a fraction of a period the delay of a sum
//!   is the amplitude-weighted mean of the delays. Kuhn's low-frequency law
//!   makes the ITD linear in the sine of the lateral angle, so the lateral cue
//!   is the amplitude-weighted mean of the loudspeakers' lateral sines: the
//!   lateral component of Gerzon's velocity vector `rV = Σ g·u / Σ g`;
//! - above it the ear follows the interaural level difference, which a
//!   first-order head shadow makes linear in the power-weighted mean of the
//!   lateral sines: the lateral component of the energy vector
//!   `rE = Σ g²·u / Σ g²`.
//!
//! The head radius cancels out of both cues (the far-field delay difference
//! and the shadow both scale with it, and the ear inverts the same law), so
//! it is not a parameter here: the ears enter through the head's orientation
//! alone, its right axis. An off-centre listener is out of scope, the
//! calibration assumes the listening position.
//!
//! Either cue only fixes a cone around the interaural axis; the pinnae resolve
//! front/back and up/down, and the best stand-in for them without a HRTF is
//! the energy vector's direction, so the image is the point of the cone
//! nearest to `rE`. `|rE|` (1 for a single loudspeaker, smaller as the power
//! spreads over the array) is the image's focus.
//!
//! Every band has its own image. The object's image is their mix, each band
//! weighted by its energy and by how much of it the ear localises at all:
//! below [`LOCALIZATION_FLOOR_HZ`] a loudspeaker's position is not heard,
//! which is what bass management relies on.
//!
//! Positions are in any frame, the same for the loudspeakers and the head;
//! the listener sits at the origin.

/// The frequency above which the interaural level difference takes over from
/// the interaural time difference (the duplex theory's crossover).
pub const DUPLEX_SPLIT_HZ: f64 = 1500.0;

/// Below this the ear does not localise a loudspeaker, so a band under it
/// carries no image of its own (bass management counts on it).
pub const LOCALIZATION_FLOOR_HZ: f64 = 120.0;

/// The audible range a band is clipped to before its log-frequency shares
/// are measured.
const AUDIBLE_HZ: (f64, f64) = (20.0, 20_000.0);

/// The listener's head: its front and right axes, unit vectors in the frame
/// of the loudspeaker positions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Head {
    pub front: [f32; 3],
    pub right: [f32; 3],
}

/// One perceived image.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Image {
    /// Unit vector from the listener.
    pub direction: [f32; 3],
    /// `|rE|`: 1 when one loudspeaker carries the band, towards 0 as the
    /// power spreads over the array (or as the bands disagree).
    pub focus: f32,
    /// Power-weighted mean distance of the loudspeakers carrying it, so the
    /// image sits on the array.
    pub radius: f32,
}

impl Image {
    /// The image's point: its direction at its radius.
    pub fn point(&self) -> [f32; 3] {
        scale(self.direction, self.radius)
    }
}

/// One crossover band of an object: its gains per loudspeaker, its range,
/// and its level when the meters report one.
#[derive(Clone, Copy, Debug)]
pub struct Band<'a> {
    pub gains: &'a [f64],
    /// Low and high edges in hertz; `0` and `f64::INFINITY` are fine.
    pub hz: (f64, f64),
    pub rms_dbfs: Option<f64>,
}

/// The image of one band: the duplex blend of the velocity and energy
/// vectors' lateral components, resolved on the cone nearest the energy
/// vector. `None` when nothing carries the band.
pub fn band_image(
    gains: &[f64],
    positions: &[[f32; 3]],
    head: &Head,
    hz: (f64, f64),
) -> Option<Image> {
    let mut velocity = [0.0f32; 3];
    let mut energy = [0.0f32; 3];
    let mut amplitude_sum = 0.0f32;
    let mut power_sum = 0.0f32;
    let mut radius_sum = 0.0f32;
    for (gain, position) in gains.iter().zip(positions) {
        let gain = *gain as f32;
        if gain <= 0.0 {
            continue;
        }
        let distance = norm(*position);
        if distance < 1e-6 {
            continue;
        }
        let direction = scale(*position, 1.0 / distance);
        let power = gain * gain;
        velocity = add(velocity, scale(direction, gain));
        energy = add(energy, scale(direction, power));
        amplitude_sum += gain;
        power_sum += power;
        radius_sum += power * distance;
    }
    if power_sum <= 1e-12 {
        return None;
    }
    let velocity = scale(velocity, 1.0 / amplitude_sum);
    let energy = scale(energy, 1.0 / power_sum);

    let itd = itd_share(hz);
    let lateral =
        (itd * dot(velocity, head.right) + (1.0 - itd) * dot(energy, head.right)).clamp(-1.0, 1.0);
    Some(Image {
        direction: on_cone(lateral, energy, head),
        focus: norm(energy).min(1.0),
        radius: radius_sum / power_sum,
    })
}

/// The point of the cone `u · right = lateral` nearest to `towards`; the
/// head's front breaks the tie when `towards` lies on the interaural axis.
fn on_cone(lateral: f32, towards: [f32; 3], head: &Head) -> [f32; 3] {
    let mut perpendicular = sub(towards, scale(head.right, dot(towards, head.right)));
    if norm(perpendicular) < 1e-6 {
        perpendicular = sub(head.front, scale(head.right, dot(head.front, head.right)));
    }
    let along = norm(perpendicular);
    if along < 1e-6 {
        return scale(head.right, lateral.signum());
    }
    let perpendicular = scale(perpendicular, 1.0 / along);
    add(
        scale(head.right, lateral),
        scale(perpendicular, (1.0 - lateral * lateral).max(0.0).sqrt()),
    )
}

/// The share of a band the interaural time difference localises: its
/// log-frequency extent below [`DUPLEX_SPLIT_HZ`].
pub fn itd_share(hz: (f64, f64)) -> f32 {
    log_share_below(hz, DUPLEX_SPLIT_HZ)
}

/// The share of a band the ear localises at all: its log-frequency extent
/// above [`LOCALIZATION_FLOOR_HZ`].
pub fn localisable_share(hz: (f64, f64)) -> f32 {
    1.0 - log_share_below(hz, LOCALIZATION_FLOOR_HZ)
}

/// The log-frequency share of `hz`, clipped to the audible range, that lies
/// below `split`. A band without extent counts as the one frequency it is.
fn log_share_below(hz: (f64, f64), split: f64) -> f32 {
    let low = hz.0.max(AUDIBLE_HZ.0);
    let high = hz.1.min(AUDIBLE_HZ.1);
    if high <= low {
        return if low < split { 1.0 } else { 0.0 };
    }
    if split <= low {
        return 0.0;
    }
    if split >= high {
        return 1.0;
    }
    ((split / low).ln() / (high / low).ln()).clamp(0.0, 1.0) as f32
}

/// The object's image over every band: each band's image, weighted by the
/// band's energy and by how much of it the ear localises. Bands that
/// disagree pull the focus down. Levels missing from the meters count as
/// full scale, so bands without a level still weigh in.
pub fn object_image(bands: &[Band<'_>], positions: &[[f32; 3]], head: &Head) -> Option<Image> {
    let images: Vec<(Image, f32, f32)> = bands
        .iter()
        .filter_map(|band| {
            let image = band_image(band.gains, positions, head, band.hz)?;
            let energy = band
                .rms_dbfs
                .map_or(1.0, |db| 10f64.powf(db / 10.0).clamp(0.0, 1.0) as f32);
            Some((image, energy, localisable_share(band.hz)))
        })
        .collect();
    // A rumble alone still plays somewhere: without a localisable band, the
    // energy weights stand on their own.
    let localisable = images.iter().any(|(_, energy, share)| energy * share > 0.0);
    combine(
        images.iter().map(|(image, energy, share)| {
            (*image, if localisable { energy * share } else { *energy })
        }),
    )
}

/// The weighted mix of several images: the mean direction, the mean focus
/// scaled by how well the directions agree, the mean radius.
pub fn combine(images: impl Iterator<Item = (Image, f32)>) -> Option<Image> {
    let mut direction = [0.0f32; 3];
    let mut focus = 0.0f32;
    let mut radius = 0.0f32;
    let mut weight_sum = 0.0f32;
    for (image, weight) in images {
        if weight <= 0.0 {
            continue;
        }
        direction = add(direction, scale(image.direction, weight));
        focus += image.focus * weight;
        radius += image.radius * weight;
        weight_sum += weight;
    }
    if weight_sum <= 1e-12 {
        return None;
    }
    let agreement = norm(direction) / weight_sum;
    if agreement < 1e-6 {
        return None;
    }
    Some(Image {
        direction: scale(direction, 1.0 / norm(direction)),
        focus: (focus / weight_sum * agreement).min(1.0),
        radius: radius / weight_sum,
    })
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale(a: [f32; 3], k: f32) -> [f32; 3] {
    [a[0] * k, a[1] * k, a[2] * k]
}

fn norm(a: [f32; 3]) -> f32 {
    dot(a, a).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Scene frame: X front, Y up, Z right.
    const FRONT: [f32; 3] = [1.0, 0.0, 0.0];
    const RIGHT: [f32; 3] = [0.0, 0.0, 1.0];
    const FACING_FRONT: Head = Head {
        front: FRONT,
        right: RIGHT,
    };
    const FULL_RANGE: (f64, f64) = (0.0, f64::INFINITY);

    fn close(a: [f32; 3], b: [f32; 3]) -> bool {
        norm(sub(a, b)) < 1e-4
    }

    /// Loudspeakers at ±30° on the horizon, 2 m away.
    fn pair() -> Vec<[f32; 3]> {
        let (s, c) = 30f32.to_radians().sin_cos();
        vec![[2.0 * c, 0.0, -2.0 * s], [2.0 * c, 0.0, 2.0 * s]]
    }

    #[test]
    fn one_loudspeaker_is_heard_where_it_stands() {
        let image = band_image(&[0.0, 0.8], &pair(), &FACING_FRONT, FULL_RANGE).unwrap();
        assert!(close(image.direction, scale(pair()[1], 0.5)), "{image:?}");
        assert!((image.focus - 1.0).abs() < 1e-5);
        assert!((image.radius - 2.0).abs() < 1e-5);
    }

    #[test]
    fn an_equal_pair_images_between_them_with_its_spread() {
        let g = std::f64::consts::FRAC_1_SQRT_2;
        let image = band_image(&[g, g], &pair(), &FACING_FRONT, FULL_RANGE).unwrap();
        assert!(close(image.direction, FRONT), "{image:?}");
        assert!((image.focus - 30f32.to_radians().cos()).abs() < 1e-5);
    }

    #[test]
    fn the_lateral_cue_follows_the_head_not_the_room() {
        // The head turned to face right: both loudspeakers are now 60° to its
        // left, with the same ITD, so the image is lateral at 60° and sits in
        // the front half of the cone, where nothing else tells.
        let facing_right = Head {
            front: RIGHT,
            right: [-1.0, 0.0, 0.0],
        };
        let g = std::f64::consts::FRAC_1_SQRT_2;
        let image = band_image(&[g, g], &pair(), &facing_right, FULL_RANGE).unwrap();
        let lateral = dot(image.direction, facing_right.right);
        assert!(
            (lateral + 30f32.to_radians().cos()).abs() < 1e-5,
            "{image:?}"
        );
        assert!(dot(image.direction, facing_right.front) > 0.0, "{image:?}");
    }

    #[test]
    fn the_antipode_pair_images_on_the_near_wall_half_focused() {
        // The volumetric backend's halfway object: three quarters of the
        // power ahead, one quarter behind. A position centroid would land on
        // the object; the image is the front wall, half focused.
        let positions = [[2.0, 0.0, 0.0], [-2.0, 0.0, 0.0]];
        let gains = [0.75f64.sqrt(), 0.25f64.sqrt()];
        let image = band_image(&gains, &positions, &FACING_FRONT, FULL_RANGE).unwrap();
        assert!(close(image.direction, FRONT), "{image:?}");
        assert!((image.focus - 0.5).abs() < 1e-5, "{image:?}");
        assert!((image.radius - 2.0).abs() < 1e-5);
    }

    #[test]
    fn a_source_on_the_interaural_axis_stays_there() {
        let positions = [[0.0, 0.0, 2.0]];
        let image = band_image(&[1.0], &positions, &FACING_FRONT, FULL_RANGE).unwrap();
        assert!(close(image.direction, RIGHT), "{image:?}");
    }

    #[test]
    fn duplex_and_localisation_shares_follow_the_band_edges() {
        assert_eq!(itd_share((0.0, 110.0)), 1.0);
        assert_eq!(itd_share((3000.0, f64::INFINITY)), 0.0);
        let main = itd_share((110.0, f64::INFINITY));
        let expected = ((1500f64 / 110.0).ln() / (20_000f64 / 110.0).ln()) as f32;
        assert!((main - expected).abs() < 1e-6, "{main} vs {expected}");

        assert_eq!(localisable_share((0.0, 110.0)), 0.0);
        assert_eq!(localisable_share((200.0, 8000.0)), 1.0);
        let full = localisable_share(FULL_RANGE);
        let expected = 1.0 - ((120f64 / 20.0).ln() / (20_000f64 / 20.0).ln()) as f32;
        assert!((full - expected).abs() < 1e-6, "{full} vs {expected}");
    }

    #[test]
    fn the_bass_band_does_not_drag_the_image_to_the_subwoofer() {
        // Bass-managed layout: the sub behind, the pair ahead. The bass band
        // feeds the sub alone and is louder than the main band, yet the
        // object images between the pair.
        let mut positions = pair();
        positions.push([-1.0, -0.5, 0.0]);
        let g = std::f64::consts::FRAC_1_SQRT_2;
        let bands = [
            Band {
                gains: &[0.0, 0.0, 1.0],
                hz: (0.0, 110.0),
                rms_dbfs: Some(-6.0),
            },
            Band {
                gains: &[g, g, 0.0],
                hz: (110.0, f64::INFINITY),
                rms_dbfs: Some(-20.0),
            },
        ];
        let image = object_image(&bands, &positions, &FACING_FRONT).unwrap();
        assert!(close(image.direction, FRONT), "{image:?}");
    }

    #[test]
    fn a_rumble_alone_still_images_at_the_subwoofer() {
        let positions = [[-1.0, -0.5, 0.0]];
        let bands = [Band {
            gains: &[1.0],
            hz: (0.0, 110.0),
            rms_dbfs: Some(-6.0),
        }];
        let image = object_image(&bands, &positions, &FACING_FRONT).unwrap();
        assert!(close(
            image.direction,
            scale(positions[0], 1.0 / norm(positions[0]))
        ));
    }

    #[test]
    fn bands_that_disagree_blur_the_image() {
        let front = Image {
            direction: FRONT,
            focus: 1.0,
            radius: 2.0,
        };
        let right = Image {
            direction: RIGHT,
            focus: 1.0,
            radius: 2.0,
        };
        let image = combine([(front, 1.0), (right, 1.0)].into_iter()).unwrap();
        let bisector = scale(add(FRONT, RIGHT), std::f32::consts::FRAC_1_SQRT_2);
        assert!(close(image.direction, bisector), "{image:?}");
        assert!((image.focus - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-5);
        assert!((image.radius - 2.0).abs() < 1e-5);
    }

    #[test]
    fn silence_has_no_image() {
        assert!(band_image(&[0.0, 0.0], &pair(), &FACING_FRONT, FULL_RANGE).is_none());
        assert!(object_image(&[], &pair(), &FACING_FRONT).is_none());
        assert!(combine(std::iter::empty()).is_none());
    }
}
