//! Signal source for the object test: the mono block that gets panned.
//!
//! Split out from the injection sites because there are two of them — the
//! speaker path pans the block through the active backend's gains, the binaural
//! path feeds it to an HRIR pair — and every property that makes the test
//! trustworthy is a property of the *signal*, not of either renderer: the peak
//! bound, the safety cap, and above all when the noise restarts. Generating it
//! in one place is what stops the two paths from quietly disagreeing about what
//! a level means or about whether a move should click.

use crate::live_params::ObjectTest;
use crate::speaker_test::PinkNoise;

/// Cap on a single uninterrupted test, in seconds. Same reasoning as the
/// speaker test's: Studio owns the trigger policy, so the renderer's only job
/// is to make sure a client that dies mid-test cannot leave the room making
/// noise forever.
const MAX_SECONDS: u64 = 120;

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
        sample_rate: u32,
        frames: usize,
    ) -> Option<&[f32]> {
        let Some(test) = test else {
            // Idle: drop the state so the next test starts clean rather than
            // spliced onto the tail of the last one.
            if self.identity.is_some() {
                self.identity = None;
                self.last_position = None;
                self.elapsed_samples = 0;
                self.noise.reset();
            }
            return None;
        };

        let identity = Self::identity_of(&test);
        if self.identity != Some(identity) {
            self.identity = Some(identity);
            self.elapsed_samples = 0;
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

        self.elapsed_samples += frames as u64;
        Some(&self.block)
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
                    .next_block(Some(test_at([0.0, 1.0, 0.0], 0.5)), 48_000, 256)
                    .unwrap(),
            );
        }

        let mut b = Vec::new();
        for i in 0..8 {
            // A different position every block, spanning the room.
            let x = -1.0 + 0.25 * i as f32;
            b.extend_from_slice(
                moving
                    .next_block(Some(test_at([x, 1.0 - x, x * 0.5], 0.5)), 48_000, 256)
                    .unwrap(),
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
        let _ = held.next_block(Some(test_at(pos, 0.5)), 48_000, 64);
        let unchanged = held
            .next_block(Some(test_at(pos, 0.5)), 48_000, 64)
            .unwrap()
            .to_vec();

        let mut changed = ObjectTestSource::default();
        let _ = changed.next_block(Some(test_at(pos, 0.5)), 48_000, 64);
        let after = changed
            .next_block(Some(test_at(pos, 0.25)), 48_000, 64)
            .unwrap()
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
                .next_block(Some(test_at([0.0, 1.0, 0.0], 1.0)), 48_000, 1024)
                .unwrap()
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
                src.next_block(Some(test_at([x, 1.0, 0.0], 0.5)), sample_rate, block)
                    .is_some(),
                "a moving object test expired at block {i}, despite the client \
                 demonstrably being alive"
            );
        }

        // Still capped once it stops moving: an abandoned test must expire.
        for _ in 0..blocks_to_cap {
            src.next_block(Some(test_at([0.5, 1.0, 0.0], 0.5)), sample_rate, block);
        }
        assert!(
            src.next_block(Some(test_at([0.5, 1.0, 0.0], 0.5)), sample_rate, block)
                .is_none(),
            "a stationary test outlived its cap — the refresh must need a move"
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
                src.next_block(Some(test_at([0.0, 1.0, 0.0], 0.5)), sample_rate, block)
                    .is_some()
            );
        }
        assert!(
            src.next_block(Some(test_at([0.0, 1.0, 0.0], 0.5)), sample_rate, block)
                .is_none(),
            "the test outlived its {MAX_SECONDS} s cap"
        );
    }
}
