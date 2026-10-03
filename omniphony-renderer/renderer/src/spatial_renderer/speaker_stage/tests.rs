//! Guards for the mix. Bit identity first: `mix_channels` and the bus it mixes
//! on, against the sample-major mix they replaced — every source added sample
//! by sample, band by band, densely over all speakers, straight into the
//! interleaved output, a gain lookup per sample for a moving object in
//! `RampMode::Sample` — which is kept here as the reference. Then the one place
//! the mix departs from it: the stride between the gain lookups of a moving
//! object in `RampMode::Sample`, measured against that same reference.

use super::*;
use crate::crossover::SmallBands;
use crate::ramp_strategy::{PositionRampStrategy, RampRenderParams};
use crate::spatial_renderer::tests::{build_table_renderer, noise_block};
use crate::spatial_renderer::{SpatialChannelEvent, SpatialRenderer};

/// Everything the mix keeps per channel, for the reference: it runs against
/// the same stage as `mix_channels` (bands, table, crossover bank) but on its
/// own copy of the channel history.
#[derive(Default)]
struct ReferenceState {
    channel_states: Vec<ChannelState>,
    filter_states: Vec<Option<CrossoverStates>>,
    bed_delays: Vec<Option<IntegerDelay>>,
    interp_end: Vec<Gains>,
}

fn reference_accumulate_band(out_frame: &mut [f32], gains: &[f32], sample: f32) {
    for (out, &gain) in out_frame.iter_mut().zip(gains.iter()) {
        *out += sample * gain;
    }
}

/// The sample-major mix. `output` must come in zeroed. Returns, per channel,
/// the band gains of the block's last sample — what the meters are handed.
fn reference_mix_channels(
    stage: &SpeakerRenderStage,
    state: &mut ReferenceState,
    frame: &SpeakerStageFrame<'_>,
    output: &mut [f32],
) -> Vec<(usize, Vec<Gains>)> {
    let num_speakers = stage.num_speakers;
    let sample_length = frame.sample_length;
    let input_channel_count = frame.input_channel_count;
    let input_pcm = frame.input_pcm;
    let mut last_gains = Vec::new();

    for ch in 0..input_channel_count {
        let obj_gain = match frame.object_params.get(ch) {
            Some(o) if o.muted => 0.0,
            _ => 1.0,
        };
        let channel = SpatialRenderer::state_mut(&mut state.channel_states, ch);
        let gain_linear = crate::spatial_renderer::gain_db_to_linear(channel.gain_db);
        let ramp_samples = stage.sample_rate as f32 * GAIN_SLEW_SECS;
        let (gain_start, gain_step) =
            channel.slew_gain(gain_linear * obj_gain, sample_length, ramp_samples);
        let input_at = |sample_idx: usize| {
            input_pcm[sample_idx * input_channel_count + ch]
                * (gain_start + gain_step * sample_idx as f32)
        };

        if let Some(ChannelRoute::Direct(label)) = frame.channel_routing.get(ch) {
            let Some(&speaker_idx) = frame.label_to_speaker.get(label) else {
                continue;
            };
            let mut routing = vec![0.0f32; num_speakers];
            routing[speaker_idx] = 1.0;
            let latency = stage
                .crossover_filter_bank
                .as_ref()
                .map_or(0, |b| b.latency_samples());
            if state.bed_delays.len() <= ch {
                state.bed_delays.resize_with(ch + 1, || None);
            }
            let mut delay = if latency > 0 {
                let slot = &mut state.bed_delays[ch];
                if slot.as_ref().is_none_or(|d| d.delay() != latency) {
                    *slot = Some(IntegerDelay::new(latency));
                }
                slot.as_mut()
            } else {
                None
            };
            for sample_idx in 0..sample_length {
                let mut sample = input_at(sample_idx);
                if let Some(delay) = delay.as_mut() {
                    sample = delay.push(sample);
                }
                let out_base = sample_idx * num_speakers;
                for (speaker_idx, &gain) in routing.iter().enumerate() {
                    output[out_base + speaker_idx] += sample * gain;
                }
            }
            let mut gains = Gains::zeroed(num_speakers);
            gains[..num_speakers].copy_from_slice(&routing);
            last_gains.push((ch, vec![gains]));
            continue;
        }

        if state.filter_states.len() <= ch {
            state.filter_states.resize_with(ch + 1, || None);
        }
        let bank = stage.crossover_filter_bank.as_ref();
        // The channel's own crossover state, with its own hop phase.
        let mut filter_states =
            bank.map(|fb| fb.ensure_channel_states(&mut state.filter_states[ch], ch));
        let mut split_at = |sample_idx: usize| match (bank, filter_states.as_mut()) {
            (Some(fb), Some(states)) => fb.process_sample(input_at(sample_idx), states),
            _ => SmallBands::single(input_at(sample_idx)),
        };
        let mut mix_sample = |output: &mut [f32], sample_idx: usize, band_gains: &[Gains]| {
            let split = split_at(sample_idx);
            let out_base = sample_idx * num_speakers;
            let out_frame = &mut output[out_base..out_base + num_speakers];
            for (b, gains) in band_gains.iter().enumerate() {
                reference_accumulate_band(out_frame, gains, split.get(b));
            }
        };

        let render_params = frame.ramp_context.render_params();
        let lookup = |position: [f64; 3], size: [f32; 3], out: &mut Vec<Gains>| {
            SpeakerRenderStage::fill_band_gains(
                &stage.unified_table,
                None,
                &stage.render_bands,
                render_params,
                position,
                size,
                out,
            )
        };
        let idle = RampProgress {
            completed_units: 0,
            total_units: 0,
        };
        let ramp = &mut channel.ramp;
        let mut band_gains: Vec<Gains> = Vec::new();
        match frame.ramp_mode {
            RampMode::Off => {
                ramp.remaining_ramp_units = None;
                ramp.start_position = ramp.target_position;
                ramp.current_position = ramp.target_position;
                ramp.start_size = ramp.target_size;
                ramp.current_size = ramp.target_size;
                ramp.output_position = ramp.target_position;
                lookup(ramp.output_position, ramp.current_size, &mut band_gains);
                for sample_idx in 0..sample_length {
                    mix_sample(output, sample_idx, &band_gains);
                }
            }
            RampMode::Frame => {
                let progress = ramp.current_progress().unwrap_or(idle);
                frame
                    .ramp_strategy
                    .evaluate(ramp, progress, frame.ramp_context);
                lookup(ramp.output_position, ramp.current_size, &mut band_gains);
                for sample_idx in 0..sample_length {
                    mix_sample(output, sample_idx, &band_gains);
                }
                ramp.commit_output_position();
                ramp.advance_ramp(sample_length as u64);
            }
            RampMode::Sample => {
                band_gains.resize(stage.render_bands.len(), Gains::zeroed(num_speakers));
                let mut last_pos = [f64::NAN; 3];
                let mut last_size = [f32::NAN; 3];
                for sample_idx in 0..sample_length {
                    let progress = ramp.current_progress().unwrap_or(idle);
                    frame
                        .ramp_strategy
                        .evaluate(ramp, progress, frame.ramp_context);
                    let position = ramp.output_position;
                    let size = ramp.current_size;
                    if position != last_pos || size != last_size {
                        lookup(position, size, &mut band_gains);
                        last_pos = position;
                        last_size = size;
                    }
                    mix_sample(output, sample_idx, &band_gains);
                    ramp.commit_output_position();
                    ramp.advance_ramp(1);
                }
            }
            RampMode::Interp => {
                ramp.remaining_ramp_units = None;
                ramp.current_position = ramp.target_position;
                ramp.current_size = ramp.target_size;
                ramp.output_position = ramp.target_position;
                lookup(
                    ramp.target_position,
                    ramp.target_size,
                    &mut state.interp_end,
                );
                let n_bands = state.interp_end.len();
                if channel.interp_prev_gains.len() != n_bands {
                    channel.interp_prev_gains.clear();
                    channel
                        .interp_prev_gains
                        .extend_from_slice(&state.interp_end);
                }
                band_gains.resize(n_bands, Gains::zeroed(num_speakers));
                let inv_n = 1.0 / sample_length.max(1) as f32;
                for sample_idx in 0..sample_length {
                    let f = (sample_idx as f32 + 1.0) * inv_n;
                    for (b, slot) in band_gains.iter_mut().enumerate() {
                        let (s0, s1) = (&channel.interp_prev_gains[b], &state.interp_end[b]);
                        for (spk, g) in slot[..num_speakers].iter_mut().enumerate() {
                            *g = s0[spk] * (1.0 - f) + s1[spk] * f;
                        }
                    }
                    mix_sample(output, sample_idx, &band_gains);
                }
                channel.interp_prev_gains.clear();
                channel
                    .interp_prev_gains
                    .extend_from_slice(&state.interp_end);
            }
        }
        last_gains.push((ch, band_gains));
    }
    last_gains
}

fn bits(samples: &[f32]) -> Vec<u32> {
    samples.iter().map(|v| v.to_bits()).collect()
}

const N_BEDS: usize = 2;
const N_OBJECTS: usize = 6;
const N_CHANNELS: usize = N_BEDS + N_OBJECTS;
const MODES: [RampMode; 4] = [
    RampMode::Sample,
    RampMode::Frame,
    RampMode::Interp,
    RampMode::Off,
];

/// Metadata for one block: bed gains that step, objects on slow circles with
/// ramps shorter than, equal to and longer than the block — so ramps end
/// mid-block and span blocks — and gains that step too, to keep the slew
/// moving.
fn scene_events(block: usize, block_len: usize) -> Vec<SpatialChannelEvent> {
    let mut events: Vec<SpatialChannelEvent> = (0..N_BEDS)
        .map(|ch| SpatialChannelEvent {
            channel_idx: ch,
            is_bed: true,
            gain_db: Some(-3.0 - ((block + ch) % 3) as f32),
            ramp_length: None,
            size: None,
            position: None,
            sample_pos: None,
        })
        .collect();
    // Every third block carries no object metadata: the objects settle.
    if block % 3 == 2 {
        return events;
    }
    for obj in 0..N_OBJECTS {
        let degrees = obj as f64 * 37.0 + block as f64 * (0.4 + obj as f64 * 0.9);
        let az = degrees.to_radians();
        let ramp_length = [0, block_len * 5 / 8, block_len, block_len * 5 / 2][obj % 4];
        events.push(SpatialChannelEvent {
            channel_idx: N_BEDS + obj,
            is_bed: false,
            gain_db: Some(-(((block + obj) % 4) as f32)),
            ramp_length: Some(ramp_length as u32),
            size: Some([0.0, 0.0, 0.0]),
            position: Some([0.9 * az.sin(), 0.9 * az.cos(), (obj % 4) as f64 * 0.3]),
            sample_pos: Some(0),
        });
    }
    events
}

/// Mix the scene block by block through `mix_channels` and through the
/// reference, on the same stage, and require the same output bits, the same
/// ramp state and the same metered gains — in every ramp mode, with beds and
/// objects, a muted object, and trilinear and nearest-cell lookups. The frames
/// ask for a gain lookup per sample for moving objects, as the reference
/// does.
fn assert_mix_matches_reference(mut r: SpatialRenderer, label: &str, block_len: usize) {
    const BLOCKS_PER_MODE: usize = 9;
    let topology = r.control.active_topology();
    let num_speakers = r.speaker_stage.num_speakers;

    // The first beds of the layout, by speaker index.
    let mut labelled: Vec<_> = topology.label_to_speaker.iter().collect();
    labelled.sort_by_key(|&(_, &speaker_idx)| speaker_idx);
    let routing: Vec<ChannelRoute> = labelled
        .iter()
        .take(N_BEDS)
        .map(|&(&label, _)| ChannelRoute::Direct(label))
        .collect();
    assert_eq!(
        routing.len(),
        N_BEDS,
        "{label}: the layout must name speakers"
    );

    let ramp_context = RampContext::new(RampRenderParams {
        room_ratio: [1.0, 2.0, 0.5],
        room_ratio_rear: 2.0,
        room_ratio_lower: 0.5,
        room_ratio_center_blend: 0.0,
        use_distance_diffuse: false,
        distance_diffuse_threshold: 1.0,
        distance_diffuse_curve: 1.0,
        diffuse_mirror_axes: crate::spatial_vbap::MirrorAxes::default(),
        distance_model: crate::spatial_vbap::DistanceModel::Linear,
    });
    let strategy = PositionRampStrategy;
    let mut reference = ReferenceState::default();
    let mut object_params = vec![ObjectLiveParams::default(); N_CHANNELS];
    let mut silent = true;

    for block in 0..MODES.len() * BLOCKS_PER_MODE {
        let ramp_mode = MODES[block / BLOCKS_PER_MODE];
        let measure_breakdown = block % 2 == 1;
        // Nearest-cell lookups on the last third of each mode.
        r.speaker_stage
            .sync_position_interpolation(block % BLOCKS_PER_MODE < 6);
        object_params[N_BEDS + 1].muted = block % 5 == 3;

        let events = scene_events(block, block_len);
        for states in [&mut r.channel_states, &mut reference.channel_states] {
            SpatialRenderer::update_metadata(
                states,
                false,
                48_000,
                &events,
                &strategy,
                &ramp_context,
            )
            .unwrap();
        }
        let pcm = noise_block(N_CHANNELS, block_len, block);
        let frame = || SpeakerStageFrame {
            input_pcm: &pcm,
            input_channel_count: N_CHANNELS,
            sample_length: block_len,
            channel_routing: &routing,
            label_to_speaker: &topology.label_to_speaker,
            layout: &topology.speaker_layout,
            object_params: &object_params,
            ramp_mode,
            // The reference looks a moving object's gains up every sample.
            sample_ramp_stride: 1,
            ramp_strategy: &strategy,
            ramp_context: &ramp_context,
            log_object_positions: false,
            is_first: false,
            measure_breakdown,
        };

        let mut want = vec![0.0f32; block_len * num_speakers];
        let want_gains =
            reference_mix_channels(&r.speaker_stage, &mut reference, &frame(), &mut want);
        // `mix_channels` overwrites its output: hand it a dirty buffer.
        let mut got = vec![f32::NAN; block_len * num_speakers];
        let diag = r
            .speaker_stage
            .mix_channels(frame(), &mut r.channel_states, &mut got);

        assert!(
            bits(&got) == bits(&want),
            "{label}, {ramp_mode:?}, block {block}: the mix differs from the sample-major reference"
        );
        silent &= got.iter().all(|&v| v == 0.0);
        for (ch, (new, old)) in r
            .channel_states
            .iter()
            .zip(&reference.channel_states)
            .enumerate()
        {
            assert_eq!(
                (
                    new.ramp.output_position,
                    new.ramp.current_position,
                    new.ramp.current_size,
                    new.ramp.remaining_ramp_units,
                    new.slewed_gain.to_bits(),
                ),
                (
                    old.ramp.output_position,
                    old.ramp.current_position,
                    old.ramp.current_size,
                    old.ramp.remaining_ramp_units,
                    old.slewed_gain.to_bits(),
                ),
                "{label}, {ramp_mode:?}, block {block}, channel {ch}: channel state differs"
            );
        }
        if measure_breakdown {
            for (ch, band_gains) in &want_gains {
                let mut summed = Gains::zeroed(num_speakers);
                for gains in band_gains {
                    for (sum, &g) in summed.iter_mut().zip(gains.iter()) {
                        *sum += g;
                    }
                }
                let metered = diag
                    .object_gains
                    .iter()
                    .find(|(idx, _)| idx == ch)
                    .map(|(_, gains)| bits(gains));
                assert_eq!(
                    metered,
                    Some(bits(&summed)),
                    "{label}, {ramp_mode:?}, block {block}, channel {ch}: metered gains differ"
                );
                if *ch >= N_BEDS {
                    let metered_bands = diag
                        .object_band_gains
                        .iter()
                        .find(|(idx, _)| idx == ch)
                        .map(|(_, bands)| bands.iter().map(|g| bits(g)).collect::<Vec<_>>());
                    assert_eq!(
                        metered_bands,
                        Some(band_gains.iter().map(|g| bits(g)).collect()),
                        "{label}, {ramp_mode:?}, block {block}, channel {ch}: metered band gains differ"
                    );
                }
            }
        } else {
            assert!(diag.object_band_gains.is_empty());
        }
    }
    assert!(!silent, "{label}: the scene mixed silence");
}

#[test]
fn mix_is_bit_identical_to_the_sample_major_mix_on_a_cartesian_table() {
    assert_mix_matches_reference(build_table_renderer(true, true), "cartesian, LR4", 40);
}

#[test]
fn mix_is_bit_identical_to_the_sample_major_mix_on_a_polar_table() {
    assert_mix_matches_reference(build_table_renderer(false, true), "polar, LR4", 57);
}

/// No crossover: one full band, gains from the band's own evaluator.
#[test]
fn mix_is_bit_identical_to_the_sample_major_mix_without_crossover() {
    let r = build_table_renderer(true, false);
    assert!(r.speaker_stage.crossover_filter_bank.is_none());
    assert_mix_matches_reference(r, "no crossover", 40);
}

/// The linear-phase engine: the band split works in bursts and the beds go
/// through their compensation delay.
#[test]
fn mix_is_bit_identical_to_the_sample_major_mix_with_the_fir_crossover() {
    let mut r = build_table_renderer(true, true);
    r.control.live.write().crossover_type = CrossoverType::Fir;
    let topology = r.control.active_topology();
    let identity = Arc::as_ptr(&topology) as usize;
    r.speaker_stage
        .refresh_for_topology(&r.control, identity, &topology.speaker_layout)
        .unwrap();
    assert!(
        r.speaker_stage
            .crossover_filter_bank
            .as_ref()
            .is_some_and(|bank| bank.latency_samples() > 0)
    );
    assert_mix_matches_reference(r, "cartesian, FIR", 40);
}

/// The bus on its own: sources of every shape — gains that hold, gains that
/// ramp across the block, gains that change every sample — added in turn must
/// leave the sums a sample-major accumulation of the same products leaves,
/// whatever the signs of the zeros involved.
#[test]
fn bus_sums_match_a_sample_major_accumulation() {
    const SPEAKERS: usize = 7;
    const BLOCK: usize = 24;
    const BANDS: usize = 3;

    // Sparse gains with both zeros, negative values and a negative zero.
    let gain = |seed: u32| -> f32 {
        let x = seed.wrapping_mul(2_654_435_761);
        match (x >> 5) % 7 {
            0 | 1 | 2 => 0.0,
            3 => -0.0,
            4 => -(((x >> 9) & 0xfff) as f32 / 4095.0),
            _ => ((x >> 9) & 0xfff) as f32 / 4095.0,
        }
    };
    let gains_for = |seed: u32| -> Vec<Gains> {
        (0..BANDS as u32)
            .map(|b| {
                let mut g = Gains::zeroed(SPEAKERS);
                for (k, v) in g.iter_mut().enumerate() {
                    *v = gain(seed * 131 + b * 17 + k as u32);
                }
                g
            })
            .collect()
    };
    let bands_for = |seed: usize| -> Vec<Vec<f32>> {
        (0..BANDS)
            .map(|b| noise_block(1, BLOCK, seed * 7 + b))
            .collect()
    };
    let mut fractions = Vec::new();
    block_fractions(&mut fractions, BLOCK);

    let mut planar = vec![0.0f32; SPEAKERS * BLOCK];
    let mut got = vec![f32::NAN; SPEAKERS * BLOCK];
    let mut want = vec![0.0f32; SPEAKERS * BLOCK];
    let mut bus = MixBus::silent(&mut planar, &mut got, BLOCK, SPEAKERS);
    for (source, shape) in [0, 1, 2, 2, 0, 1, 1, 2, 0].into_iter().enumerate() {
        let bands = bands_for(source);
        let start = gains_for(source as u32);
        let end = gains_for(source as u32 + 100);
        // The gains each sample is mixed with, per shape.
        let gains_at = |sample_idx: usize| -> Vec<Gains> {
            match shape {
                0 => start.clone(),
                1 => {
                    let f = fractions[sample_idx];
                    let mut lerped = start.clone();
                    for (b, slot) in lerped.iter_mut().enumerate() {
                        for (k, g) in slot.iter_mut().enumerate() {
                            *g = start[b][k] * (1.0 - f) + end[b][k] * f;
                        }
                    }
                    lerped
                }
                _ => gains_for(source as u32 * 1000 + sample_idx as u32),
            }
        };
        match shape {
            0 => {
                // Two runs, to cover a range that does not start at zero.
                bus.add_constant(&bands, &start, 0..5);
                bus.add_constant(&bands, &start, 5..BLOCK);
            }
            1 => bus.add_lerp(&bands, &start, &end, &fractions, 0..BLOCK),
            _ => {
                for sample_idx in 0..BLOCK {
                    bus.add_constant(&bands, &gains_at(sample_idx), sample_idx..sample_idx + 1);
                }
            }
        }
        for sample_idx in 0..BLOCK {
            let gains = gains_at(sample_idx);
            let frame = &mut want[sample_idx * SPEAKERS..(sample_idx + 1) * SPEAKERS];
            for (band, gains) in bands.iter().zip(&gains) {
                reference_accumulate_band(frame, gains, band[sample_idx]);
            }
        }
    }
    bus.finish();
    assert!(bits(&got) == bits(&want), "bus sums differ");
    assert!(want.iter().any(|&v| v != 0.0));
}

/// The object test is mixed through the same bus, onto a block that already
/// holds programme audio: `inject_object_test` against the sample-major
/// interpolated mix it replaced, with and without a crossover, summed onto the
/// programme and replacing it.
#[test]
fn object_test_is_bit_identical_to_the_sample_major_mix() {
    use crate::live_params::{ObjectTest, TestIsolation};
    const BLOCK: usize = 40;

    for band_limited in [true, false] {
        let mut r = build_table_renderer(true, band_limited);
        let stage = &mut r.speaker_stage;
        let num_speakers = stage.num_speakers;
        let render_params = RampRenderParams {
            room_ratio: [1.0, 2.0, 0.5],
            room_ratio_rear: 2.0,
            room_ratio_lower: 0.5,
            room_ratio_center_blend: 0.0,
            use_distance_diffuse: false,
            distance_diffuse_threshold: 1.0,
            distance_diffuse_curve: 1.0,
            diffuse_mirror_axes: crate::spatial_vbap::MirrorAxes::default(),
            distance_model: crate::spatial_vbap::DistanceModel::Linear,
        };
        let mut prev: Vec<Gains> = Vec::new();
        let mut end: Vec<Gains> = Vec::new();
        let mut filter_states: Option<CrossoverStates> = None;

        for block in 0..12 {
            let az = (block as f32 * 23.0).to_radians();
            let position = [0.8 * az.sin(), 0.8 * az.cos(), 0.1 * (block % 4) as f32];
            let test = ObjectTest {
                position,
                size: [0.0; 3],
                level: 0.5,
                isolation: if block % 4 == 3 {
                    TestIsolation::TestOnly
                } else {
                    TestIsolation::WithProgramme
                },
                signal: Default::default(),
            };
            let noise = noise_block(1, BLOCK, block);
            let programme = noise_block(num_speakers, BLOCK, 100 + block);

            let mut want = programme.clone();
            if test.isolation != TestIsolation::WithProgramme {
                want.fill(0.0);
            }
            SpeakerRenderStage::fill_band_gains(
                &stage.unified_table,
                None,
                &stage.render_bands,
                render_params,
                position.map(|v| v as f64),
                test.size,
                &mut end,
            );
            if prev.len() != end.len() {
                prev = end.clone();
            }
            let bank = stage.crossover_filter_bank.as_ref();
            let inv_n = 1.0 / BLOCK as f32;
            for sample_idx in 0..BLOCK {
                let f = (sample_idx as f32 + 1.0) * inv_n;
                let split = match bank {
                    Some(bank) => {
                        let states = bank.ensure_states(&mut filter_states);
                        bank.process_sample(noise[sample_idx], states)
                    }
                    None => SmallBands::single(noise[sample_idx]),
                };
                let frame = &mut want[sample_idx * num_speakers..(sample_idx + 1) * num_speakers];
                for (b, (s0, s1)) in prev.iter().zip(&end).enumerate() {
                    let mut gains = Gains::zeroed(num_speakers);
                    for (spk, g) in gains.iter_mut().enumerate() {
                        *g = s0[spk] * (1.0 - f) + s1[spk] * f;
                    }
                    reference_accumulate_band(frame, &gains, split.get(b));
                }
            }
            prev = end.clone();

            let mut got = programme.clone();
            let active = stage.inject_object_test(
                Some(test),
                Some(&crate::object_test::ObjectTestBlock {
                    pcm: &noise,
                    peak_dbfs: 0.0,
                    rms_dbfs: 0.0,
                    position,
                }),
                render_params,
                &mut got,
            );
            assert!(active);
            assert!(
                bits(&got) == bits(&want),
                "band_limited={band_limited}, block {block}: the object test differs from the \
                 sample-major reference"
            );
            assert!(got != programme, "the test added nothing");
        }
    }
}

/// `mix_sample_ramp` on its own, with a gain law that is linear in the
/// position: one object on two speakers, gains `(x, 1 − x)`, mixed from a
/// constant input of one so that the bus reads back the gains each sample was
/// mixed with. Returns those gains for speaker 0 and the number of lookups.
struct RampProbe {
    ramp: crate::ramp_strategy::ChannelRampState,
    carry: GainCarry,
    pass: u64,
    stride: usize,
}

impl RampProbe {
    fn new(stride: usize) -> Self {
        Self {
            ramp: Default::default(),
            carry: GainCarry::default(),
            pass: 0,
            stride,
        }
    }

    fn context() -> RampContext {
        RampContext::new(RampRenderParams {
            room_ratio: [1.0; 3],
            room_ratio_rear: 1.0,
            room_ratio_lower: 1.0,
            room_ratio_center_blend: 0.0,
            use_distance_diffuse: false,
            distance_diffuse_threshold: 1.0,
            distance_diffuse_curve: 1.0,
            diffuse_mirror_axes: crate::spatial_vbap::MirrorAxes::default(),
            distance_model: crate::spatial_vbap::DistanceModel::None,
        })
    }

    /// Start a ramp of `ramp_length` samples towards `x`.
    fn move_to(&mut self, x: f64, ramp_length: u64) {
        PositionRampStrategy.update_target(
            &mut self.ramp,
            crate::ramp_strategy::RampTarget {
                position: [x, 0.0, 0.0],
                size: [0.0; 3],
                ramp_length,
            },
            None,
            &Self::context(),
        );
    }

    /// Mix one block; `(gains of speaker 0 per sample, positions per sample,
    /// lookups)`.
    fn block(&mut self, len: usize) -> (Vec<f32>, Vec<f64>, usize) {
        const SPEAKERS: usize = 2;
        self.pass += 1;
        let bands = vec![vec![1.0f32; len]];
        let mut planar = vec![0.0f32; SPEAKERS * len];
        let mut output = vec![0.0f32; SPEAKERS * len];
        let mut band_gains = vec![Gains::zeroed(SPEAKERS)];
        let mut segment_end = Vec::new();
        let mut lookups = 0;
        let mut bus = MixBus::silent(&mut planar, &mut output, len, SPEAKERS);
        // The positions the ramp goes through, read off a copy of it.
        let mut positions = Vec::with_capacity(len);
        let mut walk = self.ramp.clone();
        for _ in 0..len {
            let progress = walk.current_progress().unwrap_or(RampProgress {
                completed_units: 0,
                total_units: 0,
            });
            PositionRampStrategy.evaluate(&mut walk, progress, &Self::context());
            positions.push(walk.output_position[0]);
            walk.commit_output_position();
            walk.advance_ramp(1);
        }
        mix_sample_ramp(
            &mut bus,
            &bands,
            len,
            &mut self.ramp,
            &PositionRampStrategy,
            &Self::context(),
            self.stride,
            &mut self.carry,
            self.pass,
            &mut band_gains,
            &mut segment_end,
            |position, _size, out| {
                lookups += 1;
                out.clear();
                let mut gains = Gains::zeroed(SPEAKERS);
                gains.set(0, position[0] as f32);
                gains.set(1, 1.0 - position[0] as f32);
                out.push(gains);
            },
        );
        bus.finish();
        assert_eq!(
            self.ramp.output_position[0].to_bits(),
            walk.output_position[0].to_bits(),
            "the mix must leave the ramp where a per-sample walk leaves it"
        );
        let mixed = output.chunks_exact(SPEAKERS).map(|f| f[0]).collect();
        (mixed, positions, lookups)
    }
}

/// With a gain law linear in the position, interpolating between lookups
/// loses nothing: every sample must come out with the gains of its own
/// position, to rounding — through a ramp that spans blocks whose length is
/// not a multiple of the stride, so the first segment of a block has to start
/// from the gains the previous block ended on.
#[test]
fn stride_interpolation_is_exact_for_a_linear_gain_law_across_blocks() {
    const BLOCK: usize = 37;
    let mut probe = RampProbe::new(crate::config_fields::sample_ramp_stride::DEFAULT);
    probe.move_to(0.2, 0);
    let (_, _, lookups) = probe.block(BLOCK);
    assert_eq!(lookups, 1, "a settled block is one lookup");

    probe.move_to(0.9, 5 * BLOCK as u64 / 2);
    let mut lookups_per_block = Vec::new();
    for block in 0..4 {
        let (mixed, positions, lookups) = probe.block(BLOCK);
        lookups_per_block.push(lookups);
        for (sample_idx, (&gain, &x)) in mixed.iter().zip(&positions).enumerate() {
            assert!(
                (gain - x as f32).abs() < 2e-6,
                "block {block}, sample {sample_idx}: mixed with {gain}, position {x}"
            );
        }
    }
    // First block: the settled gains are looked up on the first sample, then
    // 36 moving samples make five segments. Second block: 37 moving samples
    // from the carried gains, five segments. Third: the ramp ends on its 19th
    // sample — two full segments, one of three samples closed where the
    // movement stops, and nothing more. Fourth: settled.
    assert_eq!(lookups_per_block, [6, 5, 3, 1]);
}

/// The samples of a segment are interpolated, but its ends are not: the last
/// sample of every segment, the last sample of a ramp and every sample after
/// it are mixed with the gains of their own position, bit for bit. Checked
/// with a gain law that is not linear, so an interpolated sample shows.
#[test]
fn stride_keeps_segment_ends_and_settled_samples_exact() {
    const BLOCK: usize = 40;
    let law = |x: f64| ((x * 7.0).sin() * 0.5 + 0.5) as f32;
    let run = |stride: usize| -> Vec<f32> {
        let mut ramp = crate::ramp_strategy::ChannelRampState::default();
        let mut carry = GainCarry::default();
        let mut out = Vec::new();
        let context = RampProbe::context();
        for block in 0..4u64 {
            if block == 1 {
                PositionRampStrategy.update_target(
                    &mut ramp,
                    crate::ramp_strategy::RampTarget {
                        position: [0.8, 0.0, 0.0],
                        size: [0.0; 3],
                        ramp_length: 53,
                    },
                    None,
                    &context,
                );
            }
            let bands = vec![vec![1.0f32; BLOCK]];
            let mut planar = vec![0.0f32; BLOCK];
            let mut output = vec![0.0f32; BLOCK];
            let mut band_gains = vec![Gains::zeroed(1)];
            let mut segment_end = Vec::new();
            let mut bus = MixBus::silent(&mut planar, &mut output, BLOCK, 1);
            mix_sample_ramp(
                &mut bus,
                &bands,
                BLOCK,
                &mut ramp,
                &PositionRampStrategy,
                &context,
                stride,
                &mut carry,
                block + 1,
                &mut band_gains,
                &mut segment_end,
                |position, _size, out| {
                    out.clear();
                    let mut gains = Gains::zeroed(1);
                    gains.set(0, law(position[0]));
                    out.push(gains);
                },
            );
            bus.finish();
            out.extend_from_slice(&output);
        }
        out
    };
    let exact = run(1);
    let strided = run(crate::config_fields::sample_ramp_stride::DEFAULT);
    // Block 0 is settled. The ramp starts on sample 40, still at the settled
    // position, moves over samples 41..=92 and lands on its target on sample
    // 93, where the object stays.
    let mut interpolated = 0;
    for (sample_idx, (a, b)) in exact.iter().zip(&strided).enumerate() {
        let moving = (41..94).contains(&sample_idx);
        // Segments of the first moving block start on its second sample,
        // those of the next block on its first; the last one ends where the
        // movement does.
        let segment_end = match sample_idx {
            41..80 => (sample_idx - 41) % 8 == 7 || sample_idx == 79,
            80..94 => (sample_idx - 80) % 8 == 7 || sample_idx == 93,
            _ => true,
        };
        if !moving || segment_end {
            assert_eq!(
                a.to_bits(),
                b.to_bits(),
                "sample {sample_idx} must have the gains of its own position"
            );
        } else if a != b {
            interpolated += 1;
            assert!((a - b).abs() < 0.05, "sample {sample_idx}: {a} vs {b}");
        }
    }
    assert!(interpolated > 20, "the stride interpolated nothing");
}

/// Gains kept for the next block are only good for the block right after:
/// once a pass is skipped, a moving block starts from a lookup on its first
/// sample instead.
#[test]
fn a_skipped_pass_drops_the_carried_gains() {
    const BLOCK: usize = 40;
    let mut probe = RampProbe::new(crate::config_fields::sample_ramp_stride::DEFAULT);
    probe.move_to(0.9, 10 * BLOCK as u64);
    assert_eq!(probe.block(BLOCK).2, 6, "first moving block: no carry");
    assert_eq!(probe.block(BLOCK).2, 5, "second: starts from the carry");
    probe.pass += 1;
    assert_eq!(probe.block(BLOCK).2, 6, "after a skipped pass: no carry");
}

/// Renders of the moving scene through the whole renderer, stride against a
/// lookup per sample: `(peak deviation, RMS deviation, peak level)`, linear.
fn stride_deviation(cartesian: bool, band_limited: bool, moving: bool) -> (f32, f32, f32) {
    const BLOCK: usize = 40;
    const BLOCKS: usize = 60;
    const N: usize = 8;
    let render = |stride: usize| -> Vec<f32> {
        let mut r = build_table_renderer(cartesian, band_limited);
        r.control.live.write().ramp_mode = RampMode::Sample;
        r.control.live.write().sample_ramp_stride = stride;
        let mut out = Vec::new();
        let mut buf = Vec::new();
        for block in 0..BLOCKS {
            let events: Vec<SpatialChannelEvent> = if block == 0 || (moving && block % 4 != 3) {
                (0..N)
                    .map(|obj| {
                        // Up to a third of a turn per second, plus a slow rise.
                        let degrees = obj as f64 * 37.0
                            + block as f64
                                * (0.02 + obj as f64 * 0.012)
                                * if moving { 1.0 } else { 0.0 };
                        let az = degrees.to_radians();
                        SpatialChannelEvent {
                            channel_idx: obj,
                            is_bed: false,
                            gain_db: Some(0.0),
                            // Ramps as long as the block, shorter, and longer.
                            // A jump onto the first position, and whenever
                            // nothing is meant to move.
                            ramp_length: Some(if moving && block > 0 {
                                [BLOCK, BLOCK * 5 / 8, BLOCK * 5 / 2][obj % 3] as u32
                            } else {
                                0
                            }),
                            size: Some([0.0; 3]),
                            position: Some([
                                0.9 * az.sin(),
                                0.9 * az.cos(),
                                0.1 + 0.002 * block as f64 * (obj % 3) as f64,
                            ]),
                            sample_pos: Some(0),
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let pcm = noise_block(N, BLOCK, block);
            let frame = r.render_frame(&pcm, N, &events, buf, false).unwrap();
            out.extend_from_slice(&frame.samples);
            buf = frame.samples;
        }
        out
    };
    let exact = render(1);
    let strided = render(crate::config_fields::sample_ramp_stride::DEFAULT);
    let mut peak = 0.0f32;
    let mut sum_sq = 0.0f64;
    let mut level = 0.0f32;
    for (a, b) in exact.iter().zip(&strided) {
        let d = (a - b).abs();
        peak = peak.max(d);
        sum_sq += (d as f64) * (d as f64);
        level = level.max(a.abs());
    }
    (peak, (sum_sq / exact.len() as f64).sqrt() as f32, level)
}

/// An object that is not ramping must not notice the stride: scenes where
/// objects only ever jump (zero-length ramps) or hold render the same bits as
/// with a lookup per sample.
#[test]
fn stride_leaves_settled_objects_bit_identical() {
    for (cartesian, band_limited) in [(true, true), (true, false)] {
        let (peak, _, level) = stride_deviation(cartesian, band_limited, false);
        assert!(level > 0.01, "the scene rendered silence");
        assert_eq!(
            peak, 0.0,
            "cartesian={cartesian}, band_limited={band_limited}: settled objects changed"
        );
    }
}

/// On moving objects the stride departs from a lookup per sample by what the
/// gains curve inside eight samples, which is little: the deviation stays
/// more than 70 dB under the signal on every table geometry.
#[test]
fn stride_stays_close_to_a_lookup_per_sample_on_moving_objects() {
    for (cartesian, band_limited) in [(true, true), (false, true), (true, false)] {
        let (peak, rms, level) = stride_deviation(cartesian, band_limited, true);
        let db = |v: f32| 20.0 * v.max(1e-12).log10();
        eprintln!(
            "stride deviation, cartesian={cartesian}, band_limited={band_limited}: \
             peak {:.1} dBFS, rms {:.1} dBFS (signal peak {:.1} dBFS)",
            db(peak),
            db(rms),
            db(level)
        );
        assert!(peak > 0.0, "the scene never interpolated");
        assert!(
            db(peak) < db(level) - 70.0,
            "cartesian={cartesian}, band_limited={band_limited}: peak deviation {:.1} dBFS \
             against a signal peak of {:.1} dBFS",
            db(peak),
            db(level)
        );
    }
}
