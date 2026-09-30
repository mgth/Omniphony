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

/// Keep the renderer's DSP on the rate the stream actually runs at. A host may
/// build the renderer before it has seen the stream (the standalone renderer
/// always does) or be told another rate than the stream's; rendering 96 kHz
/// frames with 48 kHz filters, delays and ramps puts every crossover an octave
/// up and halves every delay. One comparison per frame; the rebuild
/// ([`SpatialRenderer::set_sample_rate`]) only runs when the rate changes.
///
/// [`SpatialRenderer::set_sample_rate`]: renderer::spatial_renderer::SpatialRenderer::set_sample_rate
#[inline]
pub fn follow_stream_rate(
    renderer: &mut renderer::spatial_renderer::SpatialRenderer,
    stream_rate: u32,
) -> anyhow::Result<()> {
    if stream_rate != 0 && stream_rate != renderer.sample_rate() {
        log::info!(
            "Stream runs at {} Hz, the renderer at {} Hz: rebuilding its DSP for the stream",
            stream_rate,
            renderer.sample_rate()
        );
        renderer.set_sample_rate(stream_rate)?;
    }
    Ok(())
}

#[cfg(test)]
mod rate_tests {
    use super::follow_stream_rate;

    fn renderer_at(rate: u32) -> renderer::spatial_renderer::SpatialRenderer {
        crate::renderer_build::build_spatial_renderer(
            &crate::renderer_build::SpatialRendererParams::from_render_config(None),
            renderer::speaker_layout::SpeakerLayout::preset("7.1.4").expect("preset"),
            rate,
            bridge_api::RVbapCartesianDefaults {
                x_size: 9,
                y_size: 9,
                z_size: 5,
                allow_negative_z: false,
            },
            bridge_api::RVbapTableMode::Cartesian,
            None,
        )
        .expect("renderer")
    }

    /// A renderer built before the stream was seen follows the stream's rate:
    /// its DSP, and the rate it publishes to the control, move to 96 kHz with
    /// the first 96 kHz frame, and it keeps rendering at the right width.
    #[test]
    fn the_renderer_follows_the_stream_rate() {
        let mut renderer = renderer_at(48_000);
        follow_stream_rate(&mut renderer, 48_000).expect("same rate");
        assert_eq!(renderer.sample_rate(), 48_000);

        follow_stream_rate(&mut renderer, 96_000).expect("new rate");
        assert_eq!(renderer.sample_rate(), 96_000);
        assert_eq!(
            renderer
                .renderer_control()
                .sample_rate
                .load(std::sync::atomic::Ordering::Relaxed),
            96_000
        );
        let pcm = vec![0.1f32; 960 * 2];
        let rendered = renderer
            .render_frame(&pcm, 2, &[], Vec::new(), false)
            .expect("render at 96 kHz");
        assert_eq!(rendered.n_channels, renderer.num_speakers());
        assert_eq!(rendered.samples.len(), 960 * renderer.num_speakers());

        // A zero rate (no frame information) leaves it alone.
        follow_stream_rate(&mut renderer, 0).expect("no rate");
        assert_eq!(renderer.sample_rate(), 96_000);
    }

    /// Re-targeted to a rate, a renderer renders exactly as one built at that
    /// rate: the rebuild leaves nothing of the old rate behind.
    #[test]
    fn a_retargeted_renderer_matches_one_built_at_that_rate() {
        use renderer::spatial_renderer::SpatialChannelEvent;
        let mut retargeted = renderer_at(48_000);
        follow_stream_rate(&mut retargeted, 44_100).expect("new rate");
        let mut native = renderer_at(44_100);

        let frames = 441;
        let pcm: Vec<f32> = (0..frames * 2)
            .map(|i| ((i as f32) * 0.37).sin() * 0.5)
            .collect();
        // A stereo bed, planned through the real channel planner.
        let labels = [bridge_api::RChannelLabel::L, bridge_api::RChannelLabel::R];
        let events = |renderer: &renderer::spatial_renderer::SpatialRenderer| {
            let mut planner = crate::virtual_bed::BedChannelPlanner::new();
            planner.plan(
                renderer,
                &labels,
                renderer::placement::SourceFamily::Generic,
                &[],
            );
            planner.events().to_vec()
        };
        let retargeted_events: Vec<SpatialChannelEvent> = events(&retargeted);
        let native_events: Vec<SpatialChannelEvent> = events(&native);
        for _ in 0..4 {
            let a = retargeted
                .render_frame(&pcm, 2, &retargeted_events, Vec::new(), false)
                .expect("render");
            let b = native
                .render_frame(&pcm, 2, &native_events, Vec::new(), false)
                .expect("render");
            assert!(a.samples.iter().any(|&x| x != 0.0), "rendered silence");
            assert_eq!(a.samples, b.samples);
        }
    }
}
