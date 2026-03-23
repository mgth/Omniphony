use super::state::SpatialState;
use crate::events::{Configuration, Event};
use crate::runtime_osc::{ObjectMeta, OscSender};
use anyhow::Result;
use bridge_api::{RCoordinateFormat, RMetadataFrame};

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

            if !meta.bed_indices.is_empty() {
                let new_bed_indices: Vec<usize> = meta.bed_indices.iter().copied().collect();
                let changed = self.spatial.bed_indices.as_ref() != Some(&new_bed_indices);
                if changed {
                    self.spatial.bed_indices = Some(new_bed_indices);
                    log::debug!(
                        "Extracted bed indices from bridge metadata: {:?}",
                        self.spatial.bed_indices
                    );

                    if let (Some(renderer), Some(bed_indices)) =
                        (self.spatial_renderer, &self.spatial.bed_indices)
                    {
                        renderer.configure_beds(bed_indices);
                    }
                }
            }

            self.handle_metadata_writing(meta, conf, sample_rate)?;
        }
        Ok(())
    }

    pub fn reset_for_segment(&mut self) {
        self.spatial.has_objects = false;
        self.spatial.bed_indices = None;
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
            let bed_to_speaker = active_layout
                .as_ref()
                .map(|layout| layout.bed_to_speaker_mapping())
                .unwrap_or_default();
            let objects: Vec<ObjectMeta> = conf
                .events
                .iter()
                .enumerate()
                .map(|(idx, event)| {
                    let logical_id = event.id().unwrap_or(idx as u32);
                    let direct_speaker_index = if logical_id < 10 {
                        bed_to_speaker
                            .get(&(logical_id as usize))
                            .copied()
                            .map(|idx| idx as u32)
                    } else {
                        None
                    };
                    let (ox, oy, oz, coord_mode) = direct_speaker_index
                        .and_then(|speaker_idx| {
                            active_layout.as_ref().and_then(|layout| {
                                layout.speakers.get(speaker_idx as usize).map(|speaker| {
                                    if speaker.coord_mode.eq_ignore_ascii_case("cartesian") {
                                        (
                                            speaker.x as f64,
                                            speaker.y as f64,
                                            speaker.z as f64,
                                            "cartesian".to_string(),
                                        )
                                    } else {
                                        (
                                            speaker.azimuth as f64,
                                            speaker.elevation as f64,
                                            speaker.distance as f64,
                                            "polar".to_string(),
                                        )
                                    }
                                })
                            })
                        })
                        .unwrap_or_else(|| {
                            let [x, y, z] =
                                Self::event_pos_raw(coordinate_format, event).unwrap_or([0.0; 3]);
                            (
                                x,
                                y,
                                z,
                                match coordinate_format {
                                    RCoordinateFormat::Cartesian => "cartesian".to_string(),
                                    RCoordinateFormat::Polar => "polar".to_string(),
                                },
                            )
                        });
                    ObjectMeta {
                        name: self
                            .spatial
                            .object_names
                            .get(&logical_id)
                            .cloned()
                            .unwrap_or_else(|| format!("Obj_{logical_id}")),
                        x: ox as f32,
                        y: oy as f32,
                        z: oz as f32,
                        coord_mode,
                        direct_speaker_index,
                        gain: event.gain_db().map_or(-128, |g| g as i32),
                        priority: 0.0,
                        divergence: 0.0,
                    }
                })
                .collect();
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
            let bed_indices = self.spatial.bed_indices.as_deref().unwrap_or(&[]);
            let bed_id_to_channel: std::collections::HashMap<usize, usize> = bed_indices
                .iter()
                .enumerate()
                .map(|(idx, &bid)| (bid, idx))
                .collect();
            let num_beds = bed_indices.len();

            for event in &conf.events {
                let object_id = match event.id() {
                    Some(id) => id as usize,
                    None => continue,
                };
                let (channel_idx, is_bed) = if object_id < 10 {
                    match bed_id_to_channel.get(&object_id) {
                        Some(&ch) => (ch, true),
                        None => continue,
                    }
                } else {
                    (num_beds + (object_id - 10), false)
                };
                self.spatial
                    .frame_events
                    .push(renderer::spatial_renderer::SpatialChannelEvent {
                        channel_idx,
                        is_bed,
                        gain_db: event.gain_db(),
                        ramp_length: event.ramp_length(),
                        spread: None,
                        position: Self::event_pos_as_adm_cartesian(coordinate_format, event),
                        sample_pos: event.sample_pos,
                    });
            }
        }

        Ok(())
    }

    fn normalize_azimuth_deg(mut azimuth_deg: f32) -> f32 {
        while azimuth_deg < -180.0 {
            azimuth_deg += 360.0;
        }
        while azimuth_deg > 180.0 {
            azimuth_deg -= 360.0;
        }
        azimuth_deg
    }

    fn event_pos_raw(_coordinate_format: RCoordinateFormat, event: &Event) -> Option<[f64; 3]> {
        let p = event.pos()?;
        if p.len() < 3 {
            return None;
        }
        Some([p[0], p[1], p[2]])
    }

    fn event_pos_as_adm_cartesian(
        coordinate_format: RCoordinateFormat,
        event: &Event,
    ) -> Option<[f64; 3]> {
        let p = event.pos()?;
        if p.len() < 3 {
            return None;
        }

        match coordinate_format {
            RCoordinateFormat::Cartesian => Some([p[0], p[1], p[2]]),
            RCoordinateFormat::Polar => {
                let az = Self::normalize_azimuth_deg(p[0] as f32);
                let el = (p[1] as f32).clamp(-90.0, 90.0);
                let dist = (p[2] as f32).max(0.0);
                let (x, y, z) = renderer::spatial_vbap::spherical_to_adm(az, el, dist);
                Some([x as f64, y as f64, z as f64])
            }
        }
    }
}
