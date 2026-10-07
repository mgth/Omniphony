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

#[cfg(test)]
mod tests {
    use super::*;

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
