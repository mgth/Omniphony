use super::state::SpatialState;
use anyhow::Result;
use bridge_api::{RChannelLabel, RCoordinateFormat, RMetadataFrame};
use orender_engine::events::{Configuration, Event};
use orender_engine::osc::{ObjectMeta, OscSender};

pub struct SpatialMetadataCoordinator<'a> {
    spatial: &'a mut SpatialState,
    spatial_renderer: Option<&'a renderer::spatial_renderer::SpatialRenderer>,
    osc_sender: Option<&'a mut OscSender>,
}

impl<'a> SpatialMetadataCoordinator<'a> {
    pub fn new(
        spatial: &'a mut SpatialState,
        spatial_renderer: Option<&'a renderer::spatial_renderer::SpatialRenderer>,
        osc_sender: Option<&'a mut OscSender>,
    ) -> Self {
        Self {
            spatial,
            spatial_renderer,
            osc_sender,
        }
    }

    pub fn handle_spatial_metadata(
        &mut self,
        frame: &bridge_api::RDecodedFrame,
        sample_rate: u32,
    ) -> Result<()> {
        if frame.metadata.is_empty() {
            return Ok(());
        }

        for meta in frame.metadata.iter() {
            let conf = Configuration::from(meta);
            self.spatial.has_objects = true;

            // Cache the sparse object↔channel declaration.
            if !meta.object_channels.is_empty() {
                let mut decl: Vec<(u32, usize)> = meta
                    .object_channels
                    .iter()
                    .map(|oc| (oc.id, oc.channel as usize))
                    .collect();
                decl.sort_unstable_by_key(|&(_, channel)| channel);
                if self.spatial.object_channels != decl {
                    self.spatial.object_channels = decl;
                }
            }

            // Renderer bed set = legacy bed ids of the fixed-channel labels
            // (the 0-9 scheme leaves with the unified channel plan, phase 2b).
            let new_bed_indices =
                orender_engine::spatial::derive_bed_indices(&frame.channel_labels);
            if self.spatial.bed_indices.as_ref() != Some(&new_bed_indices) {
                self.spatial.bed_indices = Some(new_bed_indices);
                log::debug!(
                    "Derived bed indices from channel labels: {:?}",
                    self.spatial.bed_indices
                );
                if let (Some(renderer), Some(bed_indices)) =
                    (self.spatial_renderer, &self.spatial.bed_indices)
                {
                    renderer.configure_beds(bed_indices);
                }
            }

            self.handle_metadata_writing(meta, conf, &frame.channel_labels, sample_rate)?;
        }
        Ok(())
    }

    pub fn reset_for_segment(&mut self) {
        self.spatial.has_objects = false;
        self.spatial.bed_indices = None;
        self.spatial.object_channels.clear();
        self.spatial.object_names.clear();
        self.spatial.frame_events.clear();
        if let Some(renderer) = self.spatial_renderer {
            renderer.reset_runtime_state();
        }
    }

    fn handle_metadata_writing(
        &mut self,
        meta: &RMetadataFrame,
        conf: Configuration,
        channel_labels: &[RChannelLabel],
        sample_rate: u32,
    ) -> Result<()> {
        let sample_pos = meta.sample_pos;
        let segment_relative_sample_pos = if self.spatial.is_segmented {
            let relative_pos = sample_pos.saturating_sub(self.spatial.segment_start_samples);
            log::trace!(
                "Adjusting metadata sample position: absolute={}, segment_start={}, relative={}",
                sample_pos,
                self.spatial.segment_start_samples,
                relative_pos
            );
            relative_pos
        } else {
            sample_pos
        };
        let coordinate_format = self.spatial.coordinate_format;

        if self
            .osc_sender
            .as_ref()
            .is_some_and(|sender| sender.has_osc_clients())
        {
            let osc_sender = self.osc_sender.as_mut().expect("osc_sender present");
            for upd in meta.name_updates.iter() {
                self.spatial
                    .object_names
                    .insert(upd.id, upd.name.to_string());
            }
            let active_layout = self
                .spatial_renderer
                .map(|renderer| renderer.speaker_layout());
            let objects: Vec<ObjectMeta> = orender_engine::spatial::build_object_metas(
                &conf,
                coordinate_format,
                active_layout.as_ref(),
                &self.spatial.object_names,
                channel_labels,
                &meta.channel_gains,
            );
            let ramp_duration = meta.ramp_duration;
            let osc_coord_format = match coordinate_format {
                RCoordinateFormat::Cartesian => 0,
                RCoordinateFormat::Polar => 1,
            };
            if let Err(e) = osc_sender.send_object_frame(
                segment_relative_sample_pos,
                ramp_duration,
                osc_coord_format,
                &objects,
            ) {
                log::warn!("Failed to send OSC metadata: {}", e);
            }
            let seconds = segment_relative_sample_pos as f64 / sample_rate as f64;
            if let Err(e) = osc_sender.send_timestamp(segment_relative_sample_pos, seconds) {
                log::warn!("Failed to send OSC timestamp: {}", e);
            }
        }

        if self.spatial_renderer.is_some() {
            orender_engine::spatial::build_spatial_channel_events(
                &conf,
                coordinate_format,
                &self.spatial.object_channels,
                &meta.channel_gains,
                meta.sample_pos,
                meta.ramp_duration,
                &mut self.spatial.frame_events,
            );
        }

        Ok(())
    }

    fn event_pos_raw(_coordinate_format: RCoordinateFormat, event: &Event) -> Option<[f64; 3]> {
        let p = event.pos()?;
        if p.len() < 3 {
            return None;
        }
        Some([p[0], p[1], p[2]])
    }
}
