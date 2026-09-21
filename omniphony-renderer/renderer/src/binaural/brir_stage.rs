//! The BRIR render stage of the cascaded binaural path.
//!
//! The cascade ([`crate::spatial_renderer::cascade`]) mixes the programme
//! onto the app's speaker layout as a virtual room; with a room impulse
//! response set selected ([`super::HrirSource::Brir`]) this stage takes over
//! from the HRTF binaural stage and convolves each virtual speaker bus with
//! the pair measured from the nearest emitter of the set, at the head
//! orientation nearest the tracked one. Nothing else is applied: the
//! propagation delay, the interaural delay, the reflections and the tail are
//! the measurement. A non-spatialized bus (the LFE) is fed to both ears at
//! constant power, as on the HRTF path.
//!
//! # Streaming
//!
//! One [`InputHistory`] per bus and a shared [`ConvolutionPlan`] of
//! [`BRIR_BLOCK`] samples. When the buses complete a block, every bus is
//! analysed once, and for each ear the kernels of every bus are accumulated
//! in the frequency domain before a single inverse transform — one IFFT per
//! ear per block however many buses. The host's frames are decoupled from
//! the block: samples are pushed one at a time and the output is read from
//! a block-sized FIFO, so the stage adds exactly `BRIR_BLOCK − 1` samples of
//! latency, reported through [`BrirStage::latency_samples`].
//!
//! # Kernels and the worker
//!
//! The set stays in the time domain; only the *active orientation* is
//! partitioned, as a [`KernelBank`] (one pair per emitter). Banks and sets
//! are built on a worker thread and handed over through `ArcSwapOption`
//! slots: the audio thread compares, sends a request, and keeps convolving
//! the current bank until the new one lands. A bank swap is blended over
//! one block ([`ConvolutionPlan::finish_blend`]); retired banks and sets go
//! back to the worker to be freed. The set loaded for a head-tracked
//! listener holds every orientation; without tracking only the one nearest
//! straight ahead is resident (see [`crate::live_params::BrirLiveParams`]).

use std::sync::Arc;
use std::sync::mpsc;

use arc_swap::ArcSwapOption;

use super::brir::{BrirLoadOptions, BrirSet};
use super::head_pose::HeadPose;
use crate::partitioned_conv::{ConvolutionPlan, InputHistory, OutputScratch, PartitionedKernel};

/// Partition size of the BRIR convolution, in samples. Sets the stage's own
/// latency (`BRIR_BLOCK − 1`) and the size of the burst of work at each
/// block boundary; the work per output sample is set by the kernel length,
/// not by this.
pub const BRIR_BLOCK: usize = 128;

/// Gain of a direct (non-spatialized) bus into each ear: constant power,
/// the binaural stage's standing policy for the LFE (issue #156).
const DIRECT_EAR_GAIN: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Angle between a virtual speaker and the emitter it is rendered from
/// above which the mapping is logged as a mismatch, degrees.
const MISMATCH_WARN_DEG: f32 = 10.0;

/// What the last BRIR load produced, for the control surface.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BrirStatus {
    /// The file asked for (empty when no BRIR source is selected).
    pub path: String,
    /// The resident set, when the load succeeded.
    pub loaded: Option<BrirSummary>,
    /// Why the file is not in use (the cascade then runs on the HRTF stage).
    pub error: Option<String>,
}

/// Shape of a resident BRIR set.
#[derive(Debug, Clone, PartialEq)]
pub struct BrirSummary {
    pub conventions: String,
    pub emitters: usize,
    pub orientations: usize,
    pub max_taps: usize,
    pub sample_rate: u32,
    pub bytes: usize,
}

impl BrirSummary {
    fn of(set: &BrirSet) -> Self {
        Self {
            conventions: set.conventions().to_string(),
            emitters: set.emitters().len(),
            orientations: set.orientations().len(),
            max_taps: set.max_taps(),
            sample_rate: set.sample_rate(),
            bytes: set.bytes(),
        }
    }
}

/// Where the stage reports each load's [`BrirStatus`].
pub type BrirStatusSink = Arc<dyn Fn(BrirStatus) + Send + Sync>;

/// What a load is identified by: the file and the load options. A change
/// reloads.
#[derive(Debug, Clone, PartialEq)]
struct LoadKey {
    path: String,
    opts: BrirLoadOptions,
}

/// The partitioned pairs of every emitter at one head orientation.
pub(crate) struct KernelBank {
    /// Identity of the set the bank was built from (`Arc` pointer).
    set_id: usize,
    orientation: usize,
    /// `[emitter] → [left, right]`.
    kernels: Vec<[PartitionedKernel; 2]>,
}

fn set_id(set: &Arc<BrirSet>) -> usize {
    Arc::as_ptr(set) as usize
}

fn build_bank(plan: &ConvolutionPlan, set: &Arc<BrirSet>, orientation: usize) -> KernelBank {
    let kernels = (0..set.emitters().len())
        .map(|e| {
            let pair = set.pair(e, orientation);
            [plan.partition(&pair.left), plan.partition(&pair.right)]
        })
        .collect();
    KernelBank {
        set_id: set_id(set),
        orientation,
        kernels,
    }
}

/// A loaded set with everything the audio thread needs to start on it.
struct Loaded {
    key: LoadKey,
    set: Arc<BrirSet>,
    bank: Arc<KernelBank>,
    /// One history per bus, sized for the set's longest kernel.
    inputs: Vec<InputHistory>,
}

enum Request {
    Load {
        key: LoadKey,
        buses: usize,
    },
    Bank {
        set: Arc<BrirSet>,
        orientation: usize,
    },
    /// Something the audio thread retired (a set, a bank, histories):
    /// carried here only to be dropped on the worker, not there.
    Drop(Retired),
}

type Retired = Box<dyn std::any::Any + Send>;

/// See the module doc.
pub struct BrirStage {
    sample_rate: u32,
    plan: ConvolutionPlan,
    /// The load last asked for.
    key: Option<LoadKey>,
    set: Option<Arc<BrirSet>>,
    bank: Option<Arc<KernelBank>>,
    /// The bank on its way out during the block a swap lands in.
    fade_from: Option<Arc<KernelBank>>,
    /// Orientation requested from the worker and not delivered yet.
    pending_orientation: Option<usize>,
    incoming_set: Arc<ArcSwapOption<Loaded>>,
    incoming_bank: Arc<ArcSwapOption<KernelBank>>,
    request_tx: mpsc::Sender<Request>,
    /// Per bus: the emitter it is rendered from, `None` for a direct bus.
    bus_emitter: Vec<Option<usize>>,
    /// Identity of the bus geometry `bus_emitter` was built for.
    geometry_id: usize,
    /// Identity of the set `bus_emitter` was built against.
    mapped_set_id: usize,
    inputs: Vec<InputHistory>,
    scratch: [OutputScratch; 2],
    ear_block: [Vec<f32>; 2],
    /// Interleaved stereo output of the last completed block.
    fifo: Vec<f32>,
    read_pos: usize,
    /// Bumped on every set swap.
    set_generation: u64,
}

impl BrirStage {
    /// A stage whose load status goes nowhere.
    pub fn new(sample_rate: u32) -> Self {
        Self::with_status_sink(sample_rate, Arc::new(|_| {}))
    }

    /// A stage reporting every load's [`BrirStatus`] to `sink`.
    pub fn with_status_sink(sample_rate: u32, sink: BrirStatusSink) -> Self {
        let plan = ConvolutionPlan::new(BRIR_BLOCK);
        let incoming_set: Arc<ArcSwapOption<Loaded>> = Arc::new(ArcSwapOption::empty());
        let incoming_bank: Arc<ArcSwapOption<KernelBank>> = Arc::new(ArcSwapOption::empty());
        let (request_tx, request_rx) = mpsc::channel::<Request>();
        {
            let plan = plan.clone();
            let set_slot = Arc::clone(&incoming_set);
            let bank_slot = Arc::clone(&incoming_bank);
            std::thread::Builder::new()
                .name("binaural-brir-worker".into())
                .spawn(move || {
                    Self::worker(request_rx, plan, sample_rate, sink, set_slot, bank_slot)
                })
                .expect("spawn BRIR worker");
        }
        Self {
            sample_rate,
            scratch: [plan.make_scratch(), plan.make_scratch()],
            plan,
            key: None,
            set: None,
            bank: None,
            fade_from: None,
            pending_orientation: None,
            incoming_set,
            incoming_bank,
            request_tx,
            bus_emitter: Vec::new(),
            geometry_id: usize::MAX,
            mapped_set_id: 0,
            inputs: Vec::new(),
            ear_block: [vec![0.0; BRIR_BLOCK], vec![0.0; BRIR_BLOCK]],
            fifo: vec![0.0; 2 * BRIR_BLOCK],
            read_pos: 0,
            set_generation: 0,
        }
    }

    /// The worker: loads sets, partitions banks, frees what the audio
    /// thread retires. Requests are coalesced: a load supersedes everything
    /// queued before it, and only the latest bank request is built.
    fn worker(
        rx: mpsc::Receiver<Request>,
        plan: ConvolutionPlan,
        sample_rate: u32,
        sink: BrirStatusSink,
        set_slot: Arc<ArcSwapOption<Loaded>>,
        bank_slot: Arc<ArcSwapOption<KernelBank>>,
    ) {
        while let Ok(first) = rx.recv() {
            let mut load: Option<(LoadKey, usize)> = None;
            let mut bank: Option<(Arc<BrirSet>, usize)> = None;
            let mut handle = |req: Request| match req {
                Request::Load { key, buses } => {
                    load = Some((key, buses));
                    bank = None;
                }
                Request::Bank { set, orientation } => bank = Some((set, orientation)),
                Request::Drop(retired) => drop(retired),
            };
            handle(first);
            while let Ok(next) = rx.try_recv() {
                handle(next);
            }
            if let Some((key, buses)) = load {
                match Self::load(&key, sample_rate) {
                    Ok(set) => {
                        let set = Arc::new(set);
                        let (yaw, pitch) = (0.0, 0.0);
                        let front = set.nearest_orientation(yaw, pitch);
                        let bank = Arc::new(build_bank(&plan, &set, front));
                        let capacity = plan.partitions_for(set.max_taps());
                        let inputs = (0..buses).map(|_| plan.make_input(capacity)).collect();
                        sink(BrirStatus {
                            path: key.path.clone(),
                            loaded: Some(BrirSummary::of(&set)),
                            error: None,
                        });
                        set_slot.store(Some(Arc::new(Loaded {
                            key,
                            set,
                            bank,
                            inputs,
                        })));
                    }
                    Err(e) => {
                        log::warn!(
                            "binaural: BRIR '{}' unavailable ({e}); the cascade runs on the HRTF stage",
                            key.path
                        );
                        sink(BrirStatus {
                            path: key.path.clone(),
                            loaded: None,
                            error: Some(e),
                        });
                    }
                }
            } else if let Some((set, orientation)) = bank {
                bank_slot.store(Some(Arc::new(build_bank(&plan, &set, orientation))));
            }
        }
    }

    #[cfg(feature = "sofa")]
    fn load(key: &LoadKey, sample_rate: u32) -> Result<BrirSet, String> {
        if key.path.trim().is_empty() {
            return Err("no BRIR file selected".to_string());
        }
        BrirSet::from_sofa(&key.path, sample_rate, &key.opts).map_err(|e| e.to_string())
    }

    #[cfg(not(feature = "sofa"))]
    fn load(_key: &LoadKey, _sample_rate: u32) -> Result<BrirSet, String> {
        Err("SOFA support not built into this renderer (enable the 'sofa' feature)".to_string())
    }

    /// Engine rate the stage was built for.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Samples the stage delays the buses by: `BRIR_BLOCK − 1`.
    pub fn latency_samples(&self) -> usize {
        self.plan.latency_samples()
    }

    /// Whether a set and its bank are resident: the stage can render.
    pub fn is_ready(&self) -> bool {
        self.set.is_some() && self.bank.is_some()
    }

    /// Bumped on every set swap (observable by tests and diagnostics).
    pub fn set_generation(&self) -> u64 {
        self.set_generation
    }

    /// The resident set, if any.
    pub fn set(&self) -> Option<&Arc<BrirSet>> {
        self.set.as_ref()
    }

    /// Orientation index of the bank being convolved.
    pub fn bank_orientation(&self) -> Option<usize> {
        self.bank.as_ref().map(|b| b.orientation)
    }

    fn retire(&self, r: Retired) {
        // A closed channel means the worker is gone (the stage is being torn
        // down); dropping here is then the only option.
        let _ = self.request_tx.send(Request::Drop(r));
    }

    /// One block's worth of frames on `bus` (tests, timing).
    #[cfg(test)]
    fn process_one_block_for_timing(&mut self, bus: &[f32], total: usize, head_pose: HeadPose) {
        let mut out = vec![0.0f32; 2 * BRIR_BLOCK];
        self.render_frame(bus, total, BRIR_BLOCK, head_pose, &mut out);
    }

    /// Track the requested file and options; called once per frame from the
    /// audio thread. Steady state is one compare. A change sends a load
    /// request to the worker; the stage keeps its current set until the new
    /// one lands, then swaps (and `buses` histories come pre-built with it).
    pub fn ensure_loaded(&mut self, path: &str, opts: &BrirLoadOptions, buses: usize) {
        let changed = match &self.key {
            Some(k) => k.path != path || k.opts != *opts,
            None => true,
        };
        if changed {
            let key = LoadKey {
                path: path.to_string(),
                opts: *opts,
            };
            let _ = self.request_tx.send(Request::Load {
                key: key.clone(),
                buses,
            });
            self.key = Some(key);
        }
        if let Some(loaded) = self.incoming_set.swap(None) {
            let stale = self.key.as_ref() != Some(&loaded.key);
            // `Arc<Loaded>` is uniquely ours now: take its parts.
            match Arc::try_unwrap(loaded) {
                Ok(loaded) if !stale => self.adopt(loaded),
                Ok(loaded) => {
                    self.retire(Box::new(loaded.set));
                    self.retire(Box::new(loaded.bank));
                    self.retire(Box::new(loaded.inputs));
                }
                Err(_) => unreachable!("the incoming slot held the only reference"),
            }
        }
    }

    /// Swap a freshly loaded set in, retiring the previous one.
    fn adopt(&mut self, loaded: Loaded) {
        if let Some(old) = self.set.replace(loaded.set) {
            self.retire(Box::new(old));
        }
        if let Some(old) = self.bank.replace(loaded.bank) {
            self.retire(Box::new(old));
        }
        if let Some(old) = self.fade_from.take() {
            self.retire(Box::new(old));
        }
        let old_inputs = std::mem::replace(&mut self.inputs, loaded.inputs);
        if !old_inputs.is_empty() {
            self.retire(Box::new(old_inputs));
        }
        self.pending_orientation = None;
        self.fifo.fill(0.0);
        self.read_pos = 0;
        self.set_generation += 1;
    }

    /// Install a set directly (tests): the front bank is built on the
    /// calling thread and the stage is ready on return.
    #[cfg(test)]
    pub(crate) fn install_set(&mut self, set: Arc<BrirSet>, buses: usize) {
        let front = set.nearest_orientation(0.0, 0.0);
        let bank = Arc::new(build_bank(&self.plan, &set, front));
        let capacity = self.plan.partitions_for(set.max_taps());
        let inputs = (0..buses).map(|_| self.plan.make_input(capacity)).collect();
        self.adopt(Loaded {
            key: LoadKey {
                path: String::new(),
                opts: BrirLoadOptions::default(),
            },
            set,
            bank,
            inputs,
        });
    }

    /// Map the virtual speakers onto the set's emitters and size the
    /// histories. `geometry_id` identifies the bus geometry (the cascade's
    /// topology identity); steady state is two compares. A bus is rendered
    /// from the emitter nearest to it in direction; mismatches beyond
    /// [`MISMATCH_WARN_DEG`] and emitters shared by several buses are
    /// logged once per mapping.
    pub fn configure_buses(&mut self, positions: &[[f64; 3]], direct: &[bool], geometry_id: usize) {
        let Some(set) = self.set.as_ref() else {
            return;
        };
        let sid = set_id(set);
        let total = positions.len();
        if self.geometry_id != geometry_id
            || self.mapped_set_id != sid
            || self.bus_emitter.len() != total
        {
            self.bus_emitter.clear();
            let emitters = set.emitters();
            let unit = |p: [f32; 3]| {
                let n = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt();
                if n > 1e-9 {
                    [p[0] / n, p[1] / n, p[2] / n]
                } else {
                    [0.0, 1.0, 0.0]
                }
            };
            let emitter_dirs: Vec<[f32; 3]> = emitters.iter().map(|&e| unit(e)).collect();
            let mut uses = vec![0usize; emitters.len()];
            for (b, p) in positions.iter().enumerate() {
                if direct.get(b).copied().unwrap_or(false) || emitters.is_empty() {
                    self.bus_emitter.push(None);
                    continue;
                }
                let d = unit([p[0] as f32, p[1] as f32, p[2] as f32]);
                let (best, dot) = emitter_dirs
                    .iter()
                    .enumerate()
                    .map(|(i, e)| (i, d[0] * e[0] + d[1] * e[1] + d[2] * e[2]))
                    .fold(
                        (0, f32::NEG_INFINITY),
                        |acc, x| if x.1 > acc.1 { x } else { acc },
                    );
                let angle = dot.clamp(-1.0, 1.0).acos().to_degrees();
                if angle > MISMATCH_WARN_DEG {
                    log::warn!(
                        "BRIR: virtual speaker {b} is {angle:.0}° from its nearest emitter {best}; the set has no loudspeaker there"
                    );
                }
                uses[best] += 1;
                self.bus_emitter.push(Some(best));
            }
            for (e, &n) in uses.iter().enumerate() {
                if n > 1 {
                    log::warn!("BRIR: emitter {e} serves {n} virtual speakers");
                }
            }
            log::info!(
                "BRIR: {} virtual speakers → {} emitters used of {} ({} direct)",
                total,
                uses.iter().filter(|&&n| n > 0).count(),
                emitters.len(),
                self.bus_emitter.iter().filter(|e| e.is_none()).count()
            );
            self.geometry_id = geometry_id;
            self.mapped_set_id = sid;
        }
        // Histories: pre-built by the load for the bus count then; a relayout
        // since is the rare case that allocates here.
        let capacity = self.plan.partitions_for(set.max_taps());
        if self.inputs.len() != total
            || self.inputs.first().is_some_and(|i| i.capacity() < capacity)
        {
            let old = std::mem::replace(
                &mut self.inputs,
                (0..total).map(|_| self.plan.make_input(capacity)).collect(),
            );
            if !old.is_empty() {
                self.retire(Box::new(old));
            }
            self.fifo.fill(0.0);
            self.read_pos = 0;
        }
    }

    /// Per bus: the emitter it is rendered from (`None` = direct).
    pub fn bus_emitters(&self) -> &[Option<usize>] {
        &self.bus_emitter
    }

    /// Convolve one frame of interleaved buses (`total` per sample) into
    /// `out` (interleaved stereo, `2 · sample_length`, added to). The stage
    /// must be ready and configured for `total` buses.
    pub fn render_frame(
        &mut self,
        bus: &[f32],
        total: usize,
        sample_length: usize,
        head_pose: HeadPose,
        out: &mut [f32],
    ) {
        debug_assert_eq!(out.len(), 2 * sample_length);
        debug_assert_eq!(bus.len(), total * sample_length);
        if !self.is_ready() || self.inputs.len() != total || total == 0 {
            return;
        }
        for i in 0..sample_length {
            let frame = &bus[i * total..(i + 1) * total];
            let mut complete = false;
            for (input, &x) in self.inputs.iter_mut().zip(frame) {
                complete = input.push(x);
            }
            if complete {
                self.process_block(head_pose);
            }
            out[2 * i] += self.fifo[2 * self.read_pos];
            out[2 * i + 1] += self.fifo[2 * self.read_pos + 1];
            self.read_pos += 1;
        }
    }

    /// One completed block: analyse every bus, follow the head, sum the
    /// kernels per ear, blend a landing bank swap, add the direct buses.
    fn process_block(&mut self, head_pose: HeadPose) {
        for input in &mut self.inputs {
            self.plan.analyze(input);
        }
        let set = self.set.as_ref().expect("ready");
        let sid = set_id(set);

        // Follow the head: ask for the nearest measured orientation when it
        // differs from the bank's and is not already on its way.
        if set.orientations().len() > 1 {
            let (yaw, pitch) = head_pose.yaw_pitch_deg();
            let wanted = set.nearest_orientation(yaw, pitch);
            let current = self.bank.as_ref().map(|b| b.orientation);
            if current != Some(wanted) && self.pending_orientation != Some(wanted) {
                let _ = self.request_tx.send(Request::Bank {
                    set: Arc::clone(set),
                    orientation: wanted,
                });
                self.pending_orientation = Some(wanted);
            }
        }
        if let Some(bank) = self.incoming_bank.swap(None) {
            if bank.set_id == sid {
                if self.pending_orientation == Some(bank.orientation) {
                    self.pending_orientation = None;
                }
                let old = self.bank.replace(bank);
                if let Some(prev) = self.fade_from.replace(old.expect("ready")) {
                    // Two swaps in one block: the middle bank never played.
                    self.retire(Box::new(prev));
                }
            } else {
                self.retire(Box::new(bank));
            }
        }

        let bank = self.bank.as_ref().expect("ready");
        let plan = &self.plan;
        let inputs = &self.inputs;
        let bus_emitter = &self.bus_emitter;
        for ear in 0..2 {
            let scratch = &mut self.scratch[ear];
            let out = &mut self.ear_block[ear];
            // The outgoing bank first when a swap lands, then the new one
            // ramps in over the block.
            let first = self.fade_from.as_deref().unwrap_or(bank);
            scratch.clear();
            for (input, e) in inputs.iter().zip(bus_emitter) {
                if let Some(e) = e {
                    plan.accumulate(input, &first.kernels[*e][ear], scratch);
                }
            }
            plan.finish(scratch, out);
            if self.fade_from.is_some() {
                scratch.clear();
                for (input, e) in inputs.iter().zip(bus_emitter) {
                    if let Some(e) = e {
                        plan.accumulate(input, &bank.kernels[*e][ear], scratch);
                    }
                }
                plan.finish_blend(scratch, out);
            }
            // Direct buses: the block just analysed is the one this output
            // block corresponds to, so no extra delay is needed.
            for (input, e) in inputs.iter().zip(bus_emitter) {
                if e.is_none() {
                    for (o, &x) in out.iter_mut().zip(input.last_block()) {
                        *o += DIRECT_EAR_GAIN * x;
                    }
                }
            }
        }
        if let Some(old) = self.fade_from.take() {
            self.retire(Box::new(old));
        }
        for i in 0..BRIR_BLOCK {
            self.fifo[2 * i] = self.ear_block[0][i];
            self.fifo[2 * i + 1] = self.ear_block[1][i];
        }
        self.read_pos = 0;
    }
}

/// Synthetic sets for the stage's tests and the renderer's.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Arc;

    use crate::binaural::brir::{BrirLoadOptions, BrirSet, RawRoomIr};

    /// A synthetic `MultiSpeakerBRIR`-shaped set: emitters at SOFA azimuths
    /// `spk_az` (2 m, SOFA sign: left positive), views at SOFA azimuths
    /// `yaws`. Each pair is an impulse at `40 + 5·e` samples of amplitude
    /// `0.5 + 0.1·e + 0.01·o`, negated on the right ear and given a crude
    /// interaural level difference (the ear on the emitter's side is 1.5×,
    /// the other 0.5×), plus a decaying tail so the kernels span several
    /// partitions. Tails are kept whole (no floor, no length bound).
    pub(crate) fn synth_set(spk_az: &[f32], yaws: &[f32], n: usize) -> Arc<BrirSet> {
        synth_set_decay(spk_az, yaws, n, 60.0)
    }

    /// [`synth_set`] with the tail's decay constant in samples (`60` there;
    /// a long one keeps a long response from vanishing under the floor).
    pub(crate) fn synth_set_decay(
        spk_az: &[f32],
        yaws: &[f32],
        n: usize,
        decay_samples: f32,
    ) -> Arc<BrirSet> {
        let sph = |az_deg: f32, r: f32| {
            let az = az_deg.to_radians();
            [r * az.cos(), r * az.sin(), 0.0]
        };
        let (m, r, e) = (yaws.len(), 2, spk_az.len());
        let mut ir = vec![0.0f32; m * r * e * n];
        for mi in 0..m {
            for ri in 0..r {
                for (k, &az) in spk_az.iter().enumerate() {
                    let base = ((mi * r + ri) * e + k) * n;
                    // SOFA azimuth is left-positive: `side` is +1 on the right.
                    let side = -az.to_radians().sin();
                    let ild = if ri == 0 {
                        1.0 - 0.5 * side
                    } else {
                        1.0 + 0.5 * side
                    };
                    let amp = (0.5 + 0.1 * k as f32 + 0.01 * mi as f32)
                        * ild
                        * if ri == 0 { 1.0 } else { -1.0 };
                    let d = 40 + 5 * k;
                    ir[base + d] = amp;
                    for t in 1..(n - d) {
                        let sign = if t % 2 == 0 { 1.0 } else { -0.7 };
                        ir[base + d + t] = amp * 0.3 * (-(t as f32) / decay_samples).exp() * sign;
                    }
                }
            }
        }
        let emitter: Vec<f32> = spk_az.iter().flat_map(|&a| sph(a, 2.0)).collect();
        let view: Vec<f32> = yaws.iter().flat_map(|&y| sph(y, 1.0)).collect();
        let raw = RawRoomIr {
            conventions: "MultiSpeakerBRIR",
            sample_rate: 48000.0,
            m,
            r,
            e,
            n,
            source_position: &[0.0, 0.0, 0.0],
            emitter_position: &emitter,
            listener_position: &[0.0, 0.0, 0.0],
            listener_view: &view,
            data_ir: &ir,
            data_delay: &[],
        };
        let opts = BrirLoadOptions {
            max_length_s: 0.0,
            tail_floor_db: 120.0,
            ..Default::default()
        };
        Arc::new(BrirSet::from_raw(&raw, 48000, &opts).unwrap())
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{synth_set, synth_set_decay};
    use super::*;

    /// Virtual speakers: left (−30°), right (+30°), and a direct LFE.
    fn buses() -> (Vec<[f64; 3]>, Vec<bool>) {
        let a = 30f64.to_radians();
        (
            vec![
                [-a.sin(), a.cos(), 0.0],
                [a.sin(), a.cos(), 0.0],
                [0.0, 1.0, 0.0],
            ],
            vec![false, false, true],
        )
    }

    fn ready_stage(set: &Arc<BrirSet>) -> BrirStage {
        let mut stage = BrirStage::new(48000);
        stage.install_set(Arc::clone(set), 3);
        let (pos, direct) = buses();
        stage.configure_buses(&pos, &direct, 1);
        stage
    }

    /// Render `frames` of `sample_length` from `bus` (interleaved, 3 wide)
    /// and return (left, right).
    fn run(
        stage: &mut BrirStage,
        bus: &[f32],
        sample_length: usize,
        head: HeadPose,
    ) -> (Vec<f32>, Vec<f32>) {
        let total = 3;
        let mut l = Vec::new();
        let mut r = Vec::new();
        for chunk in bus.chunks(total * sample_length) {
            let n = chunk.len() / total;
            let mut out = vec![0.0f32; 2 * n];
            stage.render_frame(chunk, total, n, head, &mut out);
            for f in out.chunks_exact(2) {
                l.push(f[0]);
                r.push(f[1]);
            }
        }
        (l, r)
    }

    fn impulse_on(bus: usize, len: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; 3 * len];
        v[bus] = 1.0;
        v
    }

    #[test]
    fn impulse_on_a_bus_returns_its_pair_delayed_by_the_block() {
        let set = synth_set(&[30.0, -30.0], &[0.0], 700);
        let mut stage = ready_stage(&set);
        assert!(stage.is_ready());
        assert_eq!(stage.bus_emitters(), &[Some(0), Some(1), None]);
        let len = 10 * BRIR_BLOCK;
        let (l, r) = run(&mut stage, &impulse_on(1, len), 40, HeadPose::identity());
        let pair = set.pair(1, 0);
        let lat = stage.latency_samples();
        let taps = pair.taps().min(len - lat);
        assert!(taps >= 600, "the pair spans several partitions: {taps}");
        for k in 0..taps {
            assert!(
                (l[k + lat] - pair.left[k]).abs() < 1e-4,
                "left tap {k}: {} vs {}",
                l[k + lat],
                pair.left[k]
            );
            assert!((r[k + lat] - pair.right[k]).abs() < 1e-4, "right tap {k}");
        }
        assert!(
            l[..lat].iter().all(|&v| v == 0.0),
            "nothing before the latency"
        );
    }

    #[test]
    fn direct_bus_feeds_both_ears_at_constant_power() {
        let set = synth_set(&[30.0, -30.0], &[0.0], 300);
        let mut stage = ready_stage(&set);
        let len = 3 * BRIR_BLOCK;
        let (l, r) = run(&mut stage, &impulse_on(2, len), 64, HeadPose::identity());
        let lat = stage.latency_samples();
        assert!((l[lat] - DIRECT_EAR_GAIN).abs() < 1e-6);
        assert!((r[lat] - DIRECT_EAR_GAIN).abs() < 1e-6);
        let rest: f32 = l.iter().chain(&r).map(|v| v.abs()).sum::<f32>() - 2.0 * DIRECT_EAR_GAIN;
        assert!(
            rest.abs() < 1e-6,
            "a direct bus is not convolved: residual {rest}"
        );
    }

    #[test]
    fn host_chunking_does_not_change_the_output() {
        let set = synth_set(&[30.0, -30.0], &[0.0], 500);
        let len = 5 * BRIR_BLOCK;
        let mut lcg = 0x1234_5678u32;
        let signal: Vec<f32> = (0..3 * len)
            .map(|_| {
                lcg = lcg.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (lcg >> 8) as f32 / (1 << 24) as f32 - 0.5
            })
            .collect();
        let mut a = ready_stage(&set);
        let mut b = ready_stage(&set);
        let (la, ra) = run(&mut a, &signal, 40, HeadPose::identity());
        let (lb, rb) = run(&mut b, &signal, 1000, HeadPose::identity());
        assert_eq!(la, lb);
        assert_eq!(ra, rb);
    }

    #[test]
    fn head_turn_switches_to_the_nearest_orientation() {
        // Views at SOFA −20, 0, +20 → renderer yaws +20, 0, −20 → sorted
        // indices 0 (−20), 1 (0), 2 (+20).
        let set = synth_set(&[30.0, -30.0], &[-20.0, 0.0, 20.0], 400);
        let mut stage = ready_stage(&set);
        assert_eq!(stage.bank_orientation(), Some(1));
        // Turn right by 18°: nearest is +20 (index 2). The bank comes from
        // the worker; keep rendering silence until it lands.
        let head = HeadPose::from_euler_deg(18.0, 0.0, 0.0);
        let silence = vec![0.0f32; 3 * BRIR_BLOCK];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while stage.bank_orientation() != Some(2) {
            assert!(
                std::time::Instant::now() < deadline,
                "bank for orientation 2 never landed"
            );
            let _ = run(&mut stage, &silence, BRIR_BLOCK, head);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        // Once the swap has played out, an impulse yields the new pair.
        let _ = run(&mut stage, &silence, BRIR_BLOCK, head);
        let len = 8 * BRIR_BLOCK;
        let (l, _) = run(&mut stage, &impulse_on(0, len), 40, head);
        let pair = set.pair(0, 2);
        let lat = stage.latency_samples();
        let taps = pair.taps().min(len - lat);
        assert!(taps >= 300, "{taps}");
        for k in 0..taps {
            assert!((l[k + lat] - pair.left[k]).abs() < 1e-4, "tap {k}");
        }
        assert!(l.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn unready_stage_is_silent_and_reports_its_latency() {
        let mut stage = BrirStage::new(48000);
        assert!(!stage.is_ready());
        assert_eq!(stage.latency_samples(), BRIR_BLOCK - 1);
        let mut out = vec![0.0f32; 80];
        stage.render_frame(&[1.0; 120], 3, 40, HeadPose::identity(), &mut out);
        assert!(out.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn a_new_set_is_adopted_and_the_old_one_retired() {
        let a = synth_set(&[30.0, -30.0], &[0.0], 300);
        let b = synth_set(&[0.0], &[0.0], 300);
        let mut stage = ready_stage(&a);
        let g = stage.set_generation();
        stage.install_set(Arc::clone(&b), 3);
        let (pos, direct) = buses();
        stage.configure_buses(&pos, &direct, 1);
        assert_eq!(stage.set_generation(), g + 1);
        // One emitter now: both spatialized buses render from it.
        assert_eq!(stage.bus_emitters(), &[Some(0), Some(0), None]);
        assert_eq!(Arc::strong_count(&b), 2, "the stage holds the new set");
    }

    /// Cost of the block-boundary burst for a realistic set: 12 virtual
    /// speakers (11 convolved + the LFE) on a 12-loudspeaker room with
    /// half-second responses at 48 kHz. Prints the mean and worst block
    /// time; the burst has to fit inside one host frame. Manual:
    /// `cargo test --release -p renderer --lib -- --ignored --nocapture brir_stage::tests::block_burst_timing`.
    #[test]
    #[ignore = "timing printout for a release build; not a gate"]
    fn block_burst_timing() {
        let az: Vec<f32> = (0..12).map(|i| -180.0 + 30.0 * i as f32).collect();
        let set = synth_set_decay(&az, &[0.0], 24_000, 6_000.0);
        assert!(
            set.max_taps() >= 23_000,
            "a half-second set: {}",
            set.max_taps()
        );
        let mut stage = BrirStage::new(48000);
        stage.install_set(Arc::clone(&set), 12);
        let positions: Vec<[f64; 3]> = az
            .iter()
            .map(|a| {
                let r = (-a).to_radians() as f64;
                [r.sin(), r.cos(), 0.0]
            })
            .collect();
        let mut direct = vec![false; 12];
        direct[11] = true;
        stage.configure_buses(&positions, &direct, 1);
        let mut lcg = 1u32;
        let bus: Vec<f32> = (0..12 * BRIR_BLOCK)
            .map(|_| {
                lcg = lcg.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                (lcg >> 8) as f32 / (1 << 24) as f32 - 0.5
            })
            .collect();
        for _ in 0..20 {
            stage.process_one_block_for_timing(&bus, 12, HeadPose::identity());
        }
        let mut worst = std::time::Duration::ZERO;
        let mut total = std::time::Duration::ZERO;
        let runs = 200;
        for _ in 0..runs {
            let t = std::time::Instant::now();
            stage.process_one_block_for_timing(&bus, 12, HeadPose::identity());
            let d = t.elapsed();
            total += d;
            worst = worst.max(d);
        }
        let partitions = stage.plan.partitions_for(set.max_taps());
        println!(
            "BRIR block burst: 11 buses × {partitions} partitions of {BRIR_BLOCK} — mean {:.3} ms, worst {:.3} ms per block ({:.3} ms of audio)",
            total.as_secs_f64() * 1e3 / runs as f64,
            worst.as_secs_f64() * 1e3,
            BRIR_BLOCK as f64 / 48.0
        );
    }

    #[cfg(not(feature = "sofa"))]
    #[test]
    fn a_load_without_sofa_support_reports_the_error() {
        let seen = Arc::new(std::sync::Mutex::new(Vec::<BrirStatus>::new()));
        let sink_seen = Arc::clone(&seen);
        let mut stage = BrirStage::with_status_sink(
            48000,
            Arc::new(move |s| sink_seen.lock().unwrap().push(s)),
        );
        stage.ensure_loaded("/nonexistent.sofa", &BrirLoadOptions::default(), 3);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while seen.lock().unwrap().is_empty() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let s = seen.lock().unwrap()[0].clone();
        assert_eq!(s.path, "/nonexistent.sofa");
        assert!(s.loaded.is_none());
        assert!(s.error.as_deref().unwrap_or("").contains("sofa"), "{s:?}");
        assert!(!stage.is_ready());
    }
}
