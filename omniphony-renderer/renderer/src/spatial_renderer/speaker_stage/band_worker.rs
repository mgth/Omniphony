//! Builds the speaker stage's band engines off the render thread.
//!
//! A band set — one engine per crossover band, each sampling its own gain
//! table, plus the crossover bank and the unified table — takes from
//! milliseconds to seconds to build. The render thread asks the worker for
//! the set a topology needs and keeps rendering the one it has until the new
//! one lands, the way the binaural stages hand over their HRIR and BRIR
//! builds. The worker also frees the sets the render thread retires.

use std::sync::Arc;
use std::sync::mpsc;

use parking_lot::Mutex;

use super::{BandSet, BandSetKey, SpeakerRenderStage};
use crate::live_params::{RenderTopology, RendererControl};
use crate::spatial_renderer::components::BandRenderer;

enum Request {
    /// Build the set for `topology` and `key`. Only the latest one queued is
    /// built.
    Build {
        topology: Arc<RenderTopology>,
        key: BandSetKey,
    },
    /// The bands the render thread built itself, for the next build to reuse
    /// their gain models.
    Seed(Vec<BandRenderer>),
    /// Something the render thread is done with, to free here.
    Retire(Box<dyn std::any::Any + Send>),
}

pub(super) struct BandWorker {
    requests: mpsc::Sender<Request>,
    /// The finished set, until the render thread takes it. The render thread
    /// only `try_lock`s it; the worker holds it for the store alone.
    finished: Arc<Mutex<Option<BandSet>>>,
}

impl BandWorker {
    pub(super) fn spawn(
        control: Arc<RendererControl>,
        num_speakers: usize,
        sample_rate: u32,
    ) -> Self {
        let (requests, rx) = mpsc::channel();
        let finished = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&finished);
        std::thread::Builder::new()
            .name("speaker-band-worker".into())
            .spawn(move || Self::run(rx, &control, num_speakers, sample_rate, &slot))
            .expect("spawn speaker band worker");
        Self { requests, finished }
    }

    /// Ask for the set `key` needs. A newer request supersedes this one if
    /// the worker has not started on it yet.
    pub(super) fn request(&self, topology: Arc<RenderTopology>, key: BandSetKey) {
        let _ = self.requests.send(Request::Build { topology, key });
    }

    /// Hand the worker the bands just built on the render thread, so its
    /// next build can reuse their gain models.
    pub(super) fn seed(&self, bands: Vec<BandRenderer>) {
        let _ = self.requests.send(Request::Seed(bands));
    }

    /// Free `retired` on the worker rather than the render thread.
    pub(super) fn retire(&self, retired: Box<dyn std::any::Any + Send>) {
        let _ = self.requests.send(Request::Retire(retired));
    }

    /// The finished set, if one is waiting and the slot is free right now.
    /// Never blocks.
    pub(super) fn take_finished(&self) -> Option<BandSet> {
        self.finished.try_lock().and_then(|mut slot| slot.take())
    }

    /// Whether a finished set is waiting to be taken.
    #[cfg(test)]
    pub(super) fn has_finished(&self) -> bool {
        self.finished.lock().is_some()
    }

    fn run(
        rx: mpsc::Receiver<Request>,
        control: &Arc<RendererControl>,
        num_speakers: usize,
        sample_rate: u32,
        finished: &Mutex<Option<BandSet>>,
    ) {
        // The bands of the last set built here or seeded, whose gain models
        // the next build reuses when the geometry did not change.
        let mut last: Vec<BandRenderer> = Vec::new();
        while let Ok(first) = rx.recv() {
            let mut build = None;
            let mut handle = |request: Request| match request {
                Request::Build { topology, key } => build = Some((topology, key)),
                Request::Seed(bands) => last = bands,
                Request::Retire(retired) => drop(retired),
            };
            handle(first);
            while let Ok(next) = rx.try_recv() {
                handle(next);
            }
            let Some((topology, key)) = build else {
                continue;
            };
            match SpeakerRenderStage::build_band_set(
                control,
                topology,
                key,
                num_speakers,
                sample_rate,
                &last,
            ) {
                Ok(set) => {
                    last = set.render_bands.clone();
                    // An older set still waiting is superseded: it drops here.
                    let superseded = finished.lock().replace(set);
                    drop(superseded);
                }
                Err(e) => log::error!(
                    "speaker stage: band engines not rebuilt ({e:#}); the previous ones keep rendering"
                ),
            }
        }
    }
}
