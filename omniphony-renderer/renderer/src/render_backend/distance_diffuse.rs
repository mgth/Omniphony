use anyhow::Result;

use super::{BackendCapabilities, GainModel, RenderRequest, RenderResponse};
use crate::spatial_vbap::{DistanceMetric, Gains};
use crate::speaker_layout::SpeakerLayout;

/// Decorator that applies distance-based mirrored diffuse blending to the gains
/// produced by any inner gain model.
///
/// The source's gains are blended with the gains of a mirror image obtained by
/// negating the ADM axes selected in `diffuse_mirror_axes`; the mix is driven by
/// the ADM distance, pulling toward a diffuse field as the source approaches the
/// centre. The default `xy` reproduces the historical horizontal antipode
/// `(-x, -y, z)` — a half-turn about the vertical axis — while `xyz` gives a true
/// point inversion through the origin and a single flip gives a reflection in the
/// plane normal to that axis. Like the distance model, this used to live inside
/// the VBAP backend; extracting it into a decorator makes it available to every
/// backend.
///
/// Applied *under* the distance-model decorator so the energy renormalization
/// here does not cancel the distance attenuation. When `use_distance_diffuse` is
/// false, or when no axis is flipped, it is a no-op (and skips the second backend
/// evaluation).
pub struct DistanceDiffuseModel {
    inner: Box<dyn GainModel>,
    metric: DistanceMetric,
}

impl DistanceDiffuseModel {
    pub fn new(inner: Box<dyn GainModel>, metric: DistanceMetric) -> Self {
        Self { inner, metric }
    }
}

impl GainModel for DistanceDiffuseModel {
    fn backend_id(&self) -> &'static str {
        self.inner.backend_id()
    }

    fn backend_label(&self) -> &'static str {
        self.inner.backend_label()
    }

    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            supports_distance_diffuse: true,
            ..self.inner.capabilities()
        }
    }

    fn speaker_count(&self) -> usize {
        self.inner.speaker_count()
    }

    fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
        // No flip means the mirror is the source itself, so the blend would
        // renormalize straight back to the direct gains: skip both the second
        // evaluation and the mixing.
        if !req.use_distance_diffuse || req.diffuse_mirror_axes.is_identity() {
            return self.inner.compute_gains(req);
        }

        let direct = self.inner.compute_gains(req).gains;

        // Mirror in ADM space, i.e. on the authored position, before the room
        // scaling the backend applies downstream. Note the warp is only an odd
        // function when `room_ratio_rear` and `room_ratio_lower` are 1: with an
        // asymmetric room the mirror does not land on the geometric image of the
        // source once warped. Reflecting what was authored is the intended
        // behaviour — the alternative would make the diffuse field depend on the
        // room proportions.
        let mut mirror_req = *req;
        mirror_req.adm_position = req.diffuse_mirror_axes.reflect(req.adm_position);
        let mirror = self.inner.compute_gains(&mirror_req).gains;

        // Blend weight from the (raw) ADM distance, under the selected metric.
        let adm_dist = self.metric.measure(req.adm_position.map(|v| v as f32));
        let t = (adm_dist / req.distance_diffuse_threshold.max(1e-6))
            .min(1.0)
            .powf(req.distance_diffuse_curve);
        let alpha = 0.5 + 0.5 * t;
        let w_direct = alpha.sqrt();
        let w_mirror = (1.0 - alpha).sqrt();

        let n = direct.len().min(mirror.len());
        let mut blended = Gains::zeroed(self.inner.speaker_count());
        let mut energy_direct = 0.0f32;
        let mut energy_blended = 0.0f32;
        for i in 0..n {
            let g = w_direct * direct[i] + w_mirror * mirror[i];
            blended.set(i, g);
            energy_direct += direct[i] * direct[i];
            energy_blended += g * g;
        }

        if energy_blended > 1e-12 {
            let scale = (energy_direct / energy_blended).sqrt();
            for g in blended.iter_mut() {
                *g *= scale;
            }
        }

        RenderResponse { gains: blended }
    }

    fn save_to_file(&self, path: &std::path::Path, speaker_layout: &SpeakerLayout) -> Result<()> {
        self.inner.save_to_file(path, speaker_layout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_backend::BarycenterBackend;

    use crate::spatial_vbap::MirrorAxes;

    fn request(position: [f64; 3], diffuse: bool) -> RenderRequest {
        request_with_axes(position, diffuse, MirrorAxes::default())
    }

    fn request_with_axes(
        position: [f64; 3],
        diffuse: bool,
        diffuse_mirror_axes: MirrorAxes,
    ) -> RenderRequest {
        RenderRequest {
            adm_position: position,
            event_size: [0.0, 0.0, 0.0],
            room_ratio: [1.0, 1.0, 1.0],
            room_ratio_rear: 1.0,
            room_ratio_lower: 1.0,
            room_ratio_center_blend: 0.0,
            use_distance_diffuse: diffuse,
            distance_diffuse_threshold: 1.0,
            distance_diffuse_curve: 1.0,
            diffuse_mirror_axes,
            distance_model: crate::spatial_vbap::DistanceModel::None,
        }
    }

    fn speakers() -> Vec<[f32; 3]> {
        vec![
            [-1.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
        ]
    }

    /// Inner model that records every position it is asked about, so a test can
    /// assert exactly which mirror the decorator derived — the contract the axis
    /// switches control.
    #[derive(Default)]
    struct RecordingBackend {
        seen: std::sync::Mutex<Vec<[f64; 3]>>,
    }

    impl GainModel for RecordingBackend {
        fn backend_id(&self) -> &'static str {
            "recording"
        }
        fn backend_label(&self) -> &'static str {
            "Recording"
        }
        fn capabilities(&self) -> BackendCapabilities {
            BackendCapabilities::default()
        }
        fn speaker_count(&self) -> usize {
            2
        }
        fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
            self.seen.lock().unwrap().push(req.adm_position);
            let mut gains = Gains::zeroed(2);
            gains.set(0, 1.0);
            RenderResponse { gains }
        }
        fn save_to_file(&self, _path: &std::path::Path, _layout: &SpeakerLayout) -> Result<()> {
            Ok(())
        }
    }

    /// Positions handed to the inner model for one evaluation: `[direct]` when
    /// the stage short-circuits, `[direct, mirror]` otherwise.
    fn positions_evaluated(position: [f64; 3], axes: MirrorAxes) -> Vec<[f64; 3]> {
        let inner = std::sync::Arc::new(RecordingBackend::default());
        struct Shared(std::sync::Arc<RecordingBackend>);
        impl GainModel for Shared {
            fn backend_id(&self) -> &'static str {
                self.0.backend_id()
            }
            fn backend_label(&self) -> &'static str {
                self.0.backend_label()
            }
            fn capabilities(&self) -> BackendCapabilities {
                self.0.capabilities()
            }
            fn speaker_count(&self) -> usize {
                self.0.speaker_count()
            }
            fn compute_gains(&self, req: &RenderRequest) -> RenderResponse {
                self.0.compute_gains(req)
            }
            fn save_to_file(&self, path: &std::path::Path, layout: &SpeakerLayout) -> Result<()> {
                self.0.save_to_file(path, layout)
            }
        }
        let model =
            DistanceDiffuseModel::new(Box::new(Shared(inner.clone())), DistanceMetric::Spherical);
        model.compute_gains(&request_with_axes(position, true, axes));
        let seen = inner.seen.lock().unwrap().clone();
        seen
    }

    fn wrapped() -> DistanceDiffuseModel {
        DistanceDiffuseModel::new(
            Box::new(BarycenterBackend::new(speakers(), 0.0)),
            DistanceMetric::Spherical,
        )
    }

    #[test]
    fn disabled_is_a_noop() {
        let position = [0.4, 0.2, 0.1];
        let decorated = wrapped().compute_gains(&request(position, false)).gains;
        let bare = BarycenterBackend::new(speakers(), 0.0)
            .compute_gains(&request(position, false))
            .gains;
        for (a, b) in decorated.iter().zip(bare.iter()) {
            assert!((a - b).abs() < 1e-6, "{a} vs {b}");
        }
    }

    #[test]
    fn enabled_preserves_direct_energy_on_a_non_vbap_backend() {
        // The renorm targets the direct energy, so the blended vector keeps the
        // direct backend's energy regardless of the mirror contribution.
        let position = [0.3, 0.1, 0.2];
        let blended = wrapped().compute_gains(&request(position, true)).gains;
        let direct = BarycenterBackend::new(speakers(), 0.0)
            .compute_gains(&request(position, true))
            .gains;
        let energy_blended: f32 = blended.iter().map(|g| g * g).sum();
        let energy_direct: f32 = direct.iter().map(|g| g * g).sum();
        assert!(
            (energy_blended - energy_direct).abs() < 1e-4,
            "blended={energy_blended} direct={energy_direct}"
        );
    }

    #[test]
    fn capabilities_advertise_distance_diffuse() {
        assert!(wrapped().capabilities().supports_distance_diffuse);
    }

    #[test]
    fn every_axis_combination_selects_its_own_mirror() {
        // Exhaustive over the eight sign patterns: one flip reflects in the
        // plane normal to that axis, two flips are a half-turn about the third,
        // and all three invert through the origin.
        let position = [0.3, 0.2, 0.1];
        for (x, y, z) in [
            (false, false, true),
            (false, true, false),
            (false, true, true),
            (true, false, false),
            (true, false, true),
            (true, true, false),
            (true, true, true),
        ] {
            let axes = MirrorAxes { x, y, z };
            let seen = positions_evaluated(position, axes);
            let expected = [
                if x { -0.3 } else { 0.3 },
                if y { -0.2 } else { 0.2 },
                if z { -0.1 } else { 0.1 },
            ];
            assert_eq!(seen.len(), 2, "axes {axes} should evaluate direct + mirror");
            assert_eq!(seen[0], position, "axes {axes} must keep the direct source");
            assert_eq!(seen[1], expected, "axes {axes} picked the wrong mirror");
        }
    }

    #[test]
    fn the_default_axes_reproduce_the_historical_horizontal_antipode() {
        // Guards the promise that existing renders are bit-identical: the
        // default must stay the half-turn about the vertical axis.
        let seen = positions_evaluated([0.3, 0.2, 0.1], MirrorAxes::default());
        assert_eq!(seen[1], [-0.3, -0.2, 0.1]);
    }

    #[test]
    fn no_flipped_axis_skips_the_mirror_evaluation_entirely() {
        // The mirror would coincide with the source and renormalize straight
        // back, so the stage must short-circuit rather than pay for it.
        let seen = positions_evaluated([0.3, 0.2, 0.1], MirrorAxes::NONE);
        assert_eq!(seen, vec![[0.3, 0.2, 0.1]]);
    }

    #[test]
    fn an_identity_axis_set_leaves_the_gains_untouched() {
        let position = [0.4, 0.2, 0.1];
        let decorated = wrapped()
            .compute_gains(&request_with_axes(position, true, MirrorAxes::NONE))
            .gains;
        let bare = BarycenterBackend::new(speakers(), 0.0)
            .compute_gains(&request(position, false))
            .gains;
        for (a, b) in decorated.iter().zip(bare.iter()) {
            assert!((a - b).abs() < 1e-6, "{a} vs {b}");
        }
    }
}
