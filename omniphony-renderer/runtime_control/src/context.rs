use std::sync::Arc;

use audio_output::AudioControl;
use renderer::live_params::RendererControl;

#[derive(Clone)]
pub struct RuntimeControlContext {
    pub renderer: Arc<RendererControl>,
    pub audio: Option<Arc<AudioControl>>,
}

impl RuntimeControlContext {
    pub fn new(renderer: Arc<RendererControl>, audio: Option<Arc<AudioControl>>) -> Self {
        Self { renderer, audio }
    }
}
