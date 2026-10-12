use super::{
    ChannelRampState, RampContext, RampProgress, RampStatus, RampStrategy, RampTarget,
    interpolate_position, interpolate_size,
};

pub struct PositionRampStrategy;

impl RampStrategy for PositionRampStrategy {
    fn name(&self) -> &'static str {
        "position"
    }

    fn update_target(
        &self,
        state: &mut ChannelRampState,
        target: RampTarget,
        sample_index: Option<u64>,
        _ctx: &RampContext,
    ) {
        state.ramp_length = target.ramp_length;
        state.start_position = state.current_position;
        state.start_size = state.current_size;
        state.target_position = target.position;
        state.target_size = target.size;
        state.remaining_ramp_units = Some(target.ramp_length);
        state.target_sample_index = sample_index;
    }

    #[inline]
    fn evaluate(
        &self,
        state: &mut ChannelRampState,
        progress: RampProgress,
        _ctx: &RampContext,
    ) -> RampStatus {
        let fraction = progress.fraction();
        state.output_position =
            interpolate_position(state.start_position, state.target_position, fraction);
        state.current_size = interpolate_size(state.start_size, state.target_size, fraction);
        // Note: we intentionally do NOT compute gains here. The render path reads
        // only `output_position`/`current_size` from this strategy and recomputes
        // the VBAP gains itself at that position (via the backend). Computing them
        // here too (the old `compute_cached_or_direct` call) was a full, unused
        // VBAP evaluation per sample — the `output_gains` it wrote is never read.
        if progress.is_finished() {
            RampStatus::Finished
        } else if progress.completed_units == 0 && progress.total_units == 0 {
            RampStatus::Idle
        } else {
            RampStatus::Ramping
        }
    }

    fn as_position(&self) -> Option<&PositionRampStrategy> {
        Some(self)
    }
}
