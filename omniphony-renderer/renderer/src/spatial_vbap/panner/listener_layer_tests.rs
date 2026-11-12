//! The listener-layer modes (`vbap_native::ListenerLayerMode`) on layouts
//! whose ear-height loudspeakers are off the horizon.
//!
//! Directions are in the layout convention: azimuth negative to the left, so
//! the film's L channel is (−30°, 0°).

use dsp_fixtures::dirs::fibonacci_sphere;

use super::native_backend::NativeVbapLayout;
use super::native_validation::{LATTICE_POINTS, MAX_SEAM_JUMP, residual_jump};
use crate::spatial_vbap::vbap_native::{ListenerLayer, ListenerLayerMode, OutOfHullMode};

const L: usize = 0;
const R: usize = 1;
const C: usize = 2;
const LS: usize = 3;
const RS: usize = 4;
const TFL: usize = 5;
const TFR: usize = 6;
const TBL: usize = 7;
const TBR: usize = 8;
const NAMES: [&str; 9] = ["L", "R", "C", "Ls", "Rs", "TFL", "TFR", "TBL", "TBR"];

/// A 5.1.4 as homes have it: L and R beside the screen, 20° up; C at
/// `c_elevation`; the surrounds at ±110° at `surround_elevation`; the tops
/// at ±45° and ±135°, 45° up.
fn home_514(c_elevation: f32, surround_elevation: f32) -> [[f32; 2]; 9] {
    [
        [-30.0, 20.0],
        [30.0, 20.0],
        [0.0, c_elevation],
        [-110.0, surround_elevation],
        [110.0, surround_elevation],
        [-45.0, 45.0],
        [45.0, 45.0],
        [-135.0, 45.0],
        [135.0, 45.0],
    ]
}

/// L, R, C and the surrounds are on the listener layer, the tops are not.
fn listener(mode: ListenerLayerMode, n: usize) -> ListenerLayer {
    ListenerLayer {
        mode,
        members: (0..n).map(|i| i < TFL).collect(),
    }
}

fn panner(dirs: &[[f32; 2]], mode: ListenerLayerMode) -> NativeVbapLayout {
    panner_in(dirs, mode, OutOfHullMode::VirtualPoles)
}

fn panner_in(
    dirs: &[[f32; 2]],
    mode: ListenerLayerMode,
    out_of_hull: OutOfHullMode,
) -> NativeVbapLayout {
    NativeVbapLayout::from_speaker_dirs_with_listener_layer(
        dirs,
        out_of_hull,
        &listener(mode, dirs.len()),
    )
    .expect("triangulation")
}

fn gains(panner: &NativeVbapLayout, az: f32, el: f32) -> Vec<f32> {
    panner.vbap_gains(az, el, 0.0).expect("gains")
}

fn power(g: &[f32]) -> f32 {
    g.iter().map(|g| g * g).sum()
}

/// Gains above this count as playing.
const PLAYING: f32 = 1e-3;

/// The loudspeakers that play, by name, for a failure message.
fn playing(g: &[f32]) -> Vec<String> {
    g.iter()
        .zip(NAMES)
        .filter(|(g, _)| **g > PLAYING)
        .map(|(g, name)| format!("{name}={g:.3}"))
        .collect()
}

/// Exactly `speakers` play, the gains power-normalised.
fn assert_only(g: &[f32], speakers: &[usize], at: &str) {
    for (i, gain) in g.iter().enumerate() {
        assert_eq!(
            *gain > PLAYING,
            speakers.contains(&i),
            "{at}: expected {:?} alone, got {:?}",
            speakers.iter().map(|&i| NAMES[i]).collect::<Vec<_>>(),
            playing(g)
        );
    }
    assert!((power(g) - 1.0).abs() < 1e-4, "{at}: power {}", power(g));
}

fn bits(g: &[f32]) -> Vec<u32> {
    g.iter().map(|g| g.to_bits()).collect()
}

/// The hole the mode is for: with L 20° up, classic VBAP plays the film's L
/// channel on C and the left surround and leaves L silent. With the virtual
/// poles (the default) and C on the horizon, the nadir pole reaches up to L
/// instead and spreads a share over the whole listener layer; with C a
/// little lower, the hole is back.
#[test]
fn as_placed_drops_the_l_channel_into_the_hole() {
    let blend = OutOfHullMode::Blend {
        power: OutOfHullMode::DEFAULT_BLEND_POWER,
    };
    let g = gains(
        &panner_in(&home_514(0.0, 0.0), ListenerLayerMode::AsPlaced, blend),
        -30.0,
        0.0,
    );
    assert!(
        g[L] <= PLAYING && g[C] > 0.8 && g[LS] > 0.4,
        "{:?}",
        playing(&g)
    );

    let g = gains(
        &panner(&home_514(0.0, 0.0), ListenerLayerMode::AsPlaced),
        -30.0,
        0.0,
    );
    assert!(g[R] > 0.1 && g[RS] > 0.1, "{:?}", playing(&g));

    let g = gains(
        &panner(&home_514(-10.0, 0.0), ListenerLayerMode::AsPlaced),
        -30.0,
        0.0,
    );
    assert!(g[C] > g[L] && g[LS] > 0.3, "{:?}", playing(&g));
}

#[test]
fn shadows_put_each_channel_on_its_loudspeaker() {
    let p = panner(&home_514(0.0, 0.0), ListenerLayerMode::Shadow);
    for side in [1.0, -1.0] {
        let (front, surround) = if side > 0.0 { (R, RS) } else { (L, LS) };
        let at = |az: f32, el: f32| (side * az, el, format!("({}, {el})", side * az));
        for (az, el, speakers) in [
            (30.0, 0.0, vec![front]),
            (0.0, 0.0, vec![C]),
            (15.0, 0.0, vec![front, C]),
            (60.0, 0.0, vec![front, surround]),
            (30.0, 10.0, vec![front]),
            (110.0, 0.0, vec![surround]),
        ] {
            let (az, el, label) = at(az, el);
            assert_only(&gains(&p, az, el), &speakers, &label);
        }
    }
}

/// Between L and the top above it, the image moves from where L really is:
/// an object 10° above the film's L is nearer L than anything else and
/// stays on it, and at 30° L still leads. Flattening L to 0° warps that
/// space and lets the top in early.
#[test]
fn the_heights_blend_from_where_the_loudspeakers_stand() {
    let dirs = home_514(0.0, 0.0);
    let shadow = panner(&dirs, ListenerLayerMode::Shadow);
    let g = gains(&shadow, -30.0, 30.0);
    let top_share = g[TFL] * g[TFL] / power(&g);
    assert!(g[L] > g[TFL] && top_share < 0.5, "{:?}", playing(&g));
    assert!(
        g[L] * g[L] + g[TFL] * g[TFL] > 0.95,
        "L and TFL carry the image: {:?}",
        playing(&g)
    );
    for quiet in [C, LS, RS, TBL, TBR] {
        assert!(g[quiet] <= PLAYING, "{}: {:?}", NAMES[quiet], playing(&g));
    }

    let flat = gains(&panner(&dirs, ListenerLayerMode::Flatten), -30.0, 10.0);
    assert!(
        flat[TFL] > 0.1,
        "flattened L lets TFL in: {:?}",
        playing(&flat)
    );
}

/// A centre under the screen pans as if on the horizon in shadow mode, so
/// the dialogue stays on it; as placed, it leaks to L and R.
#[test]
fn a_centre_under_the_screen_pans_at_ear_level() {
    let dirs = home_514(-10.0, 0.0);
    assert_only(
        &gains(&panner(&dirs, ListenerLayerMode::Shadow), 0.0, 0.0),
        &[C],
        "(0, 0) with C at -10",
    );
    let placed = gains(&panner(&dirs, ListenerLayerMode::AsPlaced), 0.0, 0.0);
    assert!(placed[L] > 0.1 && placed[R] > 0.1, "{:?}", playing(&placed));
}

/// Shadows only under raised loudspeakers: one under each of the surrounds
/// on the horizon leaned the hull out from the top backs, and a source
/// behind the listener took 0.39 on each top back.
#[test]
fn surrounds_on_the_horizon_keep_the_rear_on_them() {
    let p = panner(&home_514(0.0, 0.0), ListenerLayerMode::Shadow);
    assert_eq!(p.n_listener_virtuals, 2, "a shadow under L and R only");
    assert_only(&gains(&p, 180.0, 0.0), &[LS, RS], "(180, 0)");
}

/// Raised surrounds 140° apart with nothing between: the plane from the top
/// backs down to the floor passes outside their edge, so as placed a source
/// behind the listener plays mostly on the top backs. Shadow mode closes
/// the ring with a virtual loudspeaker between them.
#[test]
fn a_wide_raised_pair_keeps_its_ring_edge() {
    let dirs = home_514(0.0, 15.0);
    let placed = gains(&panner(&dirs, ListenerLayerMode::AsPlaced), 180.0, 0.0);
    assert!(placed[TBL] > placed[LS], "{:?}", playing(&placed));

    let p = panner(&dirs, ListenerLayerMode::Shadow);
    for el in [0.0, 15.0] {
        assert_only(&gains(&p, 180.0, el), &[LS, RS], &format!("(180, {el})"));
    }
    // Off centre, the nearer surround leads.
    let g = gains(&p, -140.0, 0.0);
    assert!(g[LS] > g[RS] && g[TBL] <= PLAYING, "{:?}", playing(&g));
}

/// Under the listener layer nothing reaches the heights, and no virtual
/// loudspeaker's gain is lost: every direction keeps unit power.
#[test]
fn below_the_ring_only_the_listener_layer_plays() {
    for surround_elevation in [0.0, 15.0] {
        let p = panner(
            &home_514(0.0, surround_elevation),
            ListenerLayerMode::Shadow,
        );
        for az in (-180..180).step_by(5).map(|a| a as f32) {
            for el in [-10.0, -30.0, -60.0, -89.0] {
                let g = gains(&p, az, el);
                assert!(
                    (power(&g) - 1.0).abs() < 1e-4,
                    "({az}, {el}): {}",
                    power(&g)
                );
                for top in [TFL, TFR, TBL, TBR] {
                    assert!(g[top] <= PLAYING, "({az}, {el}): {:?}", playing(&g));
                }
            }
        }
    }
}

/// Power-normalised and continuous over the whole sphere, pure and spread,
/// in every out-of-hull mode.
#[test]
fn shadow_gains_keep_unit_power_and_have_no_seam() {
    for surround_elevation in [0.0, 15.0] {
        let dirs = home_514(-10.0, surround_elevation);
        for out_of_hull in [
            OutOfHullMode::VirtualPoles,
            OutOfHullMode::Blend {
                power: OutOfHullMode::DEFAULT_BLEND_POWER,
            },
        ] {
            let p = panner_in(&dirs, ListenerLayerMode::Shadow, out_of_hull);
            for (az, el) in fibonacci_sphere(LATTICE_POINTS) {
                for spread in [0.0, 0.3] {
                    let g = p.vbap_gains(az, el, spread).expect("gains");
                    assert!(
                        (power(&g) - 1.0).abs() < 1e-4,
                        "{out_of_hull:?} spread {spread} ({az}, {el}): {}",
                        power(&g)
                    );
                }
                let (jump, at) = residual_jump(&p, az, el);
                assert!(
                    jump <= MAX_SEAM_JUMP,
                    "{out_of_hull:?}: a jump of {jump} at ({at}, {el})"
                );
            }
        }
    }
}

/// The channels land on their loudspeakers whatever the out-of-hull mode:
/// the shadows fold through the virtual-loudspeaker downmix, not through
/// the per-triangle fold the poles take outside `VirtualPoles`.
#[test]
fn shadows_work_in_every_out_of_hull_mode() {
    let dirs = home_514(0.0, 0.0);
    for out_of_hull in [
        OutOfHullMode::Fade,
        OutOfHullMode::Blend {
            power: OutOfHullMode::DEFAULT_BLEND_POWER,
        },
        OutOfHullMode::VirtualPoles,
    ] {
        let p = panner_in(&dirs, ListenerLayerMode::Shadow, out_of_hull);
        for (az, el, speakers) in [
            (-30.0, 0.0, vec![L]),
            (-30.0, 10.0, vec![L]),
            (-15.0, 0.0, vec![L, C]),
        ] {
            assert_only(
                &gains(&p, az, el),
                &speakers,
                &format!("{out_of_hull:?} ({az}, {el})"),
            );
        }
    }
}

/// Flatten is the workaround it replaces, bit for bit: the listener layer
/// put at 0° in the layout itself.
#[test]
fn flatten_is_the_layout_flattened_by_hand() {
    let dirs = home_514(-10.0, 15.0);
    let mut by_hand = dirs;
    for dir in &mut by_hand[..TFL] {
        dir[1] = 0.0;
    }
    for out_of_hull in [
        OutOfHullMode::Fade,
        OutOfHullMode::Blend { power: 24.0 },
        OutOfHullMode::VirtualPoles,
    ] {
        let flattened = panner_in(&dirs, ListenerLayerMode::Flatten, out_of_hull);
        let placed = panner_in(&by_hand, ListenerLayerMode::AsPlaced, out_of_hull);
        for (az, el) in fibonacci_sphere(LATTICE_POINTS) {
            for spread in [0.0, 0.4] {
                assert_eq!(
                    bits(&flattened.vbap_gains(az, el, spread).unwrap()),
                    bits(&placed.vbap_gains(az, el, spread).unwrap()),
                    "{out_of_hull:?} spread {spread} ({az}, {el})"
                );
            }
        }
    }
}

/// As placed is the panner without a listener layer, whatever the members
/// say; and on a listener layer already on the horizon, whose ring the hull
/// keeps, shadow mode changes nothing either.
#[test]
fn as_placed_and_a_flat_layer_are_classic_vbap() {
    let raised = home_514(-10.0, 15.0);
    let flat_714: [[f32; 2]; 11] = [
        [-30.0, 0.0],
        [30.0, 0.0],
        [0.0, 0.0],
        [-90.0, 0.0],
        [90.0, 0.0],
        [-150.0, 0.0],
        [150.0, 0.0],
        [-45.0, 45.0],
        [45.0, 45.0],
        [-135.0, 45.0],
        [135.0, 45.0],
    ];
    let flat_members = ListenerLayer {
        mode: ListenerLayerMode::Shadow,
        members: (0..11).map(|i| i < 7).collect(),
    };
    for out_of_hull in [OutOfHullMode::Fade, OutOfHullMode::VirtualPoles] {
        let cases = [
            (
                NativeVbapLayout::from_speaker_dirs(&raised, out_of_hull).unwrap(),
                panner_in(&raised, ListenerLayerMode::AsPlaced, out_of_hull),
            ),
            (
                NativeVbapLayout::from_speaker_dirs(&flat_714, out_of_hull).unwrap(),
                NativeVbapLayout::from_speaker_dirs_with_listener_layer(
                    &flat_714,
                    out_of_hull,
                    &flat_members,
                )
                .unwrap(),
            ),
        ];
        for (classic, layered) in &cases {
            assert_eq!(layered.n_listener_virtuals, 0);
            for (az, el) in fibonacci_sphere(LATTICE_POINTS) {
                for spread in [0.0, 0.4] {
                    assert_eq!(
                        bits(&layered.vbap_gains(az, el, spread).unwrap()),
                        bits(&classic.vbap_gains(az, el, spread).unwrap()),
                        "{out_of_hull:?} spread {spread} ({az}, {el})"
                    );
                }
            }
        }
    }
}

/// Two listener-layer loudspeakers stacked on one azimuth share one shadow,
/// the lower one's: two shadows in one direction would leave no hull.
#[test]
fn stacked_loudspeakers_share_one_shadow() {
    let mut dirs = home_514(0.0, 0.0).to_vec();
    // A second centre above the first, also on the listener layer.
    dirs.push([0.0, 25.0]);
    let mut members: Vec<bool> = (0..dirs.len()).map(|i| i < TFL).collect();
    members[dirs.len() - 1] = true;
    dirs[C] = [0.0, 5.0];
    let p = NativeVbapLayout::from_speaker_dirs_with_listener_layer(
        &dirs,
        OutOfHullMode::VirtualPoles,
        &ListenerLayer {
            mode: ListenerLayerMode::Shadow,
            members,
        },
    )
    .expect("triangulation");
    // L, R, and one for the two centres.
    assert_eq!(p.n_listener_virtuals, 3);
    let g = p.vbap_gains(0.0, -20.0, 0.0).unwrap();
    assert!(g[C] > 0.99, "the lower centre owns the shadow: {g:?}");
}
