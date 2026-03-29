use std::sync::Arc;

use audio_output::{AudioControl, InputControl};
use renderer::live_params::RendererControl;

#[derive(Clone)]
pub struct RuntimeControlContext {
    pub renderer: Arc<RendererControl>,
    pub audio: Option<Arc<AudioControl>>,
    pub input: Option<Arc<InputControl>>,
}

impl RuntimeControlContext {
    pub fn new(
        renderer: Arc<RendererControl>,
        audio: Option<Arc<AudioControl>>,
        input: Option<Arc<InputControl>>,
    ) -> Self {
        Self {
            renderer,
            audio,
            input,
        }
    }
}
