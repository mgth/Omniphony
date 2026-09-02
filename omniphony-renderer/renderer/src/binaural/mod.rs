//! Independent binaural (headphone) output stage.
//!
//! This is a **parallel render path**, not a [`GainModel`] backend: a backend
//! only emits per-speaker gains and cannot carry the per-ear delay (ITD) or the
//! stateful HRTF convolution a binaural renderer needs. When
//! [`OutputMode::Binaural`] is selected, `SpatialRenderer::render_frame` skips
//! the whole VBAP / crossover / speaker chain and calls [`BinauralRenderer`]
//! instead, producing a 2-channel (L/R) interleaved frame.
//!
//! Pipeline per input channel, per frame:
//! `pos_adm → rotate(head_pose) → (az, el, dist)`
//!   → per-ear ITD delay → per-ear HRIR convolution
//!   → (+ shoebox early reflections, see [`reflections`])
//!   → sum into `[L, R]`.
//!
//! Space scaling is a single **isotropic** `unit_scale_m` (metres per ADM unit);
//! the anisotropic `room_ratio` is deliberately *not* reused here because it
//! would distort directions and corrupt HRTF localisation.
//!
//! [`GainModel`]: crate::render_backend::GainModel
//! [`OutputMode`]: crate::live_params::OutputMode

pub mod convolver;
pub mod head_pose;
pub mod hrir;
pub mod itd;
pub mod measured;
pub mod prtf;
pub mod reflections;
pub mod reverb;
pub mod tracking;

#[cfg(test)]
mod validation;

pub use head_pose::HeadPose;
pub use tracking::{HeadTracking, HeadTrackingFormat};

use crate::delay_line::DelayLine;
use crate::live_params::{BinauralReflections, BinauralReverb};
use convolver::EarConvolver;
use hrir::{DirectionKey, HRIR_LEN, HrirPair, HrirSet, ParametricPinnaHrir};
use measured::MeasuredHrirData;
use prtf::SpagnolPrtfHrir;
use reflections::ReflectionBank;
use reverb::Fdn;

/// Per-listener `D_n` preset for the parametric pinna model (Brown & Duda 1998,
/// Table I). `D_n` is the only parameter the paper individualizes per subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PinnaPreset {
    /// Subjects PB & NH: D = [1, 0.5, 0.5, 0.5, 0.5].
    #[default]
    PbNh,
    /// Subject RD: D = [0.85, 0.35, 0.35, 0.35, 0.35].
    Rd,
}

impl PinnaPreset {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::PbNh => "pbnh",
            Self::Rd => "rd",
        }
    }
    pub fn from_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "rd" => Self::Rd,
            _ => Self::PbNh,
        }
    }
    /// The published `D_n` column for this preset.
    fn d_base(&self) -> [f32; 5] {
        match self {
            Self::PbNh => ParametricPinnaHrir::D_PB_NH,
            Self::Rd => ParametricPinnaHrir::D_RD,
        }
    }
}

/// Which HRIR data set the binaural stage convolves with.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HrirSource {
    /// Built-in analytic head-shadow model (no measured data, lightest).
    Synthetic,
    /// Embedded SAF KEMAR measured set (ISC). Default — real measured HRTF.
    #[default]
    SafKemar,
    /// A SOFA file loaded from disk (requires the `sofa` build feature).
    Sofa(String),
    /// Parametric structural model: analytic head shadow + the Brown-Duda pinna
    /// echo train (exact Table I coefficients). `preset` picks a published `D_n`
    /// column (the only per-listener parameter), `d_scale_pct` fine-tunes it,
    /// `depth_pct` is the echo strength (0 ≈ synthetic, 100 = full). No measured
    /// data — the "tune a few knobs" alternative to `saf`/`sofa`.
    Pinna {
        preset: PinnaPreset,
        d_scale_pct: u16,
        depth_pct: u16,
    },
    /// Structural PRTF model (Spagnol/Geronazzo/Avanzini): head shadow + two
    /// concha resonances + three elevation-dependent notches, population-average
    /// preset. `depth_pct` is the pinna-coloration amount (0 ≈ synthetic), and
    /// `freq_scale_pct` shifts all notch/resonance frequencies (individualization).
    Prtf { freq_scale_pct: u16, depth_pct: u16 },
}

impl HrirSource {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Synthetic => "synthetic",
            Self::SafKemar => "saf",
            Self::Sofa(_) => "sofa",
            Self::Pinna { .. } => "pinna",
            Self::Prtf { .. } => "prtf",
        }
    }

    /// Parse a source selector. `"sofa:<path>"` carries the file path; a bare
    /// `"sofa"` yields `Sofa("")` (path to be set separately).
    pub fn from_str(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some(path) = s.strip_prefix("sofa:") {
            return Some(Self::Sofa(path.to_string()));
        }
        let lower = s.to_ascii_lowercase();
        // "pinna" | "pinna:<preset>:<dscale>:<depth>" (preset = pbnh|rd,
        // dscale/depth percent integers).
        if let Some(rest) = lower.strip_prefix("pinna:") {
            let mut it = rest.split(':');
            let preset = PinnaPreset::from_str(it.next().unwrap_or("pbnh"));
            let d_scale = it
                .next()
                .and_then(|v| v.trim().parse::<u16>().ok())
                .unwrap_or(100);
            let depth = it
                .next()
                .and_then(|v| v.trim().parse::<u16>().ok())
                .unwrap_or(100);
            return Some(Self::Pinna {
                preset,
                d_scale_pct: d_scale.clamp(50, 150),
                depth_pct: depth.clamp(0, 100),
            });
        }
        // "prtf" | "prtf:<freq_scale>:<depth>" (percent integers).
        if let Some(rest) = lower.strip_prefix("prtf:") {
            let mut it = rest.split(':');
            let freq = it
                .next()
                .and_then(|v| v.trim().parse::<u16>().ok())
                .unwrap_or(100);
            let depth = it
                .next()
                .and_then(|v| v.trim().parse::<u16>().ok())
                .unwrap_or(100);
            return Some(Self::Prtf {
                freq_scale_pct: freq.clamp(50, 150),
                depth_pct: depth.clamp(0, 100),
            });
        }
        match lower.as_str() {
            "synthetic" | "synth" => Some(Self::Synthetic),
            "saf" | "kemar" | "saf_kemar" => Some(Self::SafKemar),
            "sofa" => Some(Self::Sofa(String::new())),
            "pinna" | "parametric" => Some(Self::Pinna {
                preset: PinnaPreset::PbNh,
                d_scale_pct: 100,
                depth_pct: 100,
            }),
            "prtf" | "spagnol" => Some(Self::Prtf {
                freq_scale_pct: 100,
                depth_pct: 100,
            }),
            _ => None,
        }
    }
}

/// Closest distance (m) the reflection and reverb laws see, for the source
/// and for its images alike: bounds the near-field ratios below.
const MIN_DISTANCE_M: f32 = 0.25;
/// Maximum reflection gain relative to the direct sound (`d_src / d_img`,
/// which nears 1 for a source against a wall), so an image can't blow up.
const MAX_DISTANCE_GAIN: f32 = 4.0;
/// Source distance (m) at which the reverb send is unity. The direct sound
/// keeps its authored level whatever the distance, so the send has to grow
/// with it to move the direct/reverberant ratio the way a room does: send
/// ∝ distance, unity at this reference.
const REVERB_REF_DISTANCE_M: f32 = 1.5;
/// Cap on the distance-driven reverb send (reached at 6 m), so a far object
/// cannot flood the tail.
const MAX_REVERB_SEND: f32 = 4.0;
/// Delay-line capacity for the ITD (s) — comfortably above the ~0.7 ms max.
const ITD_MAX_S: f32 = 0.003;
/// Input channels whose DSP state is built at construction, so that the
/// first block of any stream up to this width — and every enable of the
/// reflections or the reverb — allocates nothing on the audio thread. Wider
/// streams fall back to allocating the extra slots on first use.
const PREALLOC_CHANNELS: usize = 64;
/// Reverb send bus capacity reserved at construction (samples per block).
const REVERB_BUS_CAPACITY: usize = 8192;

/// Per-input-channel binaural DSP state, lazily created on first use.
struct ChannelDsp {
    delay_l: DelayLine,
    delay_r: DelayLine,
    conv_l: EarConvolver,
    conv_r: EarConvolver,
    /// Early-reflection bank. Allocated with the state (the ring is the big
    /// allocation here, and it used to come and go with the reflections
    /// toggle — on the audio thread). While reflections are off the ring is
    /// still written, so enabling them reads real recent audio.
    refl: ReflectionBank,
    /// Whether the bank has been read from since the state last drained:
    /// decides how much silence a drain has to run (its ring, or just the
    /// ITD line).
    refl_live: bool,
    /// Air-absorption one-pole low-pass state (direct path).
    air_state: f32,
    /// Air-absorption smoothing coefficient for the current block
    /// (0 = bypass, →1 = heavy low-pass). Updated per block from distance.
    air_coeff: f32,
    /// Lattice direction the loaded kernels were built from, tagged with the
    /// HRIR grid generation they came out of — a live source switch replaces
    /// the grid under us, and a key alone would then match across two
    /// different datasets and freeze the stale kernels in place.
    last_dir: Option<(u32, DirectionKey)>,
    /// Samples of silence still to run through this state once the channel
    /// has gone silent. Every block that carries signal rearms it; a silent
    /// block runs the DSP on zeros and counts it down; at zero the channel
    /// is skipped outright. Without it a muted channel froze its histories
    /// — the ITD lines, the convolver window, and the reflection ring with
    /// up to a quarter second of audio in it — and replayed them the moment
    /// its gain came back.
    flush: u32,
}

impl ChannelDsp {
    fn new(sample_rate: u32) -> Self {
        let max_delay = (ITD_MAX_S * sample_rate as f32).ceil() as usize;
        Self {
            delay_l: DelayLine::new(max_delay),
            delay_r: DelayLine::new(max_delay),
            conv_l: EarConvolver::new(),
            conv_r: EarConvolver::new(),
            refl: ReflectionBank::new(sample_rate),
            refl_live: false,
            air_state: 0.0,
            air_coeff: 0.0,
            last_dir: None,
            flush: 0,
        }
    }

    /// Silence needed to drain every history this state holds: the longest
    /// ring in it (the reflection bank's, when present, otherwise the ITD
    /// line's) plus the convolver window.
    fn flush_len(&self, sample_rate: u32) -> u32 {
        let ring_s = if self.refl_live {
            reflections::RING_CAPACITY_S
        } else {
            ITD_MAX_S
        };
        (ring_s * sample_rate as f32).ceil() as u32 + HRIR_LEN as u32 + 2
    }
}

/// One input channel's gain over a block: `start + step · sample_index`.
///
/// The gain is slewed upstream (20 ms toward the authored value); handing
/// the ramp down as a start and a per-sample step lets the block apply it
/// per sample, as the speaker path does. Applying the block-end value as a
/// constant instead stepped a 40 dB change in 24 stairs of 1.7 dB on
/// 40-sample blocks — zipper noise on every fast object fade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChannelGain {
    /// Linear gain at the first sample of the block.
    pub start: f32,
    /// Change per sample across the block.
    pub step: f32,
}

impl ChannelGain {
    /// A gain that holds `gain` for the whole block.
    pub const fn flat(gain: f32) -> Self {
        Self {
            start: gain,
            step: 0.0,
        }
    }

    /// Whether the block is silent throughout: nothing in, and no ramp
    /// bringing anything in.
    #[inline]
    fn is_silent(&self) -> bool {
        self.start == 0.0 && self.step == 0.0
    }
}

/// A source rendered alongside the input channels, from its own mono block.
///
/// Exists for the object test, which has no input channel to ride on: it is a
/// source the renderer invents, so it needs a way in that does not disturb the
/// channel numbering everything else depends on. Giving it a slot past the end
/// of the channels means it picks up the full spatialization path — HRIR, ITD,
/// air absorption, early reflections, reverb send — rather than a simplified
/// stand-in, which is the whole reason a *binaural* object test is worth having
/// where a binaural speaker test is not.
pub struct ExtraSource<'a> {
    /// Mono samples. Normally `sample_length` of them, but **may be shorter**:
    /// the object test's safety cap ends a run mid-block, and the block it
    /// returns then stops where the cap did. Only the samples present are
    /// rendered; the rest of the frame gets nothing from this source.
    pub pcm: &'a [f32],
    /// World (ADM) position, same convention as `chan_pos`.
    pub position: [f64; 3],
    /// Linear gain applied on top of the block.
    pub gain: f32,
}

/// Per-frame live parameters for [`BinauralRenderer::render_frame`], grouped
/// so the call site stays readable as the stage grows.
pub struct BinauralFrameParams {
    pub head_pose: HeadPose,
    pub unit_scale_m: f32,
    pub head_radius_m: f32,
    pub reflections: BinauralReflections,
    pub reverb: BinauralReverb,
    pub air_absorption: bool,
    /// How finely a direction must change before its HRIR is rebuilt.
    pub hrir_update_lattice: crate::live_params::HrirUpdateLattice,
}

/// What the last HRIR grid build produced, for the control surface.
///
/// `requested` is the source that was asked for, `effective` the one the
/// grid actually holds — they differ when a SOFA file could not be loaded
/// and the build fell back to the embedded KEMAR set, in which case `error`
/// says why. Published by the rebuild worker as each build completes (a few
/// milliseconds before the audio thread swaps the grid in), so a client
/// comparing two HRTFs can see that it is not listening to the one it
/// selected, instead of a log line nobody reads.
#[derive(Debug, Clone, PartialEq)]
pub struct HrirStatus {
    pub requested: HrirSource,
    pub effective: HrirSource,
    pub error: Option<String>,
}

impl Default for HrirStatus {
    fn default() -> Self {
        Self {
            requested: HrirSource::default(),
            effective: HrirSource::default(),
            error: None,
        }
    }
}

/// Receives each build's [`HrirStatus`]; the renderer's owner wires it to the
/// state broadcast. Called on the rebuild worker (or, for the initial grid,
/// on the constructing thread), never on the audio thread.
pub type HrirStatusSink = std::sync::Arc<dyn Fn(HrirStatus) + Send + Sync>;

/// An HRIR grid together with the source it was built from.
///
/// The two travel as one allocation so that swapping a grid in also swaps the
/// answer to "which source is actually live" — see
/// [`BinauralRenderer::rebuild_pending`]. Keeping them in separate fields would
/// let the pair disagree, which is exactly what this type exists to prevent.
struct Grid {
    /// The source that was asked for — what `rebuild_pending` compares.
    requested: HrirSource,
    /// The source the set really came from (KEMAR after a failed SOFA load).
    effective: HrirSource,
    /// Why `effective` differs from `requested`, when it does.
    error: Option<String>,
    set: HrirSet,
}

impl Grid {
    fn status(&self) -> HrirStatus {
        HrirStatus {
            requested: self.requested.clone(),
            effective: self.effective.clone(),
            error: self.error.clone(),
        }
    }
}

/// Owns the per-channel binaural DSP state and the HRIR set; renders all input
/// channels of a frame to interleaved stereo.
pub struct BinauralRenderer {
    sample_rate: u32,
    hrir: std::sync::Arc<Grid>,
    /// HRIR source last *requested* (the active grid may briefly lag it while
    /// the worker builds — see [`Self::ensure_source`]).
    source: HrirSource,
    /// Finished grids from the rebuild worker, awaiting the audio-thread swap.
    incoming: std::sync::Arc<arc_swap::ArcSwapOption<Grid>>,
    /// Requests to the long-lived rebuild worker. Dropping the renderer drops
    /// the sender, which terminates the worker.
    rebuild_tx: std::sync::mpsc::Sender<HrirSource>,
    /// Grids the audio thread has swapped out, handed to the worker to be
    /// freed there: the last reference to a grid must not drop on the audio
    /// thread (half a megabyte and up).
    retire_tx: std::sync::mpsc::Sender<std::sync::Arc<Grid>>,
    /// Per-input-channel DSP state, indexed directly by channel. The first
    /// [`PREALLOC_CHANNELS`] slots are built at construction; a wider stream
    /// grows the vector and fills the extra slots on first use.
    channels: Vec<Option<ChannelDsp>>,
    /// DSP state for the extra source (the object test), kept apart from
    /// `channels` on purpose: it is not a channel and the input width it would
    /// otherwise be indexed past changes whenever playback starts or stops.
    /// Kept across tests and drained like a channel that went silent, so a
    /// test never starts on a frozen tail — nor on a fresh allocation.
    extra_dsp: ChannelDsp,
    /// Reusable HRIR scratch so `at()` writes in place (no per-channel alloc).
    hrir_scratch: HrirPair,
    /// Bumped every time a rebuilt grid is swapped in, so the per-channel
    /// direction cache cannot match across two different datasets.
    hrir_generation: u32,
    /// Shared late-reverb tail. Allocated with the renderer; cleared in
    /// place when the reverb is switched off.
    fdn: Fdn,
    /// Whether the tail holds anything since it was last cleared.
    fdn_live: bool,
    /// Mono reverb send bus, one sample per frame (reused).
    reverb_bus: Vec<f32>,
}

impl BinauralRenderer {
    /// A renderer whose build status goes nowhere (tests, hosts without a
    /// control surface). See [`Self::with_status_sink`].
    pub fn new(sample_rate: u32) -> Self {
        Self::with_status_sink(sample_rate, std::sync::Arc::new(|_| {}))
    }

    /// A renderer that reports every HRIR build's [`HrirStatus`] to `sink`.
    pub fn with_status_sink(sample_rate: u32, sink: HrirStatusSink) -> Self {
        let source = HrirSource::default();
        let incoming: std::sync::Arc<arc_swap::ArcSwapOption<Grid>> =
            std::sync::Arc::new(arc_swap::ArcSwapOption::empty());
        // Long-lived rebuild worker: grid builds (allocations, provider
        // renders, SOFA file I/O) must never run on the audio thread. The
        // worker drains request bursts to the latest one, builds, and
        // publishes into `incoming` for the audio thread to swap in.
        let (rebuild_tx, rebuild_rx) = std::sync::mpsc::channel::<HrirSource>();
        let (retire_tx, retire_rx) = std::sync::mpsc::channel::<std::sync::Arc<Grid>>();
        {
            let slot = std::sync::Arc::clone(&incoming);
            let sink = std::sync::Arc::clone(&sink);
            std::thread::Builder::new()
                .name("binaural-hrir-rebuild".into())
                .spawn(move || {
                    while let Ok(mut req) = rebuild_rx.recv() {
                        while let Ok(newer) = rebuild_rx.try_recv() {
                            req = newer;
                        }
                        // Free whatever the audio thread has retired, here
                        // rather than there.
                        while retire_rx.try_recv().is_ok() {}
                        let grid = Self::build_grid(req, sample_rate);
                        // Reported before the grid is handed over, so the
                        // status is never behind what is being convolved.
                        sink(grid.status());
                        slot.store(Some(std::sync::Arc::new(grid)));
                    }
                })
                .expect("spawn binaural HRIR rebuild worker");
        }
        // The initial (default) grid is built synchronously: `new` runs on
        // a control thread, and the renderer must be usable immediately.
        let initial = Self::build_grid(source.clone(), sample_rate);
        sink(initial.status());
        Self {
            sample_rate,
            hrir: std::sync::Arc::new(initial),
            source,
            incoming,
            rebuild_tx,
            retire_tx,
            // Every state a stream up to PREALLOC_CHANNELS wide can need,
            // built here on the control thread.
            channels: (0..PREALLOC_CHANNELS)
                .map(|_| Some(ChannelDsp::new(sample_rate)))
                .collect(),
            extra_dsp: ChannelDsp::new(sample_rate),
            hrir_scratch: HrirPair {
                left: [0.0; HRIR_LEN],
                right: [0.0; HRIR_LEN],
            },
            hrir_generation: 0,
            fdn: Fdn::new(sample_rate),
            fdn_live: false,
            reverb_bus: Vec::with_capacity(REVERB_BUS_CAPACITY),
        }
    }

    /// Identity of the active HRIR grid (tests observe the async swap with it).
    #[cfg(test)]
    fn hrir_grid_id(&self) -> usize {
        std::sync::Arc::as_ptr(&self.hrir) as usize
    }

    /// Whether a requested source change has not been swapped in yet, i.e. the
    /// grid being convolved is not the one last requested.
    ///
    /// [`Self::ensure_source`] hands a request to the rebuild worker and keeps
    /// rendering with the previous grid until the result lands, so "requested"
    /// and "live" are genuinely different questions. Anything that must
    /// attribute its output to a specific HRIR set has to wait on this.
    pub fn rebuild_pending(&self) -> bool {
        self.source != self.hrir.requested
    }

    /// Build the grid for `requested`. A SOFA source that cannot be loaded
    /// falls back to the embedded KEMAR set; the grid then records both
    /// sources and the reason, for [`HrirStatus`].
    fn build_grid(requested: HrirSource, sample_rate: u32) -> Grid {
        let (set, effective, error) = match &requested {
            HrirSource::Sofa(path) => match Self::load_sofa(path, sample_rate) {
                Ok(set) => (set, requested.clone(), None),
                Err(reason) => {
                    log::warn!(
                        "binaural: SOFA source '{path}' unavailable ({reason}); falling back to SAF KEMAR"
                    );
                    (
                        Self::build_hrir(&HrirSource::SafKemar, sample_rate),
                        HrirSource::SafKemar,
                        Some(reason),
                    )
                }
            },
            other => (Self::build_hrir(other, sample_rate), other.clone(), None),
        };
        Grid {
            requested,
            effective,
            error,
            set,
        }
    }

    fn build_hrir(source: &HrirSource, sample_rate: u32) -> HrirSet {
        match source {
            HrirSource::Synthetic => HrirSet::synthetic(sample_rate),
            HrirSource::Pinna {
                preset,
                d_scale_pct,
                depth_pct,
            } => {
                let scale = *d_scale_pct as f32 / 100.0;
                let d = preset.d_base().map(|x| x * scale);
                HrirSet::new(
                    &ParametricPinnaHrir {
                        d,
                        depth: *depth_pct as f32 / 100.0,
                    },
                    sample_rate,
                )
            }
            HrirSource::Prtf {
                freq_scale_pct,
                depth_pct,
            } => HrirSet::new(
                &SpagnolPrtfHrir {
                    depth: *depth_pct as f32 / 100.0,
                    freq_scale: *freq_scale_pct as f32 / 100.0,
                },
                sample_rate,
            ),
            HrirSource::SafKemar => HrirSet::new(
                &MeasuredHrirData::saf_kemar().resampled_to(sample_rate),
                sample_rate,
            ),
            // Handled by `build_grid`, which owns the fallback; reaching
            // here means a caller asked for the raw build, so no fallback.
            HrirSource::Sofa(path) => Self::load_sofa(path, sample_rate).unwrap_or_else(|_| {
                HrirSet::new(
                    &MeasuredHrirData::saf_kemar().resampled_to(sample_rate),
                    sample_rate,
                )
            }),
        }
    }

    /// The set from a SOFA file, or the reason it could not be loaded — the
    /// text that reaches the control surface.
    fn load_sofa(path: &str, sample_rate: u32) -> Result<HrirSet, String> {
        if path.trim().is_empty() {
            return Err("no SOFA file selected".to_string());
        }
        Self::load_sofa_file(path, sample_rate)
    }

    #[cfg(feature = "sofa")]
    fn load_sofa_file(path: &str, sample_rate: u32) -> Result<HrirSet, String> {
        measured::hrir_set_from_sofa(path, sample_rate).map_err(|e| e.to_string())
    }

    #[cfg(not(feature = "sofa"))]
    fn load_sofa_file(_path: &str, _sample_rate: u32) -> Result<HrirSet, String> {
        Err("SOFA support not built into this renderer (enable the 'sofa' feature)".to_string())
    }

    /// Track the requested HRIR source. Called once per frame from the audio
    /// thread; the steady-state cost is one compare plus one atomic swap. On
    /// an actual change it only pushes a request to the rebuild worker — the
    /// grid build (allocations, provider renders, SOFA file I/O) never runs
    /// here (issue #153). Frames keep rendering with the previous grid until
    /// the worker's result lands.
    pub fn ensure_source(&mut self, source: &HrirSource) {
        if let Some(grid) = self.incoming.swap(None) {
            let retired = std::mem::replace(&mut self.hrir, grid);
            // The old grid goes back to the worker to be freed; dropping it
            // here would free its megabytes on the audio thread. (`send`
            // allocates one queue node, as the request below does — rare.)
            let _ = self.retire_tx.send(retired);
            // Invalidates every channel's cached lattice direction at once —
            // the new grid answers differently for the same key.
            self.hrir_generation = self.hrir_generation.wrapping_add(1);
        }
        if &self.source != source {
            self.source = source.clone();
            // `send` allocates one queue node — rare (a user-initiated source
            // change), unlike the megabytes+I/O of the build it replaces.
            let _ = self.rebuild_tx.send(source.clone());
        }
    }

    /// Render one frame to interleaved stereo.
    ///
    /// - `chan_pos[c]`: world (ADM) position of input channel `c`.
    /// - `chan_gain[c]`: linear gain ramp for channel `c` over the block
    ///   (object mute/gain folded in), applied per sample.
    /// - `chan_direct[c]`: `true` for channels that keep their direct-routing
    ///   intent (beds mapped to a `spatialize: false` speaker — the LFE): fed
    ///   to both ears equally, bypassing HRIR/ITD/air/reflections/reverb.
    ///   Missing entries read as `false`.
    /// - `out`: must be `sample_length * 2`, pre-zeroed.
    /// - `extra`: an optional source that is not an input channel — the object
    ///   test. Rendered exactly like a spatialized channel, from its own DSP
    ///   slot past the end of the input channels.
    #[allow(clippy::too_many_arguments)]
    pub fn render_frame(
        &mut self,
        input_pcm: &[f32],
        input_channel_count: usize,
        sample_length: usize,
        params: &BinauralFrameParams,
        chan_pos: &[[f64; 3]],
        chan_gain: &[ChannelGain],
        chan_direct: &[bool],
        extra: Option<ExtraSource<'_>>,
        out: &mut [f32],
    ) {
        let BinauralFrameParams {
            head_pose,
            unit_scale_m,
            head_radius_m,
            ref reflections,
            ref reverb,
            air_absorption,
            hrir_update_lattice,
        } = *params;
        debug_assert_eq!(out.len(), sample_length * 2);
        if sample_length == 0 || (input_channel_count == 0 && extra.is_none()) {
            return;
        }
        // The extra source gets a DSP slot of its own — its convolvers, delay
        // lines and reflection bank persist across blocks like any channel's,
        // and that continuity is what lets it move without clicking.
        //
        // A *dedicated* slot, not one indexed past the input channels: the
        // input width is not a constant. It is 2 while the idle feed fabricates
        // silence and whatever the programme carries once one starts, so a slot
        // at `input_channel_count` moves the moment playback begins or ends —
        // landing on a fresh (silent, warming up) DSP, or on the state of a
        // channel that used to live there.
        // The extra slot also runs with no extra source while its state is
        // still draining (see `ChannelDsp::flush`), on silence: that is how
        // the end of one object test leaves nothing behind for the next.
        let extra_slot = extra.is_some() || self.extra_dsp.flush > 0;
        let source_count = input_channel_count + usize::from(extra_slot);
        if self.channels.len() < input_channel_count {
            // Wider than the preallocation: grow, and fill on first use.
            self.channels.resize_with(input_channel_count, || None);
        }

        // Late-reverb bus: per-channel sends accumulate here; the shared FDN
        // turns the mono sum into a decorrelated stereo tail after the loop.
        let reverb_active = reverb.enabled;
        if reverb_active {
            self.fdn.set_params(reverb.rt60_s, reverb.predelay_ms);
            self.fdn_live = true;
            self.reverb_bus.clear();
            self.reverb_bus.resize(sample_length, 0.0);
        } else if self.fdn_live {
            // Switched off: silence the tail in place (what dropping and
            // rebuilding the network used to do, minus the allocation).
            self.fdn.clear();
            self.fdn_live = false;
        }

        for c in 0..source_count {
            // Past the input channels sits the extra source (the object test).
            // Its PCM is mono, hence the stride of 1 — that triple is the only
            // thing that differs from a channel all the way down.
            let in_extra_slot = c >= input_channel_count;
            let extra_here = extra.as_ref().filter(|_| in_extra_slot);
            let (src_pcm, src_stride, src_offset) = match extra_here {
                Some(e) => (e.pcm, 1usize, 0usize),
                // The extra slot draining with no source: no PCM to read,
                // which the silent path below never does.
                None if in_extra_slot => (&[][..], 1usize, 0usize),
                None => (input_pcm, input_channel_count, c),
            };
            // How much of the frame this source actually has. Full length for a
            // channel; for the extra source, however far its block got before
            // the cap ended it (see `ExtraSource::pcm`). Reading `sample_length`
            // regardless is an out-of-bounds panic on the render thread — which
            // is how a binaural object test died at the two-minute mark.
            let span = match extra_here {
                Some(e) => sample_length.min(e.pcm.len()),
                None => sample_length,
            };
            if span == 0 {
                continue;
            }
            let gain = match extra_here {
                Some(e) => ChannelGain::flat(e.gain),
                None if in_extra_slot => ChannelGain::flat(0.0),
                None => chan_gain.get(c).copied().unwrap_or(ChannelGain::flat(0.0)),
            };
            let silent = gain.is_silent();
            if silent {
                // A silent channel is skipped only once its state has
                // nothing left to say. While it has, the block runs through
                // the full path below on zeros: the reflections of what was
                // playing keep arriving at their delays, the convolver and
                // ITD windows drain, and time keeps moving for this channel
                // — so nothing is frozen to replay when the gain returns.
                let draining = if in_extra_slot {
                    self.extra_dsp.flush > 0
                } else {
                    self.channels
                        .get(c)
                        .and_then(|s| s.as_ref())
                        .is_some_and(|dsp| dsp.flush > 0)
                };
                if !draining {
                    continue;
                }
            }
            // Direct (non-spatialized) feed — the LFE policy (issue #156):
            // sub-bass carries no usable direction, so the channel goes to
            // both ears at constant power (−3 dB each), dry and full-range —
            // no HRIR, no ITD, no air, no reflections, no reverb send. Unity
            // overall (no +10 dB LFE convention), matching the speaker path's
            // untouched one-hot routing. Head rotation deliberately has no
            // effect, like real sub-bass.
            // The extra source is never "direct": it is an object by definition,
            // and placing it is the entire point.
            if !in_extra_slot && chan_direct.get(c).copied().unwrap_or(false) {
                if let Some(Some(dsp)) = self.channels.get_mut(c) {
                    // Nothing of the spatialized state is drained on this
                    // path; a channel switching back to it starts its
                    // histories over.
                    dsp.flush = 0;
                    dsp.refl_live = false;
                }
                for s in 0..span {
                    let g = gain.start + gain.step * s as f32;
                    let v =
                        src_pcm[s * src_stride + src_offset] * g * std::f32::consts::FRAC_1_SQRT_2;
                    let o = s * 2;
                    out[o] += v;
                    out[o + 1] += v;
                }
                continue;
            }
            let pos = match extra_here {
                Some(e) => e.position,
                None => chan_pos.get(c).copied().unwrap_or([0.0, 1.0, 0.0]),
            };

            // World → head-relative direction, then spherical angles.
            let hp = head_pose.rotate(pos);
            let (hx, hy, hz) = (hp[0] as f32, hp[1] as f32, hp[2] as f32);
            let az_rad = hx.atan2(hy); // 0 = front, + = right
            let horiz = (hx * hx + hy * hy).sqrt();
            let el_rad = hz.atan2(horiz);

            // Isotropic distance scale → metric distance (m). Direction is
            // scale-invariant. Object/bed levels are authored upstream (Atmos
            // object gain), so the direct path applies NO inverse-distance gain;
            // dist_m only drives the distance *cues* (air absorption, reverb
            // send, early reflections), never the direct object level. Those
            // cues are therefore expressed relative to the direct sound: the
            // 1/d the direct path does not apply is folded into them.
            let dist_norm = ((pos[0] * pos[0] + pos[1] * pos[1] + pos[2] * pos[2]).sqrt()) as f32;
            let dist_m = (dist_norm * unit_scale_m).max(0.0);

            // ITD stays continuous: it is the dominant lateralisation cue, and
            // it is a closed-form formula feeding an interpolating delay line —
            // cheap enough that quantizing it would buy nothing.
            let (itd_l, itd_r) = itd::ear_delays_seconds(az_rad, el_rad, head_radius_m);

            // The HRIR is the expensive one, so it updates on a lattice
            // instead. The kernel is a pure function of this key, so an
            // unchanged key means `at()` would rebuild the very kernels the
            // convolvers already hold: skip the four-pair gather *and* the
            // kernel compare rather than paying both to learn nothing moved.
            // Skipping also leaves no crossfade armed, which halves the tap
            // loop for the block (one dot product instead of two).
            let dir = (
                self.hrir_generation,
                self.hrir.set.quantize_direction(
                    az_rad.to_degrees(),
                    el_rad.to_degrees(),
                    hrir_update_lattice.subdiv(),
                ),
            );
            let rate = self.sample_rate;
            let dsp = if in_extra_slot {
                &mut self.extra_dsp
            } else {
                // Preallocated up to PREALLOC_CHANNELS; past that, built on
                // first use (an allocation on this thread, once per slot).
                self.channels[c].get_or_insert_with(|| ChannelDsp::new(rate))
            };
            if dsp.last_dir != Some(dir) {
                dsp.last_dir = Some(dir);
                self.hrir.set.at_key(dir.1, &mut self.hrir_scratch);
                // Kernel changes (moving object / head) crossfade over the
                // block — capped at HRIR_LEN samples for large offline blocks
                // — so the transfer function never jumps at a block boundary
                // (issue #155).
                let fade = sample_length.min(HRIR_LEN);
                dsp.conv_l.set_coeffs_smooth(&self.hrir_scratch.left, fade);
                dsp.conv_r.set_coeffs_smooth(&self.hrir_scratch.right, fade);
            }
            dsp.delay_l.set_target_ms(itd_l * 1000.0, self.sample_rate);
            dsp.delay_r.set_target_ms(itd_r * 1000.0, self.sample_rate);

            // Air absorption: a one-pole low-pass whose cutoff falls with
            // distance (HF dies in air — true outdoors as much as indoors).
            // Bypass within 3 m; ~14 kHz at 10 m, ~5 kHz at 30 m, floor 2 kHz.
            dsp.air_coeff = if air_absorption && dist_m > 3.0 {
                let fc = (20_000.0 * (-0.05 * (dist_m - 3.0)).exp()).max(2_000.0);
                (-std::f32::consts::TAU * fc / self.sample_rate as f32).exp()
            } else {
                0.0
            };

            // Reverb send. In a room the reverberant field barely depends on
            // the source distance while the direct sound falls as 1/d, so
            // the direct/reverberant ratio — the dominant distance cue past
            // a metre — falls with distance. The direct path here keeps its
            // authored level instead of falling, so the send must rise in
            // its place: proportional to the distance, unity at the
            // reference, capped. (The previous `d / (d + 0.5)` saturated at
            // 1: +3 dB from 1 m to 10 m against a physical 20 dB, so the cue
            // stopped at arm's length.)
            let reverb_send = if reverb_active {
                // Multiplier on the (already gain-scaled) channel signal.
                (dist_m / REVERB_REF_DISTANCE_M).min(MAX_REVERB_SEND)
            } else {
                0.0
            };

            // ── Early reflections: per-block image-source update ─────────────
            if reflections.enabled {
                let bank = &mut dsp.refl;
                let phys = [
                    pos[0] as f32 * unit_scale_m,
                    pos[1] as f32 * unit_scale_m,
                    pos[2] as f32 * unit_scale_m,
                ];
                // The image sources are mirrors of the source *as pulled
                // inside the room*, so the direct-path reference for their
                // relative delays is that clamped source, not the raw one.
                // With the raw distance, a source outside the room (any
                // `unit_scale_m` ≥ 2 with the default room) had every
                // reflection arrive early by the excess, and the near wall's
                // at zero delay — a coincident copy of the direct sound.
                let src_m = reflections::clamp_into_room(phys, reflections.room_size_m);
                let d_src = (src_m[0] * src_m[0] + src_m[1] * src_m[1] + src_m[2] * src_m[2])
                    .sqrt()
                    .max(MIN_DISTANCE_M);
                let images = reflections::first_order_images(src_m, reflections.room_size_m);
                let c_sound = reflections::speed_of_sound();
                for (i, img) in images.iter().enumerate() {
                    let d_img = (img[0] * img[0] + img[1] * img[1] + img[2] * img[2])
                        .sqrt()
                        .max(MIN_DISTANCE_M);
                    // Relative to the direct path so the direct sound keeps
                    // zero added latency (A/V sync unchanged).
                    let rel_delay_s = (d_img - d_src).max(0.0) / c_sound;
                    // Head-relative direction → broadband ILD pan (no HRIR
                    // conv per reflection: one tap + one multiply per ear).
                    let ih = head_pose.rotate([img[0] as f64, img[1] as f64, img[2] as f64]);
                    let inorm = ((ih[0] * ih[0] + ih[1] * ih[1] + ih[2] * ih[2]) as f32)
                        .sqrt()
                        .max(1e-6);
                    let lat = (ih[0] as f32 / inorm).clamp(-1.0, 1.0);
                    const SHADOW: f32 = 0.5;
                    let g_r = ((1.0 + SHADOW * lat) / (1.0 + SHADOW)).sqrt();
                    let g_l = ((1.0 - SHADOW * lat) / (1.0 + SHADOW)).sqrt();
                    // Level relative to the direct sound: the 1/d law of the
                    // image over the 1/d the direct path would have had
                    // (`d_src / d_img`), because the direct sound keeps its
                    // authored level. An absolute `1 / d_img` was only right
                    // for a source at 1 m and moved the wrong way with
                    // distance — a receding source got *drier*.
                    let g_dist = (d_src / d_img).min(MAX_DISTANCE_GAIN);
                    let g = reflections.level.clamp(0.0, 1.0) * g_dist;
                    bank.set_targets(i, rel_delay_s, g * g_l, g * g_r);
                }
            }

            // Signal rearms the drain; silence spends it. What the drain has
            // to cover follows what is being read: the ring while the
            // reflections are on, the ITD line otherwise.
            if !silent {
                dsp.refl_live = reflections.enabled;
            }
            dsp.flush = if silent {
                dsp.flush.saturating_sub(span as u32)
            } else {
                dsp.flush_len(self.sample_rate)
            };
            let reflections_on = reflections.enabled;

            let air = dsp.air_coeff;
            for s in 0..span {
                // `raw` carries the object/metadata gain only; the direct path
                // adds its distance gain, the reflection taps theirs. The air
                // low-pass applies to the propagated wave, so it feeds the
                // direct, the reflections and the reverb send alike.
                // A silent block reads no input at all — the draining extra
                // slot has none to read.
                let mut raw = if silent {
                    0.0
                } else {
                    src_pcm[s * src_stride + src_offset] * (gain.start + gain.step * s as f32)
                };
                if air > 0.0 {
                    dsp.air_state += (raw - dsp.air_state) * (1.0 - air);
                    raw = dsp.air_state;
                }
                // Authored object/bed level is respected: no 1/d attenuation.
                let x = raw;
                let mut yl = dsp.conv_l.process(dsp.delay_l.process(x));
                let mut yr = dsp.conv_r.process(dsp.delay_r.process(x));
                if reflections_on {
                    let (rl, rr) = dsp.refl.process(raw);
                    yl += rl;
                    yr += rr;
                } else {
                    // Keep the ring current for the moment they come back.
                    dsp.refl.push(raw);
                }
                if reverb_active {
                    self.reverb_bus[s] += raw * reverb_send;
                }
                let o = s * 2;
                out[o] += yl;
                out[o + 1] += yr;
            }
        }

        // Shared tail: one FDN pass over the summed sends, added to the mix.
        if reverb_active {
            self.fdn.process_block(&self.reverb_bus, reverb.level, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Anechoic frame params: identity pose, unit scale, default head radius,
    /// reflections off — the legacy expectations below are HRIR/ITD-only.
    fn dry_params() -> BinauralFrameParams {
        BinauralFrameParams {
            head_pose: HeadPose::identity(),
            unit_scale_m: 1.0,
            head_radius_m: itd::DEFAULT_HEAD_RADIUS_M,
            reflections: BinauralReflections {
                enabled: false,
                ..Default::default()
            },
            reverb: BinauralReverb {
                enabled: false,
                ..Default::default()
            },
            air_absorption: false,
            hrir_update_lattice: crate::live_params::HrirUpdateLattice::default(),
        }
    }

    fn render_single(pos: [f64; 3]) -> (f32, f32) {
        let mut r = BinauralRenderer::new(48_000);
        let n = 512;
        // A silent first block lets the kernel crossfade (armed when a channel
        // first receives its HRIR) run to completion. Probing during the fade
        // would weight the kernel by the ramp and so measure its tail, not it.
        let silent = vec![0.0f32; n];
        let mut warm = vec![0.0f32; n * 2];
        r.render_frame(
            &silent,
            1,
            n,
            &dry_params(),
            &[pos],
            &[ChannelGain::flat(1.0)],
            &[],
            None,
            &mut warm,
        );
        // Single impulse: per-ear output energy then equals the (delay-preserved)
        // HRIR energy — a broadband probe that doesn't over-weight the Nyquist bin
        // the way an alternating ±1 input would on a measured HRIR.
        let mut input = vec![0.0f32; n];
        input[0] = 1.0;
        let mut out = vec![0.0f32; n * 2];
        r.render_frame(
            &input,
            1,
            n,
            &dry_params(),
            &[pos],
            &[ChannelGain::flat(1.0)],
            &[],
            None,
            &mut out,
        );
        let mut el = 0.0f32;
        let mut er = 0.0f32;
        for s in 0..n {
            el += out[s * 2] * out[s * 2];
            er += out[s * 2 + 1] * out[s * 2 + 1];
        }
        (el, er)
    }

    #[test]
    fn right_source_is_louder_in_right_channel() {
        let (el, er) = render_single([1.0, 0.0, 0.0]); // full right
        assert!(er > el, "L={el} R={er}");
    }

    /// The extra source is spatialized like a real channel, not summed flat.
    ///
    /// This is what makes a *binaural* object test worth having where a binaural
    /// speaker test is not: the object test has a direction, so it must get the
    /// HRIR path. Asserted by placing it hard right with silent input channels —
    /// if it were mixed centre (or bypassed), the ears would match.
    #[test]
    fn the_extra_source_is_spatialized() {
        let mut r = BinauralRenderer::new(48_000);
        let n = 512;
        let input = vec![0.0f32; n]; // one silent input channel
        let mut extra_pcm = vec![0.0f32; n];
        extra_pcm[0] = 1.0;
        let mut out = vec![0.0f32; n * 2];
        r.render_frame(
            &input,
            1,
            n,
            &dry_params(),
            &[[0.0, 1.0, 0.0]],
            &[ChannelGain::flat(0.0)],
            &[],
            Some(ExtraSource {
                pcm: &extra_pcm,
                position: [1.0, 0.0, 0.0], // hard right
                gain: 1.0,
            }),
            &mut out,
        );
        let (mut el, mut er) = (0.0f32, 0.0f32);
        for s in 0..n {
            el += out[s * 2] * out[s * 2];
            er += out[s * 2 + 1] * out[s * 2 + 1];
        }
        assert!(el + er > 0.0, "the extra source produced no output at all");
        assert!(
            er > el,
            "a hard-right extra source must favour the right ear (L={el} R={er}) \
             — equal ears mean it bypassed the HRIR path"
        );
    }

    /// The cap can end a test mid-block, and then the extra source's PCM is
    /// shorter than the frame. Reported from use: binaural object injection
    /// died after a couple of minutes with something in the log.
    #[test]
    fn a_short_extra_block_does_not_run_off_the_end() {
        let mut r = BinauralRenderer::new(48_000);
        let n = 512;
        let input = vec![0.0f32; n];
        let extra_pcm = vec![0.1f32; 200]; // the cap ran out 200 samples in
        let mut out = vec![0.0f32; n * 2];
        r.render_frame(
            &input,
            1,
            n,
            &dry_params(),
            &[[0.0, 1.0, 0.0]],
            &[ChannelGain::flat(0.0)],
            &[],
            Some(ExtraSource {
                pcm: &extra_pcm,
                position: [1.0, 0.0, 0.0],
                gain: 1.0,
            }),
            &mut out,
        );
    }

    /// The extra source keeps its own DSP state when the input width changes.
    ///
    /// The input width is not a constant: it is 2 while the idle feed fabricates
    /// silence and whatever the programme carries once one starts. A DSP slot
    /// indexed past the input channels therefore moves the moment playback
    /// begins — onto a fresh convolver that has to warm up again, or onto the
    /// state of some channel that used to sit there. Either way the source
    /// stutters, in the middle of the one gesture the test exists to judge.
    ///
    /// Asserted on continuity: a steady input through a settled convolver gives
    /// a steady output, so a drop right after the width change is the warm-up of
    /// a slot that should not have been touched.
    #[test]
    fn the_extra_source_survives_a_change_of_input_width() {
        let mut r = BinauralRenderer::new(48_000);
        let n = 512;
        let extra_pcm = vec![0.5f32; n];
        let steady = |r: &mut BinauralRenderer, channels: usize| -> f32 {
            let input = vec![0.0f32; n * channels];
            let mut out = vec![0.0f32; n * 2];
            r.render_frame(
                &input,
                channels,
                n,
                &dry_params(),
                &vec![[0.0, 1.0, 0.0]; channels],
                &vec![ChannelGain::flat(0.0); channels],
                &vec![false; channels],
                Some(ExtraSource {
                    pcm: &extra_pcm,
                    position: [1.0, 0.0, 0.0],
                    gain: 1.0,
                }),
                &mut out,
            );
            // RMS of the first 32 samples: where a warm-up would show.
            let head: f32 = out[..64].iter().map(|v| v * v).sum::<f32>() / 32.0;
            head.sqrt()
        };

        // Settle at the idle-feed width.
        for _ in 0..4 {
            steady(&mut r, 2);
        }
        let before = steady(&mut r, 2);
        // Playback starts: the programme is wider than the silence was.
        let after = steady(&mut r, 12);
        assert!(before > 0.0, "the extra source produced nothing to compare");
        assert!(
            (after - before).abs() < before * 0.05,
            "the extra source dropped from {before} to {after} when the input \
             width changed — its DSP slot moved with the channel count"
        );
    }

    /// Switching the reflections on after a stretch with them off must read
    /// the audio that just played, not what was in the ring when they were
    /// switched off (the ring used to be dropped and rebuilt — which also
    /// meant an allocation on the audio thread — and now is kept current).
    #[test]
    fn reflections_switched_back_on_read_recent_audio() {
        let mut r = BinauralRenderer::new(48_000);
        let n = 480;
        let room = BinauralReflections {
            enabled: true,
            room_size_m: [4.0, 5.0, 2.7],
            level: 0.5,
        };
        let on = BinauralFrameParams {
            reflections: room.clone(),
            ..dry_params()
        };
        let off = BinauralFrameParams {
            reflections: BinauralReflections {
                enabled: false,
                ..room
            },
            ..dry_params()
        };
        let pos = [[0.0, 1.0, 0.0]];
        let mut out = vec![0.0f32; n * 2];
        let mut render = |r: &mut BinauralRenderer, p: &BinauralFrameParams, x: &[f32]| -> f32 {
            out.iter_mut().for_each(|v| *v = 0.0);
            r.render_frame(
                x,
                1,
                n,
                p,
                &pos,
                &[ChannelGain::flat(1.0)],
                &[],
                None,
                &mut out,
            );
            out[n..].iter().map(|v| v * v).sum::<f32>()
        };
        let noise: Vec<f32> = (0..n)
            .map(|i| ((i * 7919) % 1000) as f32 / 500.0 - 1.0)
            .collect();
        let silence = vec![0.0f32; n];
        // Reflections on with noise, then off with silence for 0.4 s: the
        // ring must now hold silence, not the noise.
        render(&mut r, &on, &noise);
        for _ in 0..40 {
            render(&mut r, &off, &silence);
        }
        // Back on, silent input: the direct path is silent and the ring is
        // silent, so the second half of the block must be (near) silent.
        let back = render(&mut r, &on, &silence);
        assert!(back < 1e-9, "stale ring content replayed: {back}");
    }

    /// The extra source must not disturb the input channels' own rendering: it
    /// takes a DSP slot past the end of them, so channel numbering is untouched.
    #[test]
    fn the_extra_source_leaves_the_channels_alone() {
        let n = 512;
        let mut input = vec![0.0f32; n];
        input[0] = 1.0;
        let pos = [[-1.0, 0.0, 0.0]];

        let mut without = vec![0.0f32; n * 2];
        BinauralRenderer::new(48_000).render_frame(
            &input,
            1,
            n,
            &dry_params(),
            &pos,
            &[ChannelGain::flat(1.0)],
            &[],
            None,
            &mut without,
        );

        let extra_pcm = vec![0.0f32; n]; // silent extra: must change nothing
        let mut with = vec![0.0f32; n * 2];
        BinauralRenderer::new(48_000).render_frame(
            &input,
            1,
            n,
            &dry_params(),
            &pos,
            &[ChannelGain::flat(1.0)],
            &[],
            Some(ExtraSource {
                pcm: &extra_pcm,
                position: [1.0, 0.0, 0.0],
                gain: 1.0,
            }),
            &mut with,
        );

        assert_eq!(
            without, with,
            "adding a silent extra source changed the channel rendering — the \
             slot is not as separate as it looks"
        );
    }

    /// A SOFA file that cannot be loaded must not pretend it was: the status
    /// names the set actually convolved (KEMAR) and says why, while
    /// `rebuild_pending` still clears — the request was served, by the
    /// fallback.
    #[test]
    fn a_failed_sofa_load_reports_the_fallback() {
        let seen: std::sync::Arc<std::sync::Mutex<Vec<HrirStatus>>> = Default::default();
        let sink: HrirStatusSink = {
            let seen = std::sync::Arc::clone(&seen);
            std::sync::Arc::new(move |st| seen.lock().unwrap().push(st))
        };
        let mut r = BinauralRenderer::with_status_sink(48_000, sink);
        assert_eq!(
            seen.lock().unwrap().last().cloned(),
            Some(HrirStatus::default()),
            "the initial build must report the default set"
        );
        let missing = HrirSource::Sofa("/nonexistent/listener.sofa".to_string());
        r.ensure_source(&missing);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while r.rebuild_pending() {
            assert!(std::time::Instant::now() < deadline, "rebuild never landed");
            std::thread::sleep(std::time::Duration::from_millis(5));
            r.ensure_source(&missing);
        }
        let last = seen.lock().unwrap().last().cloned().expect("a status");
        assert_eq!(last.requested, missing);
        assert_eq!(last.effective, HrirSource::SafKemar);
        let err = last.error.expect("the failure must carry a reason");
        assert!(!err.is_empty());
    }

    /// A source switch must not stall rendering: the frame right after the
    /// request still uses the old grid (and produces audio), and the new grid
    /// lands asynchronously within a bounded delay (issue #153).
    #[test]
    fn hrir_source_switch_lands_asynchronously_without_blocking_render() {
        let mut r = BinauralRenderer::new(48_000);
        let n = 128;
        let mut input = vec![0.0f32; n];
        input[0] = 1.0;
        let pos = [[0.5, 1.0, 0.0]];
        let render = |r: &mut BinauralRenderer| -> f32 {
            let mut out = vec![0.0f32; n * 2];
            r.render_frame(
                &input,
                1,
                n,
                &dry_params(),
                &pos,
                &[ChannelGain::flat(1.0)],
                &[],
                None,
                &mut out,
            );
            out.iter().map(|x| x * x).sum()
        };

        let initial_grid = r.hrir_grid_id();
        r.ensure_source(&HrirSource::Synthetic);
        // Immediately after the request the old grid must still be active and
        // rendering must work (the build happens on the worker).
        assert_eq!(r.hrir_grid_id(), initial_grid, "swap must be asynchronous");
        assert!(render(&mut r) > 1e-9, "render stalled during rebuild");

        // The new grid must land within a bounded delay.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while r.hrir_grid_id() == initial_grid {
            assert!(
                std::time::Instant::now() < deadline,
                "rebuilt grid never arrived"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
            r.ensure_source(&HrirSource::Synthetic);
        }
        assert!(render(&mut r) > 1e-9, "render broken after grid swap");
    }

    #[test]
    fn left_source_is_louder_in_left_channel() {
        let (el, er) = render_single([-1.0, 0.0, 0.0]); // full left
        assert!(el > er, "L={el} R={er}");
    }

    #[test]
    fn front_source_is_balanced() {
        let (el, er) = render_single([0.0, 1.0, 0.0]); // front
        let ratio = el / er;
        assert!((0.5..2.0).contains(&ratio), "L={el} R={er}");
    }

    #[test]
    fn reflections_add_delayed_energy() {
        // Impulse at 2 m in a 4 m room: the side/ceiling images detour ~2.5 m
        // (~343 samples at 48 kHz) and the rear wall ~4 m (~553 samples), all
        // far past the 128-tap HRIR tail (~160 samples incl. ITD). With
        // reflections ON there must be energy out there; with them OFF, silence.
        let n = 4_096;
        let mut input = vec![0.0f32; n];
        input[0] = 1.0;
        let pos = [[0.0, 2.0, 0.0]];
        let tail = |out: &[f32]| -> f32 { out[250 * 2..].iter().map(|x| x * x).sum::<f32>() };

        let mut dry = vec![0.0f32; n * 2];
        let mut r = BinauralRenderer::new(48_000);
        r.render_frame(
            &input,
            1,
            n,
            &dry_params(),
            &pos,
            &[ChannelGain::flat(1.0)],
            &[],
            None,
            &mut dry,
        );

        let wet_params = BinauralFrameParams {
            reflections: BinauralReflections {
                enabled: true,
                room_size_m: [4.0, 4.0, 4.0],
                level: 0.5,
            },
            ..dry_params()
        };
        let mut wet = vec![0.0f32; n * 2];
        let mut r = BinauralRenderer::new(48_000);
        r.render_frame(
            &input,
            1,
            n,
            &wet_params,
            &pos,
            &[ChannelGain::flat(1.0)],
            &[],
            None,
            &mut wet,
        );

        assert!(tail(&dry) < 1e-9, "dry render must have no late energy");
        assert!(
            tail(&wet) > 1e-6,
            "reflections produced no late energy: {}",
            tail(&wet)
        );
    }

    /// A source outside the room (unit scale 3, default 4 m width) must not
    /// get a reflection on top of its direct sound: the first samples of
    /// the render carry the direct HRIR only, exactly as for the same
    /// source with reflections off, and the wall copies land later.
    #[test]
    fn reflections_of_an_outside_source_do_not_coincide_with_the_direct() {
        let n = 2_048;
        let mut input = vec![0.0f32; n];
        input[0] = 1.0;
        let pos = [[1.0, 0.0, 0.0]]; // 3 m to the right at unit scale 3
        let render = |enabled: bool| -> Vec<f32> {
            let params = BinauralFrameParams {
                unit_scale_m: 3.0,
                reflections: BinauralReflections {
                    enabled,
                    room_size_m: [4.0, 5.0, 2.7],
                    level: 0.5,
                },
                ..dry_params()
            };
            let mut out = vec![0.0f32; n * 2];
            let mut r = BinauralRenderer::new(48_000);
            r.render_frame(
                &input,
                1,
                n,
                &params,
                &pos,
                &[ChannelGain::flat(1.0)],
                &[],
                None,
                &mut out,
            );
            out
        };
        let (dry, wet) = (render(false), render(true));
        // The near wall's image sits 2·margin = 0.1 m beyond the clamped
        // source: 14 samples at 48 kHz. Before that, wet == dry.
        let head = 10 * 2;
        let diff: f32 = dry[..head]
            .iter()
            .zip(&wet[..head])
            .map(|(a, b)| (a - b).abs())
            .sum();
        assert!(
            diff < 1e-6,
            "a reflection coincides with the direct sound: {diff}"
        );
        let later: f32 = wet[head..]
            .iter()
            .zip(&dry[head..])
            .map(|(a, b)| (a - b) * (a - b))
            .sum();
        assert!(later > 1e-6, "no reflections at all: {later}");
    }

    /// Energy of an impulse render split at `split` samples: the direct
    /// HRIR (and, for the second half, whatever arrives later).
    fn head_tail_energy(out: &[f32], split: usize) -> (f32, f32) {
        let head: f32 = out[..split * 2].iter().map(|x| x * x).sum();
        let tail: f32 = out[split * 2..].iter().map(|x| x * x).sum();
        (head, tail)
    }

    /// The early reflections must get *louder relative to the direct sound*
    /// as the source recedes — that is the distance cue they exist for.
    /// Source straight ahead at 1 m then 2.4 m in the default room: before
    /// the fix the ratio fell from −18.3 dB to −19.4 dB; the physics (direct
    /// in 1/d) says −18.3 → −11.8 dB.
    #[test]
    fn reflection_to_direct_ratio_rises_with_distance() {
        let n = 4_096;
        let mut input = vec![0.0f32; n];
        input[0] = 1.0;
        let ratio = |dist: f64| -> f32 {
            let params = BinauralFrameParams {
                reflections: BinauralReflections {
                    enabled: true,
                    room_size_m: [4.0, 5.0, 2.7],
                    level: 0.5,
                },
                ..dry_params()
            };
            let mut out = vec![0.0f32; n * 2];
            let mut r = BinauralRenderer::new(48_000);
            r.render_frame(
                &input,
                1,
                n,
                &params,
                &[[0.0, dist, 0.0]],
                &[ChannelGain::flat(1.0)],
                &[],
                None,
                &mut out,
            );
            // The nearest image (front wall) of the 2.4 m source is 0.2 m
            // beyond it: 28 samples. Split after the direct HRIR but before
            // that: 20 samples — the direct kernel's head holds its energy.
            let (direct, refl) = head_tail_energy(&out, 20);
            refl / direct
        };
        let (near, far) = (ratio(1.0), ratio(2.4));
        assert!(
            far > near * 2.0,
            "reflections did not rise with distance: 1 m {near:.4} vs 2.4 m {far:.4}"
        );
    }

    /// Likewise the reverb send: three times the distance, three times the
    /// send (nine times the tail energy), where it used to gain 30 %.
    #[test]
    fn reverb_send_grows_with_distance() {
        let n = 24_000;
        let mut input = vec![0.0f32; n];
        input[0] = 1.0;
        let tail = |dist: f64| -> f32 {
            let params = BinauralFrameParams {
                reverb: BinauralReverb {
                    enabled: true,
                    level: 0.3,
                    rt60_s: 0.4,
                    predelay_ms: 20.0,
                },
                ..dry_params()
            };
            let mut out = vec![0.0f32; n * 2];
            let mut r = BinauralRenderer::new(48_000);
            r.render_frame(
                &input,
                1,
                n,
                &params,
                &[[0.0, dist, 0.0]],
                &[ChannelGain::flat(1.0)],
                &[],
                None,
                &mut out,
            );
            head_tail_energy(&out, 4_000).1
        };
        let (near, far) = (tail(1.0), tail(3.0));
        assert!(
            far > near * 4.0 && far < near * 20.0,
            "reverb send off the 1/d law: 1 m {near:.3e} vs 3 m {far:.3e}"
        );
    }

    #[test]
    fn reverb_adds_a_long_tail() {
        let n = 24_000; // 0.5 s
        let mut input = vec![0.0f32; n];
        input[0] = 1.0;
        let pos = [[0.0, 2.0, 0.0]];
        // Energy far past both the HRIR tail and the early-reflection window.
        let tail = |out: &[f32]| -> f32 { out[4_000 * 2..].iter().map(|x| x * x).sum() };

        let mut dry = vec![0.0f32; n * 2];
        let mut r = BinauralRenderer::new(48_000);
        r.render_frame(
            &input,
            1,
            n,
            &dry_params(),
            &pos,
            &[ChannelGain::flat(1.0)],
            &[],
            None,
            &mut dry,
        );
        assert!(tail(&dry) < 1e-9, "dry render must have no tail");

        let wet_params = BinauralFrameParams {
            reverb: BinauralReverb {
                enabled: true,
                level: 0.3,
                rt60_s: 0.4,
                predelay_ms: 20.0,
            },
            ..dry_params()
        };
        let mut wet = vec![0.0f32; n * 2];
        let mut r = BinauralRenderer::new(48_000);
        r.render_frame(
            &input,
            1,
            n,
            &wet_params,
            &pos,
            &[ChannelGain::flat(1.0)],
            &[],
            None,
            &mut wet,
        );
        assert!(tail(&wet) > 1e-7, "no reverb tail: {}", tail(&wet));
    }

    #[test]
    fn air_absorption_dulls_distant_sources() {
        // Nyquist-rate tone: a distance low-pass crushes it. Compare output
        // energy near vs far (the direct path has no 1/d gain to factor out).
        let n = 2_048;
        let input: Vec<f32> = (0..n)
            .map(|i| if i % 2 == 0 { 1.0 } else { -1.0 })
            .collect();
        let render = |dist: f64| -> f32 {
            let params = BinauralFrameParams {
                air_absorption: true,
                ..dry_params()
            };
            let mut out = vec![0.0f32; n * 2];
            let mut r = BinauralRenderer::new(48_000);
            r.render_frame(
                &input,
                1,
                n,
                &params,
                &[[0.0, dist, 0.0]],
                &[ChannelGain::flat(1.0)],
                &[],
                None,
                &mut out,
            );
            // Direct path no longer applies a 1/d gain, so the broadband level is
            // distance-independent; the only near/far difference is the
            // air-absorption HF roll-off under test.
            out[400 * 2..].iter().map(|x| x * x).sum()
        };
        let near = render(2.0); // within the 3 m bypass → full HF
        let far = render(30.0); // ~5 kHz cutoff → Nyquist crushed
        assert!(
            far < near * 0.2,
            "air absorption ineffective: near={near} far={far}"
        );
    }

    #[test]
    fn direct_level_is_distance_invariant() {
        // Object/bed level is authored (Atmos); the binaural direct path must
        // NOT re-attenuate by 1/d. A front source rendered near vs far — same
        // authored gain, air/reverb/reflections off — must yield identical
        // broadband energy. Before the fix this differed by the 1/d clamp
        // (here 2 m → ×0.5 vs 8 m → ×0.125, a 4× energy gap).
        let (nl, nr) = render_single([0.0, 2.0, 0.0]); // near
        let (fl, fr) = render_single([0.0, 8.0, 0.0]); // far
        let near = nl + nr;
        let far = fl + fr;
        assert!(near > 0.0, "near energy must be non-zero");
        assert!(
            (far - near).abs() <= near * 1e-6,
            "direct object level changed with distance: near={near} far={far}"
        );
    }

    /// Noise through a channel with reflections on, then the channel goes
    /// silent for longer than every history it holds, then its gain returns
    /// on silent input: the output must be silence. Before the drain, the
    /// reflection ring (a quarter second of the noise) and the convolver
    /// and ITD windows were frozen at the mute and played back at the
    /// unmute.
    #[test]
    fn unmuting_does_not_replay_frozen_audio() {
        let mut r = BinauralRenderer::new(48_000);
        let n = 480;
        let params = BinauralFrameParams {
            reflections: BinauralReflections {
                enabled: true,
                room_size_m: [4.0, 5.0, 2.7],
                level: 0.5,
            },
            ..dry_params()
        };
        let pos = [[0.6, 0.8, 0.0]];
        let noise: Vec<f32> = (0..n)
            .map(|i| ((i * 7919) % 1000) as f32 / 500.0 - 1.0)
            .collect();
        let silence = vec![0.0f32; n];
        let mut out = vec![0.0f32; n * 2];
        let mut render = |r: &mut BinauralRenderer, input: &[f32], gain: f32| -> f32 {
            out.iter_mut().for_each(|v| *v = 0.0);
            r.render_frame(
                input,
                1,
                n,
                &params,
                &pos,
                &[ChannelGain::flat(gain)],
                &[],
                None,
                &mut out,
            );
            out.iter().map(|v| v * v).sum::<f32>()
        };
        for _ in 0..10 {
            render(&mut r, &noise, 1.0);
        }
        // Muted for 0.4 s (40 blocks), longer than the 0.25 s reflection ring.
        for _ in 0..40 {
            render(&mut r, &silence, 0.0);
        }
        let back = render(&mut r, &silence, 1.0);
        assert!(
            back < 1e-12,
            "stale audio replayed at unmute: energy {back}"
        );
    }

    /// The other half of the contract: a channel that just went silent must
    /// keep sounding for a moment — its reflections are still on their way.
    #[test]
    fn a_muted_channel_keeps_its_reflection_tail() {
        let mut r = BinauralRenderer::new(48_000);
        let n = 480;
        let params = BinauralFrameParams {
            reflections: BinauralReflections {
                enabled: true,
                room_size_m: [4.0, 5.0, 2.7],
                level: 0.5,
            },
            ..dry_params()
        };
        let pos = [[0.0, 1.0, 0.0]];
        let mut input = vec![0.0f32; n];
        input[0] = 1.0;
        let mut out = vec![0.0f32; n * 2];
        r.render_frame(
            &input,
            1,
            n,
            &params,
            &pos,
            &[ChannelGain::flat(1.0)],
            &[],
            None,
            &mut out,
        );
        // The next block is muted with silent input: the wall reflections
        // of the impulse (8 ms and beyond) land here and must be audible.
        let silence = vec![0.0f32; n];
        out.iter_mut().for_each(|v| *v = 0.0);
        r.render_frame(
            &silence,
            1,
            n,
            &params,
            &pos,
            &[ChannelGain::flat(0.0)],
            &[],
            None,
            &mut out,
        );
        let energy: f32 = out.iter().map(|v| v * v).sum();
        assert!(
            energy > 1e-9,
            "the reflection tail was cut at the mute: {energy}"
        );
    }

    /// A ramped block applies `start + step·s` per sample: DC through a
    /// front source with a gain ramping 0 → 1 over the block comes out as
    /// a ramp, not as the block-end constant.
    #[test]
    fn gain_ramps_per_sample_within_the_block() {
        let n = 256;
        let input = vec![1.0f32; n];
        let pos = [[0.0, 1.0, 0.0]];
        let mut ramped = vec![0.0f32; n * 2];
        let mut r = BinauralRenderer::new(48_000);
        // Settle the kernel crossfade first, at gain 0 (silent, no drain).
        let mut warm = vec![0.0f32; n * 2];
        r.render_frame(
            &vec![0.0f32; n],
            1,
            n,
            &dry_params(),
            &pos,
            &[ChannelGain::flat(0.0)],
            &[],
            None,
            &mut warm,
        );
        r.render_frame(
            &input,
            1,
            n,
            &dry_params(),
            &pos,
            &[ChannelGain {
                start: 0.0,
                step: 1.0 / n as f32,
            }],
            &[],
            None,
            &mut ramped,
        );
        // The pre-convolution signal is s/n; after the settled front kernel
        // (unity-ish DC gain) the output must grow across the block, with
        // its first quarter well below its last quarter.
        let quarter = n / 4;
        let head: f32 = ramped[..quarter * 2].iter().map(|v| v.abs()).sum::<f32>() / quarter as f32;
        let tail: f32 = ramped[(n - quarter) * 2..]
            .iter()
            .map(|v| v.abs())
            .sum::<f32>()
            / quarter as f32;
        assert!(
            tail > 0.0 && head < 0.4 * tail,
            "no per-sample ramp: head {head} tail {tail}"
        );
        // And the ramp is monotonic in the large: the left-ear output at
        // sample 64 sits between those at 32 and 128 (after the HRIR settles).
        let l = |s: usize| ramped[s * 2].abs();
        assert!(
            l(32) < l(64) && l(64) < l(128),
            "{} {} {}",
            l(32),
            l(64),
            l(128)
        );
    }

    #[test]
    fn muted_channel_is_silent() {
        let mut r = BinauralRenderer::new(48_000);
        let n = 64;
        let input = vec![1.0f32; n];
        let mut out = vec![0.0f32; n * 2];
        r.render_frame(
            &input,
            1,
            n,
            &dry_params(),
            &[[1.0, 0.0, 0.0]],
            &[ChannelGain::flat(0.0)],
            &[],
            None,
            &mut out,
        );
        assert!(out.iter().all(|&x| x == 0.0));
    }
}
