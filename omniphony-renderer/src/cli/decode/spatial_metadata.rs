use super::state::SpatialState;
use anyhow::Result;
use bridge_api::RMetadataFrame;
use orender_engine::events::Configuration;
use orender_engine::osc::OscSender;

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
            // The object↔channel declaration and the names are cached whether
            // or not anyone is listening, as in the embedded host: names are
            // declared sparsely, typically once at the start of a stream, so a
            // Studio attaching mid-playback would otherwise show an object
            // list that stays unnamed until the next declaration — which for
            // most content never comes.
            self.spatial.stream.note_object_metadata(meta);

            // Legacy bed ids of the fixed-channel labels: EXPORT-order only
            // (file-output bed conformance). Rendering goes through the
            // shared planner below.
            let new_bed_indices =
                orender_engine::spatial::derive_bed_indices(&frame.channel_labels);
            if self.spatial.bed_indices.as_ref() != Some(&new_bed_indices) {
                log::debug!("Derived export bed ids from channel labels: {new_bed_indices:?}");
                self.spatial.bed_indices = Some(new_bed_indices);
            }

            // Plan the fixed prefix and the objects, identically to the
            // embedded engine.
            if let Some(renderer) = self.spatial_renderer {
                self.spatial
                    .stream
                    .plan_object_frame(&frame.channel_labels, meta, &conf, renderer);
            }

            self.send_object_frame(meta, &conf, sample_rate);
        }
        Ok(())
    }

    /// A new segment starts (or the bridge reset): the shared segment start
    /// ([`StreamState::begin_segment`], as the embedded engine does it —
    /// including the OSC purge of the previous layout's objects and the
    /// release of the dialogue-normalisation latch), then this host's export
    /// bed ids.
    ///
    /// The bridge's declaration is kept: the decoder thread re-sends it on a
    /// label change, not per segment.
    ///
    /// [`StreamState::begin_segment`]: orender_engine::stream_state::StreamState::begin_segment
    pub fn reset_for_segment(&mut self) {
        self.spatial.bed_indices = None;
        match self.spatial_renderer {
            Some(renderer) => self
                .spatial
                .stream
                .begin_segment(renderer, self.osc_sender.as_deref_mut()),
            None => self.spatial.stream.reset_segment(None),
        }
    }

    /// Broadcast one metadata frame's objects to the OSC clients, if any. Object
    /// frames and timestamps carry the bridge's own sample position, unchanged
    /// — the same clock the embedded engine sends, so a client reads one
    /// timeline from either host.
    fn send_object_frame(&mut self, meta: &RMetadataFrame, conf: &Configuration, sample_rate: u32) {
        let Some(osc_sender) = self
            .osc_sender
            .as_deref_mut()
            .filter(|sender| sender.has_osc_clients())
        else {
            return;
        };
        let sample_pos = meta.sample_pos;
        let objects = self
            .spatial
            .stream
            .object_frame_metas(self.spatial_renderer, conf);
        if let Err(e) = osc_sender.send_object_frame(
            sample_pos,
            meta.ramp_duration,
            self.spatial.stream.osc_coordinate_format(),
            &objects,
        ) {
            log::warn!("Failed to send OSC metadata: {}", e);
        }
        let seconds = sample_pos as f64 / sample_rate as f64;
        if let Err(e) = osc_sender.send_timestamp(sample_pos, seconds) {
            log::warn!("Failed to send OSC timestamp: {}", e);
        }
    }
}
