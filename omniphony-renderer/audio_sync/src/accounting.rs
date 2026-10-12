//! Frame accounting between the capture point and the resampler.
//!
//! The latency servo measures `L = (N_in − N_play)/rate + device delay`, with
//! `N_in` counted where source frames enter the renderer and `N_play` where
//! the resampler consumes them. That only holds if every frame counted in is
//! eventually consumed. Any stage that drops or inserts frames in between — a
//! decoder that skips to the next sync point, a queue that overflows — must
//! say so, and the servo then reads `N_in + inserted − dropped`.
//!
//! Counters live per *epoch*: a format, rate, source or writer change starts a
//! new one and the servo re-applies its start rule, so nothing carries over.
//! Updating them is one addition per event, never per frame; the events
//! themselves are returned to the caller, which forwards them off the realtime
//! path for diagnostics.

/// Where in the pipeline frames were dropped or inserted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Capture,
    Deframer,
    Decoder,
    Renderer,
    Ring,
}

/// Why they were.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscontinuityReason {
    /// The decoder emitted fewer frames than the bursts it was fed carry.
    DecoderShortfall,
    /// The decoder emitted more frames than the bursts it was fed carry.
    DecoderExcess,
    /// The parser lost sync and resynchronised.
    Resync,
    /// A bounded queue was full.
    Overflow,
    /// Silence substituted for a gap in the source (pause burst, stuffing).
    GapFill,
    /// A burst started before the one before it was through (a player that
    /// resumes or seeks starts a new burst at once): what the cut burst holds
    /// beyond the time it was given is dropped.
    Overlap,
    /// The renderer's own delay changed (a linear-phase filter switched in
    /// or out): the content sits that many frames later in its output.
    RenderDelay,
}

/// One drop or insertion, for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscontinuityEvent {
    pub epoch: u32,
    pub stage: Stage,
    /// Source frame position (in `N_in` terms) where it happened.
    pub position: u64,
    /// Frames inserted (positive) or dropped (negative).
    pub delta_frames: i64,
    pub reason: DiscontinuityReason,
}

/// Counters of one epoch. See the [module docs](self).
#[derive(Debug, Clone, Default)]
pub struct Accounting {
    epoch: u32,
    inserted: u64,
    dropped: u64,
}

impl Accounting {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// Start a new epoch: the counters restart from zero.
    pub fn new_epoch(&mut self) -> u32 {
        self.epoch = self.epoch.wrapping_add(1);
        self.inserted = 0;
        self.dropped = 0;
        self.epoch
    }

    /// Record a drop (`delta_frames < 0`) or an insertion (`> 0`).
    pub fn note(
        &mut self,
        stage: Stage,
        position: u64,
        delta_frames: i64,
        reason: DiscontinuityReason,
    ) -> DiscontinuityEvent {
        if delta_frames >= 0 {
            self.inserted = self.inserted.saturating_add(delta_frames as u64);
        } else {
            self.dropped = self.dropped.saturating_add(delta_frames.unsigned_abs());
        }
        DiscontinuityEvent {
            epoch: self.epoch,
            stage,
            position,
            delta_frames,
            reason,
        }
    }

    /// `inserted − dropped`: add it to `N_in` to get the frames that actually
    /// reach the resampler.
    pub fn offset_frames(&self) -> f64 {
        self.inserted as f64 - self.dropped as f64
    }
}

/// Watches what is in flight between the capture point and the ring, to
/// find the frames a stage lost or made up without saying so.
///
/// `in flight` is the source frames received less the frames delivered to
/// the ring, read when a burst starts (everything before it has been through
/// the decoder). In a pipeline that conserves its frames it is the decoder's
/// hold, plus what the decoder batches: its low points are one constant. A
/// burst the decoder skipped, bytes the capture lost or a gap the deframer
/// could not place raise that constant for good by the frames that went
/// missing; frames made up lower it.
///
/// The low point is taken over a window, and a step is reported once a
/// second window confirms it, as the frames to add to `N_in` (negative:
/// lost). What the pipeline holds when the watch starts is taken as its
/// hold: frames a decoder discards while it finds its first sync point are
/// not seen.
#[derive(Debug, Clone)]
pub struct InFlightAudit {
    window_s: f64,
    tolerance: f64,
    baseline: Option<f64>,
    window_start: Option<f64>,
    window_low: f64,
    /// The step the last window showed, waiting for the next one's word.
    candidate: Option<f64>,
}

impl InFlightAudit {
    /// `window_s`: the span a low point is taken over (longer than the
    /// decoder's largest batch). `tolerance`: the change in frames below
    /// which nothing happened.
    pub fn new(window_s: f64, tolerance: f64) -> Self {
        Self {
            window_s,
            tolerance,
            baseline: None,
            window_start: None,
            window_low: f64::INFINITY,
            candidate: None,
        }
    }

    /// Forget everything: the count this watched starts over.
    pub fn reset(&mut self) {
        *self = Self::new(self.window_s, self.tolerance);
    }

    /// The hold the pipeline is taken to have, once a window has shown it.
    pub fn baseline(&self) -> Option<f64> {
        self.baseline
    }

    /// One reading at `t` (s). Returns the frames to account when a step is
    /// confirmed: negative for frames lost, positive for frames made up.
    pub fn observe(&mut self, t: f64, in_flight: f64) -> Option<i64> {
        let start = *self.window_start.get_or_insert(t);
        if t - start < self.window_s {
            self.window_low = self.window_low.min(in_flight);
            return None;
        }
        let low = std::mem::replace(&mut self.window_low, in_flight);
        self.window_start = Some(t);
        let Some(baseline) = self.baseline else {
            self.baseline = Some(low);
            return None;
        };
        let step = low - baseline;
        if step.abs() <= self.tolerance {
            self.candidate = None;
            return None;
        }
        match self.candidate.take() {
            // Confirmed: the smaller of the two is what stayed.
            Some(before) if before.signum() == step.signum() => {
                let stayed = if before.abs() < step.abs() {
                    before
                } else {
                    step
                };
                self.baseline = Some(baseline + stayed);
                Some(-(stayed.round() as i64))
            }
            _ => {
                self.candidate = Some(step);
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Readings every 10 ms from `t0`: a hold of `hold` frames, with a batch
    /// ripple on top that returns to the hold twice in each 200 ms window.
    fn run(audit: &mut InFlightAudit, t0: f64, seconds: f64, hold: f64) -> Vec<i64> {
        let mut out = Vec::new();
        let n = (seconds * 100.0).round() as usize;
        for i in 0..n {
            let ripple = [0.0, 960.0, 480.0, 1440.0, 240.0][i % 5];
            if let Some(step) = audit.observe(t0 + i as f64 * 0.01, hold + ripple) {
                out.push(step);
            }
        }
        out
    }

    #[test]
    fn a_steady_hold_is_nothing_to_report() {
        let mut audit = InFlightAudit::new(0.2, 8.0);
        assert!(run(&mut audit, 0.0, 30.0, 1536.0).is_empty());
        assert_eq!(audit.baseline(), Some(1536.0));
    }

    /// A burst the decoder skipped stays in flight for good: reported once,
    /// as that many frames lost, within two windows. The hold it started
    /// from is not a loss.
    #[test]
    fn frames_that_went_missing_are_reported_once() {
        let mut audit = InFlightAudit::new(0.2, 8.0);
        assert!(run(&mut audit, 0.0, 2.0, 1536.0).is_empty());
        let steps = run(&mut audit, 2.0, 0.65, 1536.0 + 1536.0);
        assert_eq!(steps, [-1536]);
        assert!(run(&mut audit, 2.65, 5.0, 3072.0).is_empty());
        // And frames made up, the other way.
        assert_eq!(run(&mut audit, 7.65, 0.65, 3072.0 - 512.0), [512]);
    }

    /// A reading that is high for one window only (a slow decode, a batch
    /// that came late) is not a loss.
    #[test]
    fn a_passing_rise_is_not_a_loss() {
        let mut audit = InFlightAudit::new(0.2, 8.0);
        assert!(run(&mut audit, 0.0, 1.0, 512.0).is_empty());
        assert!(run(&mut audit, 1.0, 0.2, 4000.0).is_empty());
        assert!(run(&mut audit, 1.2, 3.0, 512.0).is_empty());
        assert_eq!(audit.baseline(), Some(512.0));
    }

    #[test]
    fn a_reset_learns_the_hold_again() {
        let mut audit = InFlightAudit::new(0.2, 8.0);
        assert!(run(&mut audit, 0.0, 1.0, 512.0).is_empty());
        audit.reset();
        assert!(run(&mut audit, 1.0, 5.0, 2048.0).is_empty());
        assert_eq!(audit.baseline(), Some(2048.0));
    }

    #[test]
    fn offset_is_inserted_minus_dropped() {
        let mut acc = Accounting::new();
        let ev = acc.note(Stage::Decoder, 960, -480, DiscontinuityReason::Resync);
        assert_eq!(ev.delta_frames, -480);
        assert_eq!(ev.epoch, 0);
        acc.note(Stage::Deframer, 4000, 1536, DiscontinuityReason::GapFill);
        assert_eq!(acc.offset_frames(), 1056.0);
    }

    #[test]
    fn a_new_epoch_clears_the_counters() {
        let mut acc = Accounting::new();
        acc.note(Stage::Ring, 0, -10, DiscontinuityReason::Overflow);
        assert_eq!(acc.new_epoch(), 1);
        assert_eq!(acc.offset_frames(), 0.0);
        let ev = acc.note(Stage::Ring, 0, 3, DiscontinuityReason::GapFill);
        assert_eq!(ev.epoch, 1);
    }
}
