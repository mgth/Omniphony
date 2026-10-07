use super::state::SpatialState;
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

    /// A new segment starts (or the bridge reset): the shared segment start
    /// ([`StreamState::begin_segment`], as the embedded engine does it —
    /// including the OSC purge of the previous layout's objects and the
    /// release of the dialogue-normalisation latch), then this host's export
    /// bed ids and the level it holds for the bridge while the sink plays PCM.
    ///
    /// The bridge's declaration is kept: the decoder thread re-sends it on a
    /// label change, not per segment.
    ///
    /// [`StreamState::begin_segment`]: orender_engine::stream_state::StreamState::begin_segment
    pub fn reset_for_segment(&mut self) {
        self.spatial.bed_indices = None;
        self.spatial.drop_dialnorm_aside();
        match self.spatial_renderer {
            Some(renderer) => self
                .spatial
                .pipeline
                .stream
                .begin_segment(renderer, self.osc_sender.as_deref_mut()),
            None => self.spatial.pipeline.stream.reset_segment(None),
        }
    }
}
