//! What the sync host publishes over OSC about its output stage.
//!
//! Two routes, both existing ones:
//!
//! - the meter bundle's output-stage figures
//!   ([`Engine::set_output_timings`]): the latency target, the latency
//!   measured and the resampling ratio, on the addresses the Studio's latency
//!   gauges already read;
//! - the diag channels ([`Engine::publish_diag`]): the servo's own quantities
//!   (latency error, feed-forward, phase correction, the two clock rates, the
//!   floor, and the realign / underrun / rephase counts), which the Studio's
//!   diag plot discovers from the schema.
//!
//! The latency target is the contract (`docs/resampling-rework-plan.md`
//! Phase 5): it is the constant `L` the host holds and the player is told,
//! never the instantaneous measurement, which is published beside it.
//!
//! A target the pipeline cannot sustain is raised by the servo (plan §19).
//! What is published as the target is then the latency held, a setpoint
//! still; the configured one stays on its own channel, the raise is logged
//! once, and the host keeps it for the streams that follow instead of
//! learning it again at each of them.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use audio_output::sync_output::SyncSnapshot;
use audio_sync::DiscontinuityEvent;
use diag::DiagRegistry;
use orender_engine::engine::Engine;
use orender_engine::osc::MeterTimings;

/// The diag channels the diag plot shows in its "base" tab.
/// A raise of the held target is logged when it moved by this much since
/// the last line (ms): the servo refines it by fractions of a ms.
const RAISE_LOG_STEP_MS: f64 = 1.0;

const BASE_CHANNELS: [&str; 5] = [
    "latency_ms",
    "latency_target_ms",
    "latency_error_ms",
    "feedforward_ppm",
    "correction_ppm",
];

/// See the [module docs](self).
pub struct HostTelemetry {
    registry: DiagRegistry,
    /// The target as configured, and the one held: the same, unless the
    /// servo had to raise it.
    configured_ms: f64,
    held_ms: f64,
    /// The held target when a raise was last logged.
    logged_ms: f64,
    /// A raise the host has not passed on to its clients yet.
    unreported_raise: Option<f64>,
    target_ms: Arc<AtomicU64>,
    configured_target_ms: Arc<AtomicU64>,
    latency_ms: Arc<AtomicU64>,
    error_ms: Arc<AtomicU64>,
    floor_ms: Arc<AtomicU64>,
    feedforward_ppm: Arc<AtomicU64>,
    correction_ppm: Arc<AtomicU64>,
    source_ppm: Arc<AtomicU64>,
    device_ppm: Arc<AtomicU64>,
    realigns: Arc<AtomicU64>,
    underruns: Arc<AtomicU64>,
    rephases: Arc<AtomicU64>,
    /// Frames lost and frames made up between the capture point and the
    /// ring, for the life of the host (plan §25).
    frames_lost: Arc<AtomicU64>,
    frames_filled: Arc<AtomicU64>,
    lost_total: u64,
    filled_total: u64,
    /// Counts of the streams already over: an epoch's servo starts from zero,
    /// the channels count for the life of the host.
    carried: Counts,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Counts {
    realigns: u64,
    underruns: u64,
    rephases: u64,
}

impl Counts {
    fn of(s: &SyncSnapshot) -> Self {
        Self {
            realigns: s.realigns,
            underruns: s.underruns,
            rephases: s.source_rephases,
        }
    }

    fn plus(self, other: Self) -> Self {
        Self {
            realigns: self.realigns + other.realigns,
            underruns: self.underruns + other.underruns,
            rephases: self.rephases + other.rephases,
        }
    }
}

fn put(a: &AtomicU64, v: f64) {
    a.store(v.to_bits(), Ordering::Relaxed);
}

impl HostTelemetry {
    pub fn new(target_ms: f64) -> Self {
        let registry = DiagRegistry::new();
        let channel = |name, label, group, unit| registry.register(name, label, group, unit);
        let target = channel("latency_target_ms", "Target latency", "latency", "ms");
        put(&target, target_ms);
        let configured = channel(
            "latency_target_configured_ms",
            "Configured target",
            "latency",
            "ms",
        );
        put(&configured, target_ms);
        let telemetry = Self {
            configured_ms: target_ms,
            held_ms: target_ms,
            logged_ms: target_ms,
            unreported_raise: None,
            target_ms: target,
            configured_target_ms: configured,
            latency_ms: channel("latency_ms", "End-to-end latency", "latency", "ms"),
            error_ms: channel("latency_error_ms", "Latency error", "latency", "ms"),
            floor_ms: channel("latency_floor_ms", "Latency floor", "latency", "ms"),
            feedforward_ppm: channel("feedforward_ppm", "Feed-forward rate", "sync", "ppm"),
            correction_ppm: channel("correction_ppm", "Phase correction", "sync", "ppm"),
            source_ppm: channel("source_ppm", "Source clock", "sync", "ppm"),
            device_ppm: channel("device_ppm", "Device clock", "sync", "ppm"),
            realigns: channel("realigns", "Realigns", "sync", "count"),
            underruns: channel("underruns", "Underruns", "sync", "count"),
            rephases: channel("source_rephases", "Source rephases", "sync", "count"),
            frames_lost: channel("frames_lost", "Frames lost", "accounting", "frames"),
            frames_filled: channel("frames_filled", "Frames filled", "accounting", "frames"),
            lost_total: 0,
            filled_total: 0,
            carried: Counts::default(),
            registry,
        };
        for name in BASE_CHANNELS {
            telemetry.registry.set_tier(name, "base");
        }
        telemetry
    }

    /// Publish the state of the stream playing (`None` between streams, when
    /// the channels keep their last values and only the target is reported).
    pub fn publish(&mut self, engine: &mut Engine, snapshot: Option<&SyncSnapshot>) {
        if let Some(s) = snapshot {
            self.store(s);
        }
        engine.set_output_timings(output_timings(self.held_ms, snapshot));
        engine.publish_diag(|| (self.registry.schema_json(), self.registry.values_json()));
    }

    /// The client set another target: it is the one held from now on, and
    /// what an earlier stream had raised the previous one to is forgotten.
    pub fn set_configured_target(&mut self, target_ms: f64) {
        self.configured_ms = target_ms;
        self.held_ms = target_ms;
        self.logged_ms = target_ms;
        self.unreported_raise = None;
        put(&self.target_ms, target_ms);
        put(&self.configured_target_ms, target_ms);
    }

    /// A raise of the held target since the last call, for the host to
    /// publish (at the step the log uses: the servo's refinements of a
    /// fraction of a ms are not news).
    pub fn take_raise(&mut self) -> Option<f64> {
        self.unreported_raise.take()
    }

    /// The latency the next stream starts at: the configured target, or what
    /// an earlier stream showed the pipeline needs.
    pub fn held_target_ms(&self) -> f64 {
        self.held_ms
    }

    /// Take in the target the servo holds, and say so when it raised it.
    fn follow_held_target(&mut self, s: &SyncSnapshot) {
        let Some(held) = s.target_ms.filter(|held| *held > self.held_ms) else {
            return;
        };
        self.held_ms = held;
        put(&self.target_ms, held);
        if held - self.logged_ms >= RAISE_LOG_STEP_MS {
            self.logged_ms = held;
            self.unreported_raise = Some(held);
            log::warn!(
                "sync host: a latency target of {:.0} ms is below what this stream needs \
                 (floor {:.1} ms): holding {:.1} ms. Set render.latency_target to at least \
                 that to play at the latency the player is told.",
                self.configured_ms,
                s.floor_ms,
                held
            );
        }
    }

    /// Store a stream's state in the channels. A latency not measured yet
    /// leaves the last one in place instead of publishing a hole.
    fn store(&mut self, s: &SyncSnapshot) {
        self.follow_held_target(s);
        if let Some(latency) = s.latency_ms {
            put(&self.latency_ms, latency);
        }
        put(&self.error_ms, s.error_ms);
        put(&self.floor_ms, s.floor_ms);
        put(&self.feedforward_ppm, s.feedforward_ppm);
        put(&self.correction_ppm, s.correction_ppm);
        put(&self.source_ppm, s.source_ppm);
        put(&self.device_ppm, s.device_ppm);
        self.put_counts(self.carried.plus(Counts::of(s)));
    }

    /// Frames were lost or made up between the capture point and the ring:
    /// said in the log, and counted. (A change of the renderer's own delay
    /// moves frames, it does not lose or make any.)
    pub fn note_discontinuity(&mut self, event: &DiscontinuityEvent) {
        use audio_sync::DiscontinuityReason as Reason;
        let frames = event.delta_frames;
        match event.reason {
            Reason::RenderDelay => {
                log::info!("sync host: the render's own delay changed by {frames:+} frames");
                return;
            }
            // A source that pauses does so burst after burst.
            Reason::GapFill => log::debug!(
                "sync host: {frames} frames of silence for a gap in the source (stream frame {})",
                event.position
            ),
            // What a player does when it resumes or seeks.
            Reason::Overlap => log::debug!(
                "sync host: {} frames of a burst cut short dropped (stream frame {})",
                frames.unsigned_abs(),
                event.position
            ),
            reason => log::warn!(
                "sync host: {} frames {} at the {:?} ({reason:?}, stream frame {})",
                frames.unsigned_abs(),
                if frames < 0 { "lost" } else { "made up" },
                event.stage,
                event.position
            ),
        }
        if frames < 0 {
            self.lost_total += frames.unsigned_abs();
        } else {
            self.filled_total += frames as u64;
        }
        put(&self.frames_lost, self.lost_total as f64);
        put(&self.frames_filled, self.filled_total as f64);
    }

    /// A stream is over: its counts stay in the totals.
    pub fn close_epoch(&mut self, last: &SyncSnapshot) {
        self.carried = self.carried.plus(Counts::of(last));
        self.put_counts(self.carried);
    }

    fn put_counts(&self, counts: Counts) {
        put(&self.realigns, counts.realigns as f64);
        put(&self.underruns, counts.underruns as f64);
        put(&self.rephases, counts.rephases as f64);
    }
}

/// The output-stage figures of the meter bundle: the constant target, and,
/// while a stream plays, the latency measured and the resampling ratio
/// (feed-forward times phase correction).
fn output_timings(target_ms: f64, snapshot: Option<&SyncSnapshot>) -> MeterTimings {
    MeterTimings {
        latency_target_ms: Some(target_ms as f32),
        latency_instant_ms: snapshot.and_then(|s| s.latency_ms).map(|l| l as f32),
        resample_ratio: snapshot
            .map(|s| ((1.0 + s.feedforward_ppm * 1e-6) * (1.0 + s.correction_ppm * 1e-6)) as f32),
        ..MeterTimings::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(t: &HostTelemetry, name: &str) -> f64 {
        let values: serde_json::Value =
            serde_json::from_str(&t.registry.values_json().unwrap()).unwrap();
        values[name].as_f64().unwrap()
    }

    /// The target is what the host holds: it is published as configured
    /// whether or not a stream plays, and never follows the measurement.
    #[test]
    fn the_target_is_constant_and_the_measurement_beside_it() {
        let idle = output_timings(150.0, None);
        assert_eq!(idle.latency_target_ms, Some(150.0));
        assert_eq!(idle.latency_instant_ms, None);
        assert_eq!(idle.resample_ratio, None);

        let playing = SyncSnapshot {
            latency_ms: Some(151.25),
            feedforward_ppm: 100.0,
            correction_ppm: -20.0,
            ..SyncSnapshot::default()
        };
        let t = output_timings(150.0, Some(&playing));
        assert_eq!(t.latency_target_ms, Some(150.0));
        assert_eq!(t.latency_instant_ms, Some(151.25));
        let ratio = f64::from(t.resample_ratio.unwrap());
        assert!((ratio - 1.000_08).abs() < 2e-7, "ratio {ratio}");
    }

    #[test]
    fn an_unmeasured_latency_keeps_the_last_value() {
        let mut t = HostTelemetry::new(150.0);
        t.store(&SyncSnapshot {
            latency_ms: Some(149.5),
            error_ms: -0.5,
            ..SyncSnapshot::default()
        });
        t.store(&SyncSnapshot {
            latency_ms: None,
            error_ms: 0.25,
            ..SyncSnapshot::default()
        });
        assert_eq!(value(&t, "latency_ms"), 149.5);
        assert_eq!(value(&t, "latency_error_ms"), 0.25);
        assert_eq!(value(&t, "latency_target_ms"), 150.0);
    }

    /// A target the servo raised is what the host publishes and starts the
    /// next stream at; the configured one stays visible beside it. A new
    /// stream's servo starting from the configured target again does not
    /// bring it back down.
    #[test]
    fn a_raised_target_is_published_and_kept_for_the_next_stream() {
        let mut t = HostTelemetry::new(50.0);
        assert_eq!(t.held_target_ms(), 50.0);
        t.store(&SyncSnapshot {
            target_ms: Some(99.0),
            floor_ms: 72.7,
            ..SyncSnapshot::default()
        });
        assert_eq!(t.held_target_ms(), 99.0);
        assert_eq!(t.take_raise(), Some(99.0));
        assert_eq!(t.take_raise(), None, "reported once");
        assert_eq!(value(&t, "latency_target_ms"), 99.0);
        assert_eq!(value(&t, "latency_target_configured_ms"), 50.0);
        assert_eq!(
            output_timings(t.held_target_ms(), None).latency_target_ms,
            Some(99.0)
        );

        t.store(&SyncSnapshot {
            target_ms: Some(50.0),
            ..SyncSnapshot::default()
        });
        t.store(&SyncSnapshot::default());
        assert_eq!(t.held_target_ms(), 99.0);

        // A target the client sets replaces both.
        t.set_configured_target(120.0);
        assert_eq!(t.held_target_ms(), 120.0);
        assert_eq!(value(&t, "latency_target_ms"), 120.0);
        assert_eq!(value(&t, "latency_target_configured_ms"), 120.0);
    }

    /// The counts are the host's, not the stream's: a new stream's servo
    /// starts from zero and the channels go on from where they were.
    /// Frames lost and frames that filled a gap are counted for the life of
    /// the host; a change of the render's own delay is neither.
    #[test]
    fn what_was_lost_and_filled_is_counted() {
        use audio_sync::{Accounting, DiscontinuityReason as Reason, Stage};
        let mut t = HostTelemetry::new(150.0);
        let mut accounting = Accounting::new();
        for (stage, delta, reason) in [
            (Stage::Decoder, -1536, Reason::DecoderShortfall),
            (Stage::Deframer, 38_400, Reason::GapFill),
            (Stage::Ring, -64, Reason::Overflow),
            (Stage::Renderer, 1024, Reason::RenderDelay),
        ] {
            t.note_discontinuity(&accounting.note(stage, 0, delta, reason));
        }
        assert_eq!(value(&t, "frames_lost"), 1600.0);
        assert_eq!(value(&t, "frames_filled"), 38_400.0);
    }

    #[test]
    fn counts_carry_across_streams() {
        let mut t = HostTelemetry::new(150.0);
        let first = SyncSnapshot {
            realigns: 2,
            underruns: 1,
            source_rephases: 3,
            ..SyncSnapshot::default()
        };
        t.close_epoch(&first);
        t.store(&SyncSnapshot {
            realigns: 1,
            ..SyncSnapshot::default()
        });
        assert_eq!(value(&t, "realigns"), 3.0);
        assert_eq!(value(&t, "underruns"), 1.0);
        assert_eq!(value(&t, "source_rephases"), 3.0);
    }

    #[test]
    fn the_servo_quantities_are_in_the_base_tab() {
        let t = HostTelemetry::new(150.0);
        let schema: serde_json::Value =
            serde_json::from_str(&t.registry.schema_json().unwrap()).unwrap();
        let base: Vec<&str> = schema["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["tier"] == "base")
            .map(|i| i["name"].as_str().unwrap())
            .collect();
        for name in BASE_CHANNELS {
            assert!(base.contains(&name), "{name} is not in the base tab");
        }
    }
}
