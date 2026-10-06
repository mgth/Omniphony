//! Metering around one render call, run by the frame sequence both hosts
//! share ([`crate::frame_pipeline`]), so the levels a client sees do not
//! depend on which host renders: what goes into which accumulator, and when a
//! snapshot is due.

use renderer::metering::{AudioMeter, MeterSnapshot};
use renderer::spatial_renderer::{RenderedFrame, SpatialRenderer};

/// Feed the render input — fixed channels and objects, `channels` wide,
/// interleaved — to the per-object accumulators, before the render.
pub fn meter_render_input(meter: &mut AudioMeter, pcm: &[f32], channels: usize) {
    if channels == 0 {
        return;
    }
    meter.update_channel_count(channels);
    for chunk in pcm.chunks_exact(channels) {
        meter.process_objects(chunk, channels);
    }
}

/// Feed a rendered frame to the output accumulators and return a snapshot
/// when one is due at the meter's cadence.
///
/// Which accumulators depends on what was rendered, not on the layout: a
/// binaural frame is a stereo pair for the ear meters (metering it as
/// speakers would stride through it wrongly and leave the ear gauges dead);
/// the cascaded binaural mode also meters its virtual speaker bus, so the
/// speaker gauges show the virtual room. Per-object band energies (crossover
/// objects) feed the object band meters.
pub fn meter_render_output(
    meter: &mut AudioMeter,
    renderer: &SpatialRenderer,
    rendered: &RenderedFrame,
    out: &mut MeterSnapshot,
) -> bool {
    if let Some((bus, n_bus)) = renderer.virtual_bus() {
        meter.process_speakers(bus, n_bus);
        meter.process_ears(&rendered.samples);
    } else if renderer.output_is_binaural() {
        meter.process_ears(&rendered.samples);
    } else {
        meter.process_speakers(&rendered.samples, rendered.n_channels);
    }
    meter.process_object_bands(&rendered.object_band_sq);
    meter.poll_into(out)
}
