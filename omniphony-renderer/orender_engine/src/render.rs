//! Pure DSP helpers shared by the engine and the CLI host (no I/O).

/// Convert interleaved 24-bit-scaled `i32` PCM to `f32`, applying a per-sample
/// DRC gain ramp from `*current_gain` toward `target_gain` over the remaining
/// `*ramp_remaining` samples.
///
/// `out` is cleared and refilled with the same length as `pcm`. `current_gain`
/// and `ramp_remaining` are advanced in place so the ramp continues seamlessly
/// across calls (one decoded frame per call).
#[inline]
pub fn fill_pcm_f32_drc(
    out: &mut Vec<f32>,
    pcm: &[i32],
    channel_count: usize,
    current_gain: &mut f32,
    target_gain: f32,
    ramp_remaining: &mut u32,
) {
    const SCALE: f32 = bridge_api::I32_PCM_FULL_SCALE as f32;

    out.clear();
    out.reserve(pcm.len().saturating_sub(out.capacity()));

    if channel_count == 0 {
        return;
    }
    let sample_count = pcm.len() / channel_count;

    for s in 0..sample_count {
        let gain = if *ramp_remaining > 0 {
            let step = (target_gain - *current_gain) / *ramp_remaining as f32;
            *current_gain += step;
            *ramp_remaining -= 1;
            *current_gain
        } else {
            *current_gain = target_gain;
            target_gain
        };

        let scaled_gain = gain / SCALE;

        for c in 0..channel_count {
            let val = pcm[s * channel_count + c];
            out.push(val as f32 * scaled_gain);
        }
    }
}
