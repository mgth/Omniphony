/// What a table build carries from one cell to the next along a row, for gain
/// models whose solver iterates and converges faster when started from the
/// solution of a neighbouring position.
///
/// A precomputed table is built row by row: rows are independent (and built in
/// parallel), the cells of a row are evaluated in order, and each row starts
/// from an empty hint. The table therefore does not depend on how the rows are
/// scheduled.
///
/// The hint is a short list of slots. A model that wants to remember something
/// claims the next slot with [`slot`](Self::slot), reads what the previous cell
/// left there and overwrites it. Slots are handed out in call order, and that
/// order is the same for every cell of a row, so each model in a composition
/// (the two inner models of the hybrid backend, the direct and mirrored
/// evaluations of the distance-diffuse stage) keeps a slot of its own.
pub struct NeighbourHint {
    slots: [HintSlot; Self::SLOTS],
    cursor: usize,
}

/// One model's memory of the previous cell: a list of values, usually one per
/// speaker, or nothing.
#[derive(Default)]
pub struct HintSlot {
    /// Grown by the first [`store`](Self::store) and reused from then on: a
    /// table build keeps one hint per worker, so the cells allocate nothing.
    values: Vec<f32>,
}

impl NeighbourHint {
    /// Enough for the deepest built-in composition: two inner models, each
    /// evaluated at the position and at its mirror image.
    pub const SLOTS: usize = 4;

    /// An empty hint: the state at the first cell of a row.
    pub fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| HintSlot::default()),
            cursor: 0,
        }
    }

    /// Start another row with this hint: it is empty again, as a new one is.
    pub fn begin_row(&mut self) {
        self.slots.iter_mut().for_each(HintSlot::clear);
        self.cursor = 0;
    }

    /// Start the next cell: slots are handed out from the first one again.
    /// Called by whoever walks the row, once before each cell.
    pub fn begin_cell(&mut self) {
        self.cursor = 0;
    }

    /// The next slot in call order, or `None` once they are all claimed (the
    /// model then solves from scratch).
    pub fn slot(&mut self) -> Option<&mut HintSlot> {
        let slot = self.slots.get_mut(self.cursor)?;
        self.cursor += 1;
        Some(slot)
    }
}

impl Default for NeighbourHint {
    fn default() -> Self {
        Self::new()
    }
}

impl HintSlot {
    /// What the previous cell stored, if it stored exactly `len` values.
    pub fn values(&self, len: usize) -> Option<&[f32]> {
        (len != 0 && self.values.len() == len).then_some(&self.values[..])
    }

    /// Remember `values` for the next cell.
    pub fn store(&mut self, values: &[f32]) {
        self.values.clear();
        self.values.extend_from_slice(values);
    }

    /// Leave nothing for the next cell.
    pub fn clear(&mut self) {
        self.values.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::backend_conformance::neutral_request;
    use crate::render_backend::{
        BarycenterBackend, CartesianEvaluationConfig, EvaluationBuildConfig,
        ExperimentalDistanceBackend, GainModel, PolarEvaluationConfig, PreparedEvaluator,
        RenderRequest, SampledCartesianEvaluator, SampledPolarEvaluator, SizeToSpreadMode,
        build_decorated_model,
    };
    use crate::spatial_vbap::{DistanceMetric, MirrorAxes};

    #[test]
    fn slots_are_handed_out_in_call_order_for_every_cell() {
        let mut hint = NeighbourHint::new();
        for cell in 0..3 {
            hint.begin_cell();
            for slot in 0..NeighbourHint::SLOTS {
                let held = hint.slot().expect("a free slot");
                if cell == 0 {
                    assert!(held.values(2).is_none(), "a row starts empty");
                } else {
                    assert_eq!(held.values(2), Some(&[slot as f32, cell as f32 - 1.0][..]));
                }
                held.store(&[slot as f32, cell as f32]);
            }
            assert!(hint.slot().is_none(), "no slot left to claim");
        }
    }

    #[test]
    fn a_slot_returns_only_what_was_stored_at_that_length() {
        let mut hint = NeighbourHint::new();
        let slot = hint.slot().unwrap();
        assert!(slot.values(0).is_none());
        slot.store(&[0.25, 0.75]);
        assert_eq!(slot.values(2), Some(&[0.25, 0.75][..]));
        assert!(slot.values(3).is_none());
        slot.clear();
        assert!(slot.values(2).is_none());
        // As many values as a layout has speakers, however many that is.
        slot.store(&[0.5; 100]);
        assert_eq!(slot.values(100), Some(&[0.5; 100][..]));
        assert!(slot.values(99).is_none());
    }

    #[test]
    fn a_row_begun_again_starts_empty() {
        let mut hint = NeighbourHint::new();
        hint.slot().unwrap().store(&[1.0, 2.0]);
        hint.slot().unwrap().store(&[3.0]);
        hint.begin_row();
        for _ in 0..NeighbourHint::SLOTS {
            let slot = hint
                .slot()
                .expect("slots are handed out from the first again");
            assert!(slot.values(1).is_none() && slot.values(2).is_none());
        }
    }

    fn speakers() -> Vec<[f32; 3]> {
        vec![
            [-1.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, -0.15],
            [1.0, -1.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, -1.0, 1.0],
            [-1.0, -1.0, 1.0],
            [-0.48, 0.0, 1.0],
            [0.55, 0.0, 1.0],
        ]
    }

    fn config(template: RenderRequest) -> EvaluationBuildConfig {
        EvaluationBuildConfig {
            request_template: template,
            position_interpolation: true,
            cartesian: CartesianEvaluationConfig {
                x_size: 17,
                y_size: 9,
                z_size: 4,
                z_neg_size: 1,
            },
            polar: PolarEvaluationConfig {
                azimuth_values: 24,
                elevation_values: 7,
                distance_values: 4,
                distance_max: 1.0,
                allow_negative_z: true,
            },
            distance_model_metric: DistanceMetric::default(),
            distance_diffuse_metric: DistanceMetric::default(),
            object_size_intervals: 0,
            object_size_mode: SizeToSpreadMode::default(),
        }
    }

    /// A warped room and the diffuse stage on, so that every cell reaches the
    /// solver twice (source and mirror image) through two decorators.
    fn diffuse_template() -> RenderRequest {
        RenderRequest {
            room_ratio: [1.0, 2.0, 1.0],
            room_ratio_lower: 0.47,
            room_ratio_center_blend: 0.59,
            use_distance_diffuse: true,
            diffuse_mirror_axes: MirrorAxes::default(),
            ..neutral_request()
        }
    }

    fn in_pool<T: Send>(threads: usize, build: impl FnOnce() -> T + Send) -> T {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .expect("thread pool")
            .install(build)
    }

    fn bits(gains: &[f32]) -> Vec<u32> {
        gains.iter().map(|gain| gain.to_bits()).collect()
    }

    #[test]
    fn tables_are_the_same_for_any_thread_count() {
        let config = config(diffuse_template());
        let model = || -> Arc<dyn GainModel> {
            build_decorated_model(Box::new(BarycenterBackend::new(speakers(), 0.5)), &config)
        };

        let cartesian = |threads| {
            in_pool(threads, || {
                let table = SampledCartesianEvaluator::new(model(), &config);
                bits(table.cartesian_parts().expect("cartesian table").gains)
            })
        };
        let single = cartesian(1);
        assert!(!single.is_empty());
        for threads in [2, 3, 8, 8] {
            assert!(cartesian(threads) == single, "cartesian, {threads} threads");
        }

        let polar = |threads| {
            in_pool(threads, || {
                let table = SampledPolarEvaluator::new(model(), &config);
                bits(table.polar_parts().expect("polar table").gains)
            })
        };
        let single = polar(1);
        assert!(!single.is_empty());
        for threads in [2, 3, 8, 8] {
            assert!(polar(threads) == single, "polar, {threads} threads");
        }
    }

    #[test]
    fn a_table_built_along_rows_equals_one_solve_per_cell() {
        let config = config(RenderRequest {
            room_ratio: [1.0, 2.0, 1.0],
            ..neutral_request()
        });
        let backend = Arc::new(BarycenterBackend::new(speakers(), 0.5));
        let table = SampledCartesianEvaluator::new(backend.clone(), &config);
        let parts = table.cartesian_parts().expect("cartesian table");
        let (nx, ny) = (parts.x.len(), parts.y.len());
        for (cell, gains) in parts.gains.chunks(parts.speaker_count).enumerate() {
            let mut request = config.request_template;
            request.adm_position = [
                table.x_positions[cell % nx] as f64,
                table.y_positions[(cell / nx) % ny] as f64,
                table.z_positions[cell / (nx * ny)] as f64,
            ];
            let cold = backend.gains_at(&request);
            assert!(bits(gains) == bits(&cold), "cell {cell}");
        }
    }

    #[test]
    fn a_model_without_the_hook_is_sampled_cell_by_cell() {
        let config = config(neutral_request());
        let backend = Arc::new(ExperimentalDistanceBackend::new(
            speakers(),
            crate::live_params::ExperimentalDistanceLiveParams::default(),
        ));
        let table = SampledCartesianEvaluator::new(backend.clone(), &config);
        let parts = table.cartesian_parts().expect("cartesian table");
        let (nx, ny) = (parts.x.len(), parts.y.len());
        for (cell, gains) in parts.gains.chunks(parts.speaker_count).enumerate() {
            let mut request = config.request_template;
            request.adm_position = [
                table.x_positions[cell % nx] as f64,
                table.y_positions[(cell / nx) % ny] as f64,
                table.z_positions[cell / (nx * ny)] as f64,
            ];
            let direct = backend.gains_at(&request);
            assert!(bits(gains) == bits(&direct), "cell {cell}");
        }
    }
}
