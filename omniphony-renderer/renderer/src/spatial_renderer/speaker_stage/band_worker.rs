//! Builds the speaker stage's band engines off the render thread.
//!
//! A band set — one engine per crossover band, each sampling its own gain
//! table, plus the crossover bank and the unified table — takes from
//! milliseconds to seconds to build. The render thread asks the worker for
//! the set a topology needs and keeps rendering the one it has until the new
//! one lands, the way the binaural stages hand over their HRIR and BRIR
//! builds. The worker also frees the sets the render thread retires.
//!
//! A build that fails, or panics in a backend, is handed back as a
//! [`Finished::Failed`]: the render thread stops waiting for that set, the
//! reason goes to the log and to the control (for the clients), and the
//! worker carries on with the next request.

use std::ops::Range;
use std::sync::Arc;
use std::sync::mpsc;

use parking_lot::Mutex;

use super::{BandSet, BandSetKey, PreviousBands, SpeakerRenderStage};
use crate::live_params::{RenderTopology, RendererControl};
use crate::spatial_renderer::components::BandRenderer;

enum Request {
    /// Build the set for `topology` and `key`. Only the latest one queued is
    /// built.
    Build {
        topology: Arc<RenderTopology>,
        key: BandSetKey,
        /// The channels to allocate crossover filter memory for.
        filtered_channels: Range<usize>,
    },
    /// The bands the render thread built itself for `topology`, for the next
    /// build to start from.
    Seed {
        topology: Arc<RenderTopology>,
        bands: Vec<BandRenderer>,
    },
    /// Something the render thread is done with, to free here.
    Retire(Box<dyn std::any::Any + Send>),
}

/// What the worker hands back for the request it built.
pub(super) enum Finished {
    /// The set, to install if it is still the one wanted.
    Set(BandSet),
    /// No set: the build failed.
    Failed(FailedBuild),
}

/// A key the worker could not build a set for. Carries the topology, so the
/// address in the key cannot be reused while the stage remembers the failure.
pub(super) struct FailedBuild {
    pub(super) key: BandSetKey,
    /// Held, never read: what keeps the address `key` names from being reused.
    #[allow(dead_code)]
    pub(super) topology: Arc<RenderTopology>,
}

pub(super) struct BandWorker {
    requests: mpsc::Sender<Request>,
    /// The outcome of the last build, until the render thread takes it. The
    /// render thread only `try_lock`s it; the worker holds it for the store
    /// alone.
    finished: Arc<Mutex<Option<Finished>>>,
    /// How many builds the worker has answered, counted once the outcome is
    /// in the slot: tests wait on it, whoever takes the outcome.
    #[cfg(test)]
    answered: Arc<std::sync::atomic::AtomicUsize>,
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
        #[cfg(test)]
        let answered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        #[cfg(test)]
        let count = Arc::clone(&answered);
        std::thread::Builder::new()
            .name("speaker-band-worker".into())
            .spawn(move || {
                crate::background_pool::enter_background();
                Self::run(rx, &control, num_speakers, sample_rate, |outcome| {
                    // An older outcome still waiting is superseded: it drops
                    // here.
                    let superseded = slot.lock().replace(outcome);
                    drop(superseded);
                    #[cfg(test)]
                    count.fetch_add(1, std::sync::atomic::Ordering::Release);
                })
            })
            .expect("spawn speaker band worker");
        Self {
            requests,
            finished,
            #[cfg(test)]
            answered,
        }
    }

    /// Ask for the set `key` needs, with filter memory for
    /// `filtered_channels`. A newer request supersedes this one if the worker
    /// has not started on it yet. Returns `false` when the worker is gone and
    /// nothing will come back.
    #[must_use]
    pub(super) fn request(
        &self,
        topology: Arc<RenderTopology>,
        key: BandSetKey,
        filtered_channels: Range<usize>,
    ) -> bool {
        self.requests
            .send(Request::Build {
                topology,
                key,
                filtered_channels,
            })
            .is_ok()
    }

    /// Hand the worker the bands just built on the render thread for
    /// `topology`, for its next build to start from.
    pub(super) fn seed(&self, topology: Arc<RenderTopology>, bands: Vec<BandRenderer>) {
        let _ = self.requests.send(Request::Seed { topology, bands });
    }

    /// Free `retired` on the worker rather than the render thread (on the
    /// caller's only if the worker is gone).
    pub(super) fn retire(&self, retired: Box<dyn std::any::Any + Send>) {
        let _ = self.requests.send(Request::Retire(retired));
    }

    /// The outcome of the last build, if one is waiting and the slot is free
    /// right now. Never blocks.
    pub(super) fn take_finished(&self) -> Option<Finished> {
        self.finished.try_lock().and_then(|mut slot| slot.take())
    }

    /// Whether a build outcome is waiting to be taken.
    #[cfg(test)]
    pub(super) fn has_finished(&self) -> bool {
        self.finished.lock().is_some()
    }

    /// How many builds the worker has answered so far, taken or not.
    #[cfg(test)]
    pub(super) fn answered(&self) -> usize {
        self.answered.load(std::sync::atomic::Ordering::Acquire)
    }

    fn run(
        rx: mpsc::Receiver<Request>,
        control: &Arc<RendererControl>,
        num_speakers: usize,
        sample_rate: u32,
        deliver: impl Fn(Finished),
    ) {
        // The bands of the last set built here or seeded, and the topology
        // they are for: what the next build starts from. A build for that
        // same topology shares them with the set it came from, so a set the
        // render thread drops keeps no table of its own alive here.
        let mut last: Option<(Arc<RenderTopology>, Vec<BandRenderer>)> = None;
        // Whether the clients were last told of a failed build: the next
        // build that goes through takes the error back.
        let mut error_reported = false;
        while let Ok(first) = rx.recv() {
            let mut build = None;
            let mut handle = |request: Request| match request {
                Request::Build {
                    topology,
                    key,
                    filtered_channels,
                } => build = Some((topology, key, filtered_channels)),
                Request::Seed { topology, bands } => last = Some((topology, bands)),
                Request::Retire(retired) => drop(retired),
            };
            handle(first);
            while let Ok(next) = rx.try_recv() {
                handle(next);
            }
            let Some((topology, key, filtered_channels)) = build else {
                continue;
            };
            let previous = match &last {
                Some((topology, bands)) => PreviousBands {
                    topology: Some(topology),
                    bands,
                },
                None => PreviousBands {
                    topology: None,
                    bands: &[],
                },
            };
            // A backend that panics while its table is sampled must not take
            // the worker with it: every later change would go unanswered.
            let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                SpeakerRenderStage::build_band_set(
                    control,
                    Arc::clone(&topology),
                    key,
                    num_speakers,
                    sample_rate,
                    previous,
                    filtered_channels,
                )
            }))
            .unwrap_or_else(|payload| {
                let detail = if let Some(msg) = payload.downcast_ref::<&'static str>() {
                    (*msg).to_string()
                } else if let Some(msg) = payload.downcast_ref::<String>() {
                    msg.clone()
                } else {
                    "panic with non-string payload".to_string()
                };
                Err(anyhow::anyhow!(
                    "render backend panicked during the band build: {detail}"
                ))
            });
            let outcome = match built {
                Ok(set) => {
                    last = Some((Arc::clone(&topology), set.render_bands.clone()));
                    if error_reported {
                        control.report_band_build_error(String::new());
                        error_reported = false;
                    }
                    Finished::Set(set)
                }
                Err(e) => {
                    let message = format!(
                        "Speaker stage: band engines not rebuilt ({e:#}); the previous ones keep rendering"
                    );
                    log::error!("{message}");
                    control.report_band_build_error(message);
                    error_reported = true;
                    Finished::Failed(FailedBuild { key, topology })
                }
            };
            deliver(outcome);
        }
    }
}
