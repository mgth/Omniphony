//! Idle silence feed for the test signals (speaker test and object test).
//!
//! A test only makes sound while the render loop runs, and the render
//! loop only runs when decoded frames arrive. With no input stream nothing
//! flows, so a test started from idle stayed silent (or, with the reverted
//! render-pump approach, starved the adaptive latency controller by writing
//! output the input path never saw — converge, hard-recover, cut).
//!
//! The fix mirrors what PipeWire live input mode does naturally: keep the
//! *input* flowing. While a client's test pane is armed (or a test is
//! running), the decode loop fabricates silence frames and pushes them through
//! `handle_audio_message` — the exact path real frames take — so the writer,
//! the latency controller and the metering all see an ordinary stream. The
//! feed stops the moment real input shows up again.
//!
//! This module owns only the *decision and pacing*; the frame fabrication and
//! the actual handler call stay in `session_run`, next to the decode loop.

use std::time::{Duration, Instant};

/// Arm keepalive: an arm message is honoured this long, and every re-arm
/// refreshes the window. Studio re-arms on a much shorter period while its
/// test pane is open, so an expiry means the client is gone — the feed must
/// not outlive it forever.
const KEEPALIVE: Duration = Duration::from_secs(600);

/// A real frame this recent means input is flowing: do not feed. Above the
/// 50 ms decode-loop receive timeout so ordinary inter-frame gaps never
/// trigger the feed, below anything a human would notice as warm-up.
const REAL_FRAME_HOLDOFF: Duration = Duration::from_millis(250);

/// Cap on a single fabricated chunk. Bounds the burst after a stall (standby,
/// a debugger pause) — the excess wall time is dropped, not queued as audio.
const MAX_CHUNK: Duration = Duration::from_millis(250);

/// Live-param + session inputs the pacing decision needs, decoupled from the
/// handler so the logic is unit-testable.
pub struct IdleFeedInputs {
    /// `LiveParams::speaker_test_idle_feed_gen` (0 = disarmed).
    pub arm_gen: u64,
    /// A test is currently requested (`LiveParams::speaker_test` or
    /// `LiveParams::object_test`).
    pub test_active: bool,
    /// Sample rate for fabricated frames (last stream's, or the default).
    pub sample_rate: u32,
}

/// One fabricated chunk of silence to push through the input path.
pub struct IdleFeedChunk {
    pub sample_count: u32,
    pub sample_rate: u32,
}

pub struct IdleFeeder {
    last_arm_gen: u64,
    armed_until: Option<Instant>,
    last_real_frame_at: Option<Instant>,
    feeding: bool,
    last_pump_at: Option<Instant>,
    frac_samples: f64,
}

impl Default for IdleFeeder {
    fn default() -> Self {
        Self {
            last_arm_gen: 0,
            armed_until: None,
            last_real_frame_at: None,
            feeding: false,
            last_pump_at: None,
            frac_samples: 0.0,
        }
    }
}

impl IdleFeeder {
    /// A real (channel-delivered, accepted-source) frame arrived.
    pub fn note_real_frame(&mut self, now: Instant) {
        self.last_real_frame_at = Some(now);
        self.stop_feeding();
    }

    fn stop_feeding(&mut self) {
        if self.feeding {
            log::info!("Speaker-test idle feed: stopping (real input or disarm)");
        }
        self.feeding = false;
        self.last_pump_at = None;
        self.frac_samples = 0.0;
    }

    /// Called from the decode loop's receive-timeout branch. Returns a chunk
    /// of silence to fabricate, or `None` when the feed should stay quiet.
    pub fn poll(&mut self, now: Instant, inputs: &IdleFeedInputs) -> Option<IdleFeedChunk> {
        if inputs.arm_gen != self.last_arm_gen {
            self.last_arm_gen = inputs.arm_gen;
            self.armed_until = (inputs.arm_gen != 0).then(|| now + KEEPALIVE);
        }
        let armed = inputs.test_active || self.armed_until.is_some_and(|deadline| now < deadline);
        if !armed {
            self.stop_feeding();
            return None;
        }
        if self
            .last_real_frame_at
            .is_some_and(|at| now.saturating_duration_since(at) < REAL_FRAME_HOLDOFF)
        {
            self.stop_feeding();
            return None;
        }

        let Some(last_pump_at) = self.last_pump_at else {
            // First armed tick: start the clock, emit from the next one, so a
            // stale wall-time gap never turns into a burst of audio.
            log::info!("Speaker-test idle feed: starting silence injection");
            self.feeding = true;
            self.last_pump_at = Some(now);
            self.frac_samples = 0.0;
            return None;
        };

        let elapsed = now.saturating_duration_since(last_pump_at).min(MAX_CHUNK);
        self.last_pump_at = Some(now);
        let sample_rate = inputs.sample_rate.max(1);
        let exact = elapsed.as_secs_f64() * sample_rate as f64 + self.frac_samples;
        let sample_count = exact.floor();
        self.frac_samples = exact - sample_count;
        if sample_count < 1.0 {
            return None;
        }
        Some(IdleFeedChunk {
            sample_count: sample_count as u32,
            sample_rate,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs(arm_gen: u64) -> IdleFeedInputs {
        IdleFeedInputs {
            arm_gen,
            test_active: false,
            sample_rate: 48_000,
        }
    }

    #[test]
    fn disarmed_never_feeds() {
        let mut feeder = IdleFeeder::default();
        let t0 = Instant::now();
        assert!(feeder.poll(t0, &inputs(0)).is_none());
        assert!(
            feeder
                .poll(t0 + Duration::from_millis(50), &inputs(0))
                .is_none()
        );
    }

    #[test]
    fn armed_feeds_wall_clock_samples_after_priming_tick() {
        let mut feeder = IdleFeeder::default();
        let t0 = Instant::now();
        // Priming tick starts the clock without emitting.
        assert!(feeder.poll(t0, &inputs(1)).is_none());
        let chunk = feeder
            .poll(t0 + Duration::from_millis(50), &inputs(1))
            .expect("second tick emits");
        assert_eq!(chunk.sample_count, 2400);
        assert_eq!(chunk.sample_rate, 48_000);
    }

    #[test]
    fn fractional_samples_carry_across_ticks() {
        let mut feeder = IdleFeeder::default();
        let t0 = Instant::now();
        let mut inputs = inputs(1);
        inputs.sample_rate = 44_100;
        assert!(feeder.poll(t0, &inputs).is_none());
        let mut total = 0u64;
        for tick in 1..=10 {
            if let Some(chunk) = feeder.poll(t0 + Duration::from_millis(50 * tick), &inputs) {
                total += chunk.sample_count as u64;
            }
        }
        // 500 ms at 44.1 kHz = 22050 samples; per-tick floor must not lose any.
        assert_eq!(total, 22_050);
    }

    #[test]
    fn stall_is_capped_not_queued() {
        let mut feeder = IdleFeeder::default();
        let t0 = Instant::now();
        assert!(feeder.poll(t0, &inputs(1)).is_none());
        let chunk = feeder
            .poll(t0 + Duration::from_secs(30), &inputs(1))
            .expect("emits after stall");
        assert_eq!(chunk.sample_count as u128, MAX_CHUNK.as_millis() * 48);
    }

    #[test]
    fn real_frame_holds_the_feed_off() {
        let mut feeder = IdleFeeder::default();
        let t0 = Instant::now();
        assert!(feeder.poll(t0, &inputs(1)).is_none());
        feeder.note_real_frame(t0 + Duration::from_millis(60));
        // Within the holdoff: quiet, and the pacing clock was reset.
        assert!(
            feeder
                .poll(t0 + Duration::from_millis(100), &inputs(1))
                .is_none()
        );
        // Past the holdoff: primes again, then emits.
        assert!(
            feeder
                .poll(t0 + Duration::from_millis(400), &inputs(1))
                .is_none()
        );
        assert!(
            feeder
                .poll(t0 + Duration::from_millis(450), &inputs(1))
                .is_some()
        );
    }

    #[test]
    fn keepalive_expires_without_rearm_but_refreshes_on_gen_bump() {
        let mut feeder = IdleFeeder::default();
        let t0 = Instant::now();
        assert!(feeder.poll(t0, &inputs(1)).is_none());
        let expired = t0 + KEEPALIVE + Duration::from_secs(1);
        assert!(feeder.poll(expired, &inputs(1)).is_none());
        // A re-arm (gen bump) opens a fresh window: prime, then emit.
        assert!(feeder.poll(expired, &inputs(2)).is_none());
        assert!(
            feeder
                .poll(expired + Duration::from_millis(50), &inputs(2))
                .is_some()
        );
    }

    #[test]
    fn active_test_feeds_even_without_arm() {
        let mut feeder = IdleFeeder::default();
        let t0 = Instant::now();
        let mut inputs = inputs(0);
        inputs.test_active = true;
        assert!(feeder.poll(t0, &inputs).is_none());
        assert!(
            feeder
                .poll(t0 + Duration::from_millis(50), &inputs)
                .is_some()
        );
    }
}
