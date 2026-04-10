use super::{
    ChannelRampState, RampContext, RampProgress, RampStatus, RampStrategy, RampTarget,
    compute_cached_or_direct,
};

pub struct GainTableRampStrategy;

impl RampStrategy for GainTableRampStrategy {
    fn name(&self) -> &'static str {
        "gain_table"
    }

    fn update_target(
        &self,
        state: &mut ChannelRampState,
        target: RampTarget,
        sample_index: Option<u64>,
        ctx: &RampContext<'_>,
    ) {
        state.ensure_speaker_count(ctx.speaker_count());
        if !state.gains_initialized {
            compute_cached_or_direct(state, state.current_position, ctx);
        }
        state.ramp_length = target.ramp_length;
        state.start_gains = state.output_gains.clone();
        state.start_position = target.position;
        state.start_spread = target.spread;
        state.current_position = target.position;
        state.current_spread = target.spread;
        state.target_position = target.position;
        state.target_spread = target.spread;
        state.output_position = target.position;
        state.target_gains = ctx.compute_gains(target.position);
        state.remaining_ramp_units = Some(target.ramp_length);
        state.target_sample_index = sample_index;
        state.invalidate_cache();
    }

    fn evaluate(
        &self,
        state: &mut ChannelRampState,
        progress: RampProgress,
        ctx: &RampContext<'_>,
    ) -> RampStatus {
        state.ensure_speaker_count(ctx.speaker_count());
        state.output_position = state.target_position;
        if !state.gains_initialized {
            state.start_gains = ctx.compute_gains(state.target_position);
            state.target_gains = state.start_gains.clone();
            state.gains_initialized = true;
        }
        let fraction = progress.fraction() as f32;
        let inv = 1.0 - fraction;
        for idx in 0..state.output_gains.len() {
            state.output_gains[idx] =
                state.start_gains[idx] * inv + state.target_gains[idx] * fraction;
        }
        if progress.is_finished() {
            RampStatus::Finished
        } else if progress.completed_units == 0 && progress.total_units == 0 {
            RampStatus::Idle
        } else {
            RampStatus::Ramping
        }
    }
}
