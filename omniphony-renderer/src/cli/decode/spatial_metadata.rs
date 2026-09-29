use super::state::SpatialState;
use anyhow::Result;
use bridge_api::{RCoordinateFormat, RMetadataFrame};
use orender_engine::events::Configuration;
use orender_engine::osc::{ObjectMeta, OscSender};
use orender_engine::virtual_bed::build_fixed_channel_objects;

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

            // Legacy bed ids of the fixed-channel labels: EXPORT-order only
            // (file-output bed conformance). Rendering goes through the
            // shared planner below.
            let new_bed_indices =
                orender_engine::spatial::derive_bed_indices(&frame.channel_labels);
            if self.spatial.bed_indices.as_ref() != Some(&new_bed_indices) {
                log::debug!("Derived export bed ids from channel labels: {new_bed_indices:?}");
                self.spatial.bed_indices = Some(new_bed_indices);
            }

            // Plan the fixed prefix (virtualized by default, per-entry direct
            // opt-in), identically to the embedded engine.
            if let Some(renderer) = self.spatial_renderer {
                self.spatial.fixed_planner.plan_object_stream_fixed(
                    &frame.channel_labels,
                    self.spatial.source_family,
                    &self.spatial.declared_poses,
                    renderer,
                    &mut self.spatial.frame_events,
                );
            }

            self.handle_metadata_writing(meta, conf, sample_rate)?;
        }
        Ok(())
    }

    /// A new segment starts (or the bridge reset): the shared segment start
    /// ([`orender_engine::spatial::begin_segment`], as the embedded engine
    /// does it — including the OSC purge of the previous layout's objects),
    /// then this host's per-stream state. The dialogue-normalisation latch is
    /// released so the new segment's level is applied, not the previous one's.
    ///
    /// The bridge's declaration (`source_family`, `declared_poses`) is kept:
    /// the decoder thread re-sends it on a label change, not per segment.
    pub fn reset_for_segment(&mut self) {
        self.spatial.has_objects = false;
        self.spatial.bed_indices = None;
        self.spatial.fixed_planner.reset();
        self.spatial.bed_planner.reset();
        self.spatial.object_channels.clear();
        self.spatial.object_names.clear();
        self.spatial.frame_events.clear();
        self.spatial.bed_events.clear();
        self.spatial.loudness_applied = false;
        if let Some(renderer) = self.spatial_renderer {
            orender_engine::spatial::begin_segment(renderer, self.osc_sender.as_deref_mut());
        }
    }

    fn handle_metadata_writing(
        &mut self,
        meta: &RMetadataFrame,
        conf: Configuration,
        sample_rate: u32,
    ) -> Result<()> {
        // Object frames and timestamps carry the bridge's own sample position,
        // unchanged — the same clock the embedded engine sends, so a client
        // reads one timeline from either host.
        let sample_pos = meta.sample_pos;
        let coordinate_format = self.spatial.coordinate_format;

        // Cached whether or not anyone is listening, mirroring the embedded
        // host: names are declared sparsely, typically once at the start of a
        // stream, so a Studio attaching mid-playback would otherwise show an
        // object list that stays unnamed until the next declaration — which
        // for most content never comes.
        for upd in meta.name_updates.iter() {
            if self.spatial.object_names.get(&upd.id).map(String::as_str) != Some(upd.name.as_str())
            {
                self.spatial
                    .object_names
                    .insert(upd.id, upd.name.to_string());
            }
        }

        if self
            .osc_sender
            .as_ref()
            .is_some_and(|sender| sender.has_osc_clients())
        {
            let osc_sender = self.osc_sender.as_mut().expect("osc_sender present");
            let mut objects: Vec<ObjectMeta> = self
                .spatial_renderer
                .and_then(|renderer| {
                    build_fixed_channel_objects(
                        renderer,
                        self.spatial.fixed_planner.fixed_labels(),
                        self.spatial.source_family,
                        &self.spatial.declared_poses,
                    )
                })
                .unwrap_or_default();
            objects.extend(orender_engine::spatial::build_object_metas(
                &conf,
                coordinate_format,
                &self.spatial.object_names,
            ));
            let ramp_duration = meta.ramp_duration;
            let osc_coord_format = match coordinate_format {
                RCoordinateFormat::Cartesian => 0,
                RCoordinateFormat::Polar => 1,
            };
            if let Err(e) =
                osc_sender.send_object_frame(sample_pos, ramp_duration, osc_coord_format, &objects)
            {
                log::warn!("Failed to send OSC metadata: {}", e);
            }
            let seconds = sample_pos as f64 / sample_rate as f64;
            if let Err(e) = osc_sender.send_timestamp(sample_pos, seconds) {
                log::warn!("Failed to send OSC timestamp: {}", e);
            }
        }

        if self.spatial_renderer.is_some() {
            orender_engine::spatial::build_spatial_channel_events(
                &conf,
                coordinate_format,
                &self.spatial.object_channels,
                &meta.channel_gains,
                self.spatial.fixed_planner.fixed_trims(),
                meta.sample_pos,
                meta.ramp_duration,
                &mut self.spatial.frame_events,
            );
        }

        Ok(())
    }
}
