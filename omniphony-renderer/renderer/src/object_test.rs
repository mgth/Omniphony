//! Signal source for the object test: the mono block that gets panned.
//!
//! Split out from the injection sites because there are two of them — the
//! speaker path pans the block through the active backend's gains, the binaural
//! path feeds it to an HRIR pair — and every property that makes the test
//! trustworthy is a property of the *signal*, not of either renderer: the peak
//! bound, the safety cap, and above all when the noise restarts. Generating it
//! in one place is what stops the two paths from quietly disagreeing about what
//! a level means or about whether a move should click.

use crate::live_params::{ObjectTest, ObjectTestRotation};
use crate::speaker_test::PinkNoise;

/// Cap on a single uninterrupted test, in seconds. Same reasoning as the
/// speaker test's: Studio owns the trigger policy, so the renderer's only job
/// is to make sure a client that dies mid-test cannot leave the room making
/// noise forever.
const MAX_SECONDS: u64 = 120;

/// What the object test is doing for one block: the signal, and where it is.
///
/// The two travel together because they must agree. The speaker path pans this
/// block and the binaural path gives it an HRIR pair; if each worked out the
/// orbit position for itself they could disagree by a block, and in cascaded
/// binaural — where both run — the source would be in two places at once.
pub struct ObjectTestBlock<'a> {
    pub pcm: &'a [f32],
    /// Where the source is for this block: the placed position with the orbit
    /// applied and the room clamp enforced.
    pub position: [f32; 3],
}

/// Pink noise for the object test, level-bounded and cap-limited.
pub struct ObjectTestSource {
    noise: PinkNoise,
    /// What the running test is, for restart detection. Deliberately excludes
    /// position — see [`Self::identity_of`].
    identity: Option<(u32, u8)>,
    /// Last position seen, to spot a move. Only the safety cap cares: a move
    /// must not touch the generator.
    last_position: Option<[f32; 3]>,
    elapsed_samples: u64,
    /// Orbit phase in turns, wrapped to `[0, 1)`.
    ///
    /// **Accumulated, not derived from elapsed time.** Working the angle out as
    /// `elapsed / period` makes it a function of the period, so moving the speed
    /// control teleports the source: at ten seconds in, going from a 4 s turn to
    /// a 2 s one recomputes the phase from 0.5 turns to 0.0 and the source jumps
    /// across the room. Advancing the phase by a step each block instead means
    /// the period only ever affects where the source goes *next*, which is what
    /// a speed control is.
    ///
    /// Reset only when the test itself restarts — deliberately NOT when the
    /// object is moved, so dragging the centre of a running orbit slides the
    /// circle instead of snapping the source back to the start of its turn.
    rotation_phase: f64,
    /// Reused block buffer. Sized once per block; capacity is retained across
    /// blocks, so steady state allocates nothing.
    block: Vec<f32>,
}

impl Default for ObjectTestSource {
    fn default() -> Self {
        Self {
            // A different seed from the speaker test's generator: when both run
            // at once, two identical noise streams would correlate and comb
            // rather than sound like two independent sources.
            noise: PinkNoise::new(0x85EB_CA6B),
            identity: None,
            last_position: None,
            elapsed_samples: 0,
            rotation_phase: 0.0,
            block: Vec::new(),
        }
    }
}

impl ObjectTestSource {
    /// What counts as "a different test", and so restarts the generator.
    ///
    /// **Position is not part of it, by design.** Moving the object is the
    /// tool's entire purpose: keying identity on position would reset the
    /// filter state on every drag, so a listener dragging a source would hear a
    /// string of clicks instead of a source moving. Level and isolation do
    /// restart, matching the speaker test — nudging those is a deliberate "try
    /// again from the top", and the ear expects the clock to restart with it.
    fn identity_of(test: &ObjectTest) -> (u32, u8) {
        (test.level.to_bits(), test.isolation as u8)
    }

    /// Produce this block's noise, or `None` when nothing should be heard.
    ///
    /// `None` means the caller must inject nothing at all: no test requested, or
    /// the safety cap has expired. The returned slice is shorter than `frames`
    /// only when the cap runs out mid-block.
    ///
    /// The samples are already level-scaled and peak-bounded, so a caller only
    /// has to place them. Scaling by `1/CREST` puts the typical peak on the
    /// requested level and the clamp makes that ceiling exact — the same two
    /// steps the speaker test takes, for the same reason (see
    /// [`PinkNoise::CREST`]).
    pub fn next_block(
        &mut self,
        test: Option<ObjectTest>,
        rotation: ObjectTestRotation,
        sample_rate: u32,
        frames: usize,
    ) -> Option<ObjectTestBlock<'_>> {
        let Some(test) = test else {
            // Idle: drop the state so the next test starts clean rather than
            // spliced onto the tail of the last one.
            if self.identity.is_some() {
                self.identity = None;
                self.last_position = None;
                self.elapsed_samples = 0;
                self.rotation_phase = 0.0;
                self.noise.reset();
            }
            return None;
        };

        let identity = Self::identity_of(&test);
        if self.identity != Some(identity) {
            self.identity = Some(identity);
            self.elapsed_samples = 0;
            self.rotation_phase = 0.0;
            self.noise.reset();
        }

        // A move refreshes the cap without touching the generator.
        //
        // The cap is there to catch an *abandoned* test — a client that died
        // with the noise running. A position update is proof the client is
        // alive and someone is listening, so it should buy more time; without
        // this, a user still dragging the object around hears it cut out
        // mid-gesture at two minutes, with no way back but toggling it off and
        // on. An abandoned test, by definition, stops moving and still expires.
        //
        // Only the counter resets. Restarting the generator here would
        // reintroduce the very click the design avoids.
        if self.last_position != Some(test.position) {
            self.last_position = Some(test.position);
            self.elapsed_samples = 0;
        }

        let cap = MAX_SECONDS * sample_rate.max(1) as u64;
        if self.elapsed_samples >= cap {
            return None;
        }
        let frames = frames.min((cap - self.elapsed_samples) as usize);
        if frames == 0 {
            return None;
        }

        // Hoisted: one divide per block, not per sample.
        let gain = test.level / PinkNoise::CREST;
        let ceiling = test.level.abs();
        self.block.clear();
        self.block.reserve(frames);
        for _ in 0..frames {
            let raw = self.noise.next_sample();
            self.block.push((raw * gain).clamp(-ceiling, ceiling));
        }

        // Advance the orbit by this block's worth of turn, and sample it at the
        // block's midpoint. Per block rather than per sample because the
        // position feeds a gain evaluation that is itself per block; the
        // midpoint rather than an edge because that is the average the block
        // actually represents — an edge sample biases every block half a step in
        // the direction of travel.
        //
        // The phase carries over from the last block, so the period sets only
        // how fast the source travels from here. Changing it mid-orbit changes
        // the speed and nothing else.
        let rate = sample_rate.max(1) as f64;
        let delta_turns = if rotation.is_active() {
            frames as f64 / (rate * rotation.period_s as f64)
        } else {
            // Frozen while there is no orbit, so turning the diameter back up
            // resumes from where it left off rather than from an angle that
            // drifted on while nothing was moving.
            0.0
        };
        let position = rotation.position_at(
            test.position,
            (self.rotation_phase + delta_turns * 0.5) as f32,
        );
        // Wrapped every block: an unwrapped f64 accumulating for hours would
        // start losing angular resolution to its own magnitude.
        self.rotation_phase = (self.rotation_phase + delta_turns).rem_euclid(1.0);

        self.elapsed_samples += frames as u64;
        Some(ObjectTestBlock {
            pcm: &self.block,
            position,
        })
    }

    /// True while a test is producing signal, for callers that only need to
    /// know whether to suppress peak tracking.
    pub fn is_running(&self) -> bool {
        self.identity.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live_params::TestIsolation;

    /// No orbit: the default a test starts with.
    const OFF: ObjectTestRotation = ObjectTestRotation {
        axis: crate::live_params::RotationAxis::Z,
        radius: 0.0,
        period_s: 4.0,
    };

    fn test_at(position: [f32; 3], level: f32) -> ObjectTest {
        ObjectTest {
            position,
            size: [0.0; 3],
            level,
            isolation: TestIsolation::TestOnly,
        }
    }

    /// The property the whole feature rests on: moving the object must not
    /// disturb the signal by even one sample.
    ///
    /// Asserted by generating one continuous run, then generating the same
    /// number of samples while moving the object every block, and requiring the
    /// two to be *bit-identical*. A reset would show up immediately, because
    /// the filter state that produces sample N depends on every sample before
    /// it — nothing but an untouched generator can reproduce the run exactly.
    #[test]
    fn moving_the_object_does_not_disturb_the_signal() {
        let mut still = ObjectTestSource::default();
        let mut moving = ObjectTestSource::default();

        let mut a = Vec::new();
        for _ in 0..8 {
            a.extend_from_slice(
                still
                    .next_block(Some(test_at([0.0, 1.0, 0.0], 0.5)), OFF, 48_000, 256)
                    .unwrap()
                    .pcm,
            );
        }

        let mut b = Vec::new();
        for i in 0..8 {
            // A different position every block, spanning the room.
            let x = -1.0 + 0.25 * i as f32;
            b.extend_from_slice(
                moving
                    .next_block(Some(test_at([x, 1.0 - x, x * 0.5], 0.5)), OFF, 48_000, 256)
                    .unwrap()
                    .pcm,
            );
        }

        assert_eq!(
            a, b,
            "moving the object perturbed the generator — the noise restarts on \
             a move, which is exactly the click this design exists to avoid"
        );
    }

    /// Changing the level, by contrast, *should* restart the filter: it is a
    /// deliberate "try that again", and the speaker test behaves the same way.
    ///
    /// Asserted against the counterfactual rather than against a fresh
    /// generator, because `PinkNoise::reset` clears the filter poles but
    /// deliberately not the RNG — a restarted run does not replay a new one.
    /// The observable difference is this: the generator is linear in its gain,
    /// so had the level merely been rescaled with the poles left alone, the new
    /// block would be the old one times the level ratio, sample for sample.
    /// Since the poles *are* cleared, it must not be.
    #[test]
    fn changing_the_level_restarts_the_signal() {
        let pos = [0.0, 1.0, 0.0];
        // Reference: same generator, level held constant, so any difference
        // below is attributable to the level change and nothing else.
        let mut held = ObjectTestSource::default();
        let _ = held.next_block(Some(test_at(pos, 0.5)), OFF, 48_000, 64);
        let unchanged = held
            .next_block(Some(test_at(pos, 0.5)), OFF, 48_000, 64)
            .unwrap()
            .pcm
            .to_vec();

        let mut changed = ObjectTestSource::default();
        let _ = changed.next_block(Some(test_at(pos, 0.5)), OFF, 48_000, 64);
        let after = changed
            .next_block(Some(test_at(pos, 0.25)), OFF, 48_000, 64)
            .unwrap()
            .pcm
            .to_vec();

        // What a pure rescale (no reset) would have produced.
        let rescaled: Vec<f32> = unchanged.iter().map(|s| s * 0.5).collect();
        assert_ne!(
            after, rescaled,
            "a level change only rescaled the signal — the filter state was not \
             cleared, so the new run carries the old one's tail"
        );
    }

    /// A full-scale test must not produce a sample past full scale, so that the
    /// panned result cannot clip either: backend gains are power-normalised, so
    /// bounding the mono block bounds every speaker's share of it.
    #[test]
    fn a_full_scale_block_never_exceeds_full_scale() {
        let mut src = ObjectTestSource::default();
        let mut peak = 0.0f32;
        for _ in 0..64 {
            for &s in src
                .next_block(Some(test_at([0.0, 1.0, 0.0], 1.0)), OFF, 48_000, 1024)
                .unwrap()
                .pcm
            {
                peak = peak.max(s.abs());
            }
        }
        assert!(peak > 0.0, "the source produced silence");
        assert!(
            peak <= 1.0,
            "a full-scale object test peaked at {peak} — anything above 1.0 \
             clips once panned"
        );
    }

    /// Moving the object refreshes the cap — and does so without restarting the
    /// signal, which is the pairing that makes it safe.
    ///
    /// Found by running it, not by reading it: a live placement session went
    /// silent mid-drag at two minutes, because position is deliberately kept out
    /// of the identity and so nothing was resetting the clock. A user still
    /// dragging is the clearest possible evidence the client is alive.
    #[test]
    fn moving_the_object_refreshes_the_safety_cap() {
        let mut src = ObjectTestSource::default();
        let sample_rate = 48_000;
        let block = 4800; // 100 ms
        let blocks_to_cap = (MAX_SECONDS * sample_rate as u64) / block as u64;

        // Run past the cap, moving every block as a drag would.
        for i in 0..blocks_to_cap * 2 {
            let x = if i % 2 == 0 { -0.5 } else { 0.5 };
            assert!(
                src.next_block(Some(test_at([x, 1.0, 0.0], 0.5)), OFF, sample_rate, block)
                    .is_some(),
                "a moving object test expired at block {i}, despite the client \
                 demonstrably being alive"
            );
        }

        // Still capped once it stops moving: an abandoned test must expire.
        for _ in 0..blocks_to_cap {
            src.next_block(Some(test_at([0.5, 1.0, 0.0], 0.5)), OFF, sample_rate, block);
        }
        assert!(
            src.next_block(Some(test_at([0.5, 1.0, 0.0], 0.5)), OFF, sample_rate, block)
                .is_none(),
            "a stationary test outlived its cap — the refresh must need a move"
        );
    }

    /// The orbit must turn, stay on its circle, and come back where it began.
    #[test]
    fn the_orbit_traces_a_circle_of_the_requested_diameter() {
        let rot = ObjectTestRotation {
            axis: crate::live_params::RotationAxis::Z,
            radius: 0.5,
            period_s: 4.0,
        };
        let base = [0.0, 0.0, 0.0];
        let r = 0.5;
        for i in 0..64 {
            let phase = i as f32 / 64.0;
            let p = rot.position_at(base, phase);
            // A horizontal orbit: height untouched, radius held.
            assert!((p[2] - base[2]).abs() < 1e-6, "z drifted at phase {phase}");
            let d = ((p[0] - base[0]).powi(2) + (p[1] - base[1]).powi(2)).sqrt();
            assert!(
                (d - r).abs() < 1e-5,
                "radius {d} != {r} at phase {phase} — the orbit is not a circle"
            );
        }
        let start = rot.position_at(base, 0.0);
        let full = rot.position_at(base, 1.0);
        for i in 0..3 {
            assert!(
                (start[i] - full[i]).abs() < 1e-5,
                "a whole turn did not return to the start"
            );
        }
    }

    /// Every axis must orbit in the plane perpendicular to itself — that is what
    /// "rotation about an axis" means, and a wrong pair of plane vectors would
    /// still trace a tidy circle, just the wrong one.
    #[test]
    fn each_axis_orbits_in_its_own_plane() {
        use crate::live_params::RotationAxis;
        let cases = [
            (RotationAxis::X, 0usize),
            (RotationAxis::Y, 1),
            (RotationAxis::Z, 2),
            (
                RotationAxis::Free {
                    azimuth_deg: 0.0,
                    elevation_deg: 90.0,
                },
                2,
            ),
        ];
        for (axis, fixed) in cases {
            let rot = ObjectTestRotation {
                axis,
                radius: 0.5,
                period_s: 4.0,
            };
            let base = [0.0, 0.0, 0.0];
            for i in 0..32 {
                let p = rot.position_at(base, i as f32 / 32.0);
                assert!(
                    p[fixed].abs() < 1e-5,
                    "{:?}: component {fixed} moved to {} — the orbit is in the wrong plane",
                    axis,
                    p[fixed]
                );
            }
        }
    }

    /// The room clamp is the literal reading, so the orbit flattens against a
    /// wall rather than leaving the room or quietly shrinking.
    #[test]
    fn the_orbit_is_clamped_to_the_room() {
        let rot = ObjectTestRotation {
            axis: crate::live_params::RotationAxis::Z,
            radius: 1.0,
            period_s: 4.0,
        };
        // Centred hard right: half the circle wants to be outside the room.
        let base = [0.9, 0.0, 0.0];
        let mut clamped = 0;
        for i in 0..128 {
            let p = rot.position_at(base, i as f32 / 128.0);
            for c in p {
                assert!(c >= -1.0 && c <= 1.0, "left the room at {c}");
            }
            if (p[0] - 1.0).abs() < 1e-6 {
                clamped += 1;
            }
        }
        assert!(
            clamped > 0,
            "nothing was clamped — the test cannot claim the room bound is enforced"
        );
    }

    /// Clamping changes the orbit's shape, not its motion.
    ///
    /// Worth pinning because the docs first claimed the opposite — that the
    /// source "dwells" against the wall — and a live render disproved it. The
    /// clamp is per axis, so the axes that still fit keep sweeping: the circle
    /// becomes a D and the source runs along the wall rather than stopping on
    /// it. A future change that clamped the whole vector at once would bring
    /// the dwell back, and this is what would notice.
    #[test]
    fn a_clamped_orbit_still_moves_the_whole_way_round() {
        let rot = ObjectTestRotation {
            axis: crate::live_params::RotationAxis::Z,
            radius: 1.0,
            period_s: 4.0,
        };
        let base = [0.9, 0.0, 0.0];
        let steps = 720;
        let mut against_wall = 0;
        let mut prev = rot.position_at(base, 0.0);
        let mut slowest = f32::INFINITY;
        for i in 1..=steps {
            let p = rot.position_at(base, i as f32 / steps as f32);
            if (p[0] - 1.0).abs() < 1e-6 {
                against_wall += 1;
            }
            let step = ((p[0] - prev[0]).powi(2) + (p[1] - prev[1]).powi(2)).sqrt();
            slowest = slowest.min(step);
            prev = p;
        }
        assert!(
            against_wall > steps / 4,
            "expected a long run along the wall, got {against_wall}/{steps}"
        );
        assert!(
            slowest > 0.0,
            "the source stopped dead somewhere on the clamped path"
        );
    }

    /// Moving the object while it orbits must slide the circle, not restart the
    /// turn: the phase is kept out of everything a move touches.
    #[test]
    fn moving_the_centre_does_not_reset_the_orbit_phase() {
        let rot = ObjectTestRotation {
            axis: crate::live_params::RotationAxis::Z,
            radius: 0.25,
            period_s: 1.0,
        };
        let sample_rate = 48_000;
        let block = 4800; // 100 ms, so ten blocks make a turn

        let mut still = ObjectTestSource::default();
        let mut moved = ObjectTestSource::default();
        // Advance both a quarter turn.
        for _ in 0..3 {
            still.next_block(Some(test_at([0.0, 0.0, 0.0], 0.5)), rot, sample_rate, block);
            moved.next_block(Some(test_at([0.0, 0.0, 0.0], 0.5)), rot, sample_rate, block);
        }
        // One keeps still; the other has its centre dragged sideways.
        let a = still
            .next_block(Some(test_at([0.0, 0.0, 0.0], 0.5)), rot, sample_rate, block)
            .unwrap()
            .position;
        let b = moved
            .next_block(Some(test_at([0.5, 0.0, 0.0], 0.5)), rot, sample_rate, block)
            .unwrap()
            .position;
        // Same point on the circle, just around a centre 0.5 further right.
        assert!(
            (b[0] - (a[0] + 0.5)).abs() < 1e-5 && (b[1] - a[1]).abs() < 1e-5,
            "dragging the centre moved the phase too: {a:?} vs {b:?}"
        );
    }

    /// Changing the turn time must change the speed and nothing else.
    ///
    /// Reported from use: moving the speed control made the source jump across
    /// the room. The phase was being worked out as `elapsed / period`, which
    /// makes the angle a function of the period — so at ten seconds in, going
    /// from a 4 s turn to a 2 s one recomputed it from half a turn to none.
    /// Accumulating the phase instead leaves the current angle alone.
    #[test]
    fn changing_the_turn_time_does_not_move_the_source() {
        let sample_rate = 48_000;
        let block = 4800; // 100 ms
        let slow = ObjectTestRotation {
            axis: crate::live_params::RotationAxis::Z,
            radius: 0.5,
            period_s: 4.0,
        };
        let fast = ObjectTestRotation {
            period_s: 1.0,
            ..slow
        };
        let base = [0.0, 0.0, 0.0];

        let mut src = ObjectTestSource::default();
        // Run a while at the slow rate so the phase is somewhere awkward.
        let mut last = [0.0f32; 3];
        for _ in 0..17 {
            last = src
                .next_block(Some(test_at(base, 0.5)), slow, sample_rate, block)
                .unwrap()
                .position;
        }
        // Now speed it up. The very next position must continue from where the
        // source was, not from wherever `elapsed / new_period` happens to land.
        let after = src
            .next_block(Some(test_at(base, 0.5)), fast, sample_rate, block)
            .unwrap()
            .position;
        let jump = ((after[0] - last[0]).powi(2) + (after[1] - last[1]).powi(2)).sqrt();
        // One block at the fast rate is 1/10 turn; on a 0.5 radius that is a
        // step of about 0.31. Anything much beyond that is a teleport.
        assert!(
            jump < 0.4,
            "changing the turn time moved the source by {jump} — it jumped"
        );

        // And it really did speed up: the next few blocks must cover more
        // ground than the same number did at the slow rate.
        let mut fast_travel = 0.0f32;
        let mut prev = after;
        for _ in 0..3 {
            let p = src
                .next_block(Some(test_at(base, 0.5)), fast, sample_rate, block)
                .unwrap()
                .position;
            fast_travel += ((p[0] - prev[0]).powi(2) + (p[1] - prev[1]).powi(2)).sqrt();
            prev = p;
        }
        let mut slow_src = ObjectTestSource::default();
        let mut slow_travel = 0.0f32;
        let mut prev = slow_src
            .next_block(Some(test_at(base, 0.5)), slow, sample_rate, block)
            .unwrap()
            .position;
        for _ in 0..3 {
            let p = slow_src
                .next_block(Some(test_at(base, 0.5)), slow, sample_rate, block)
                .unwrap()
                .position;
            slow_travel += ((p[0] - prev[0]).powi(2) + (p[1] - prev[1]).powi(2)).sqrt();
            prev = p;
        }
        assert!(
            fast_travel > slow_travel * 2.0,
            "the faster period covered {fast_travel} against {slow_travel}: the \
             speed did not actually change"
        );
    }

    /// The safety cap must actually stop the signal, so a dead client cannot
    /// leave a source droning in the room.
    #[test]
    fn the_safety_cap_stops_the_test() {
        let mut src = ObjectTestSource::default();
        let sample_rate = 48_000;
        let block = 4800; // 100 ms
        let blocks_to_cap = (MAX_SECONDS * sample_rate as u64) / block as u64;
        for _ in 0..blocks_to_cap {
            assert!(
                src.next_block(Some(test_at([0.0, 1.0, 0.0], 0.5)), OFF, sample_rate, block)
                    .is_some()
            );
        }
        assert!(
            src.next_block(Some(test_at([0.0, 1.0, 0.0], 0.5)), OFF, sample_rate, block)
                .is_none(),
            "the test outlived its {MAX_SECONDS} s cap"
        );
    }
}
