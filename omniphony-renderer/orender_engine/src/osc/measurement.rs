//! Control-thread measurement sessions, owned by one local TCP connection.
use super::peer::Peer;
use renderer::live_params::{OutputMode, RendererControl};
use renderer::measurement::{Configuration, Route, Signal};
use renderer::output_correction::{FilterSamples, FilterSource};
use runtime_control::HostControlHandler;
use runtime_control::osc_contract::measurement::*;
use std::sync::{Arc, atomic::Ordering};
use std::time::{Duration, Instant};

/// See [`Service::noise_output`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct NoiseOutput {
    pub index: u32,
    pub gain_bypassed: bool,
}

/// The `/state/measurement/noise` arguments: the output index, `-1` when no
/// noise plays, then whether its gain is skipped.
pub(super) fn noise_args(noise: Option<NoiseOutput>) -> Vec<rosc::OscType> {
    vec![
        rosc::OscType::Int(noise.map_or(-1, |n| n.index as i32)),
        rosc::OscType::Int(noise.is_some_and(|n| n.gain_bypassed) as i32),
    ]
}

/// The noise state to one client that registers or asks for its state again:
/// sent even when none plays, so a client that missed the end learns it.
pub(super) fn send_noise_to(
    socket: &std::net::UdpSocket,
    client: &Peer,
    noise: Option<NoiseOutput>,
) {
    super::transport::send_message_to_client(
        socket,
        client,
        runtime_control::osc_contract::STATE_MEASUREMENT_NOISE,
        noise_args(noise),
    );
}

struct Session {
    id: String,
    owner: Peer,
    /// The version of the request that began it: from [`FILTERS_VERSION`],
    /// a sweep waits for the filters to settle.
    version: u32,
    snapshot: Snapshot,
    configuration: Configuration,
    last_request: u64,
    last_signal: u64,
    signal: Option<(Progress, Arc<Signal>)>,
    noise: Option<(NoiseProgress, Arc<Signal>)>,
}

/// A filter arriving in [`Command::UploadFilter`] chunks.
struct Upload {
    id: String,
    sample_rate: u32,
    total_taps: usize,
    received: usize,
    taps: Vec<f32>,
    /// The checked filter, once every tap arrived.
    complete: Option<Arc<FilterSamples>>,
    touched: Instant,
}

impl Upload {
    fn state(&self) -> FilterUpload {
        FilterUpload {
            upload_id: self.id.clone(),
            sample_rate: self.sample_rate,
            total_taps: self.total_taps as u32,
            received_taps: self.received as u32,
            hash: self
                .complete
                .as_ref()
                .map(|filter| format!("{:016x}", filter.hash)),
        }
    }
}

/// Uploads kept at once: complete ones waiting for `set_output_filters`, and
/// those still arriving.
const MAX_UPLOADS: usize = 64;
/// Taps kept across every upload (64 MiB of `f32`).
const MAX_UPLOAD_TAPS: usize = 16 << 20;
/// An upload no request touched for this long is dropped.
const UPLOAD_TTL: Duration = Duration::from_secs(120);

pub(super) struct Service {
    control: Arc<RendererControl>,
    host: Option<Arc<dyn HostControlHandler>>,
    instance_id: String,
    revision: u64,
    sequence: u64,
    signature: String,
    configuration: Option<Configuration>,
    session: Option<Session>,
    ended_reason: String,
    uploads: Vec<Upload>,
    upload_sequence: u64,
}

impl Service {
    pub fn new(
        control: Arc<RendererControl>,
        instance_epoch: i32,
        host: Option<Arc<dyn HostControlHandler>>,
    ) -> Self {
        Self {
            control,
            host,
            instance_id: format!(
                "{instance_epoch}:{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ),
            revision: 0,
            sequence: 0,
            signature: String::new(),
            configuration: None,
            session: None,
            ended_reason: "no active measurement session".into(),
            uploads: Vec::new(),
            upload_sequence: 0,
        }
    }

    pub fn active(&self) -> bool {
        self.session.is_some()
    }

    /// The output level noise is queued or rendering on, and whether it skips
    /// that output's gain. Clients are told on
    /// [`runtime_control::osc_contract::STATE_MEASUREMENT_NOISE`].
    pub fn noise_output(&self) -> Option<NoiseOutput> {
        let session = self.session.as_ref()?;
        let (progress, signal) = session.noise.as_ref()?;
        if !matches!(signal.state(), SignalState::Queued | SignalState::Rendering) {
            return None;
        }
        let output = session
            .snapshot
            .outputs
            .iter()
            .find(|o| o.id == progress.output_id)?;
        Some(NoiseOutput {
            index: output.index,
            gain_bypassed: progress.signal.bypass_output_gain,
        })
    }

    fn snapshot(&mut self) -> Snapshot {
        let control = &self.control;
        let layout = control.active_topology();
        let live = control.live.read();
        let sample_rate = control.sample_rate.load(Ordering::Acquire);
        let mut reason = None;
        if live.binaural.output_mode != OutputMode::SpeakerArray {
            reason = Some(SPEAKER_OUTPUT_REQUIRED);
        } else if control.recomputing.load(Ordering::Acquire) || control.render_layout_outdated() {
            reason = Some("wait for the speaker layout rebuild to finish");
        } else if live.speaker_test.is_some() || live.object_test.is_some() {
            reason = Some("stop the existing speaker or object test first");
        } else if !(8_000..=192_000).contains(&sample_rate) {
            reason = Some("unsupported measurement sample rate");
        } else if !live.master_gain.is_finite() || live.master_gain <= 0.0 {
            reason = Some("master gain must be finite and positive");
        }
        let mut names = std::collections::HashSet::new();
        let radius_m = layout.speaker_layout.radius_m;
        let correction = self.control.output_correction_status();
        let correction_current = correction.generation == live.output_filters.generation;
        let outputs: Vec<_> = layout
            .speaker_layout
            .speakers
            .iter()
            .enumerate()
            .map(|(index, speaker)| {
                let state = live.speakers.get(&index);
                let gain = state.map_or_else(
                    || renderer::live_params::speaker_gain_linear(speaker.gain_db),
                    |s| s.gain,
                );
                let delay_ms = state.map_or(speaker.delay_ms, |s| s.delay_ms);
                if speaker.name.is_empty()
                    || speaker.name.len() > 128
                    || !names.insert(speaker.name.clone())
                {
                    reason = Some("output names must be unique, nonempty and at most 128 bytes");
                }
                if !gain.is_finite()
                    || gain <= 0.0
                    || !delay_ms.is_finite()
                    || !(0.0..=MAX_DELAY_MS).contains(&delay_ms)
                {
                    reason = Some("output gain or delay is outside the measurement limits");
                }
                if speaker.freq_low.is_some_and(|v| !v.is_finite() || v < 0.0)
                    || speaker.freq_high.is_some_and(|v| !v.is_finite() || v <= 0.0)
                    || matches!((speaker.freq_low, speaker.freq_high), (Some(lo), Some(hi)) if lo >= hi)
                { reason = Some("invalid output frequency range"); }
                let position = position(&layout.room, radius_m, speaker);
                let filter = live.output_filters.source(index).map(|source| {
                    output_filter(
                        source,
                        correction_current
                            .then(|| correction.outputs.get(index).and_then(Option::as_ref))
                            .flatten(),
                    )
                });
                Output {
                    id: speaker.name.clone(),
                    index: index as u32,
                    name: speaker.name.clone(),
                    gain_db: gain_db(gain),
                    delay_ms,
                    muted: state.is_some_and(|s| s.muted),
                    freq_low_hz: speaker.freq_low,
                    freq_high_hz: speaker.freq_high,
                    azimuth_deg: position.map(|p| p.0),
                    elevation_deg: position.map(|p| p.1),
                    distance_m: position.map(|p| p.2),
                    filter,
                }
            })
            .collect();
        if outputs.is_empty() {
            reason = Some("no physical outputs are available");
        }
        let stage_width = control.speaker_stage_width();
        let host = self.host.as_ref().map(|host| host.measurement_state());
        let reason = if stage_width != outputs.len() {
            Some(format!(
                "speaker layout has {} outputs but the renderer was opened with {stage_width}; \
                 save the configuration in Studio and restart the renderer before measuring",
                outputs.len()
            ))
        } else {
            reason.map(str::to_owned).or_else(|| {
                host.as_ref()
                    .and_then(|host| host.unavailable_reason.clone())
            })
        };
        let master_gain_db = gain_db(live.master_gain);
        let configuration = Configuration {
            sample_rate,
            width: outputs.len(),
            master_gain_bits: live.master_gain.to_bits(),
            speaker_generation: control.speaker_params_generation.load(Ordering::Acquire),
            geometry_generation: control.geometry_generation.load(Ordering::Acquire),
        };
        self.configuration = Some(configuration);
        // All fields affecting this direct-output signal, plus conservative
        // epochs for changes made outside this OSC listener. The filters by
        // their set's generation, not their load state: a filter that
        // finishes loading is the same configuration.
        let filterless: Vec<Output> = outputs
            .iter()
            .map(|output| Output {
                filter: None,
                ..output.clone()
            })
            .collect();
        let signature = serde_json::to_string(&(
            &layout.speaker_layout,
            &filterless,
            live.output_filters.generation,
            live.master_gain.to_bits(),
            sample_rate,
            live.binaural.output_mode.as_str(),
            configuration.speaker_generation,
            configuration.geometry_generation,
            control.options_epoch.load(Ordering::Acquire),
            host.as_ref().map(|host| &host.configuration),
            &reason,
        ))
        .unwrap_or_default();
        if signature != self.signature {
            self.signature = signature;
            self.revision = self.revision.wrapping_add(1);
        }
        Snapshot {
            instance_id: self.instance_id.clone(),
            revision: format!("{}:{}", self.instance_id, self.revision),
            sample_rate,
            master_gain_db,
            max_delay_ms: MAX_DELAY_MS,
            outputs,
            unavailable_reason: reason,
            output_filters_bypassed: self.control.measurement.filters_bypass_requested(),
        }
    }

    pub fn poll(&mut self, now: Instant) {
        if self.session.is_none() {
            return;
        }
        let snapshot = self.snapshot();
        let session = self.session.as_ref().expect("checked");
        if let Some(signal) = session
            .signal
            .as_ref()
            .map(|(_, s)| s)
            .or_else(|| session.noise.as_ref().map(|(_, s)| s))
        {
            signal.poll(now);
        }
        let reason = if session.owner.is_closed() {
            Some("measurement owner disconnected")
        } else if !self.control.measurement.active(now) {
            Some("measurement session lease expired")
        } else if let Some(reason) = snapshot.unavailable_reason.as_deref() {
            Some(reason)
        } else if snapshot.revision != session.snapshot.revision {
            Some("renderer configuration changed during measurement")
        } else {
            None
        };
        if let Some(reason) = reason {
            self.close(reason);
        }
    }

    fn close(&mut self, reason: &str) {
        let bypassed = self.control.measurement.filters_bypass_requested();
        self.control.measurement.end();
        self.session = None;
        self.ended_reason = reason.to_owned();
        if bypassed {
            // The filters are back: clients showing the bypass are told.
            self.control.bump_live_state();
        }
    }

    /// Whether every output plays through the filters a sweep would now go
    /// through alone: the set asked for (or none) loaded, the bypass on or
    /// off as the session asked, and no tail segment still convolving with
    /// what came before (`OutputCorrectionStage::set_settled_flag`).
    fn filters_settled(&self) -> bool {
        let live = self.control.live.read();
        if !live.output_filters.any() {
            return true;
        }
        let enabled =
            live.options.output_correction && !self.control.measurement.filters_bypass_requested();
        self.control
            .output_correction_settled
            .load(Ordering::Acquire)
            == renderer::output_correction::settled_key(live.output_filters.generation, enabled)
    }

    pub fn handle(&mut self, request: Request, owner: &Peer, now: Instant) -> Response {
        self.poll(now);
        self.uploads
            .retain(|upload| now.saturating_duration_since(upload.touched) < UPLOAD_TTL);
        let request_id = request.request_id;
        let version = reply_version(request.version);
        let reply = if !supported(request.version) {
            error(
                "unsupported_version",
                &format!(
                    "unsupported measurement protocol version; this renderer answers \
                     versions {MIN_VERSION} to {VERSION}"
                ),
            )
        } else if request_id == 0 {
            error("invalid_request", "request_id must be positive")
        } else if !owner.is_stream() || !owner.is_loopback() {
            error(
                "transport_required",
                "measurement requires the local TCP control connection",
            )
        } else {
            self.command(request, owner, now)
        };
        Response::for_version(version, request_id, reply)
    }

    fn set_outputs(&mut self, expected_revision: &str, settings: &[OutputSetting]) -> Reply {
        if self.session.is_some() {
            return error(
                "busy",
                "end the measurement session before changing settings",
            );
        }
        // Serialize with every live-parameter writer, including Studio and auto gain.
        // Readers continue to see the complete previous state until publication.
        let control = self.control.clone();
        let mut live = control.live.write();
        let snapshot = self.snapshot();
        if snapshot.revision != expected_revision {
            return error(
                "stale_snapshot",
                "renderer settings changed; discover again",
            );
        }
        if let Some(reason) = &snapshot.unavailable_reason {
            return error("unavailable", reason);
        }
        if settings.is_empty() || settings.len() > snapshot.outputs.len() {
            return error(
                "invalid_settings",
                "expected a nonempty subset of physical outputs",
            );
        }
        let mut indices = Vec::with_capacity(settings.len());
        for setting in settings {
            let Some(output) = snapshot.outputs.iter().find(|o| o.id == setting.output_id) else {
                return error("unknown_output", "output is absent from this configuration");
            };
            if indices.contains(&output.index)
                || !setting.gain_db.is_finite()
                || !(-60.0..=24.0).contains(&setting.gain_db)
                || !setting.delay_ms.is_finite()
                || !(0.0..=MAX_DELAY_MS).contains(&setting.delay_ms)
            {
                return error(
                    "invalid_settings",
                    "duplicate output or gain/delay outside limits",
                );
            }
            indices.push(output.index);
        }
        for (setting, index) in settings.iter().zip(indices) {
            let params = live.speakers.entry(index as usize).or_default();
            params.gain = renderer::live_params::speaker_gain_linear(setting.gain_db);
            params.delay_ms = setting.delay_ms;
        }
        drop(live);
        control.mark_speaker_params_dirty();
        control.config_dirty.store(true, Ordering::Release);
        control.bump_live_state();
        Reply::Snapshot {
            snapshot: self.snapshot(),
        }
    }

    /// Store one chunk of a filter ([`Command::UploadFilter`]). A chunk that
    /// is refused leaves its upload as it was, to be sent again.
    fn upload_filter(
        &mut self,
        upload_id: Option<String>,
        sample_rate: u32,
        total_taps: u32,
        offset: u32,
        taps: &str,
        now: Instant,
    ) -> Reply {
        let engine_rate = self.control.sample_rate.load(Ordering::Acquire);
        let chunk = match decode_taps(taps) {
            Ok(chunk) => chunk,
            Err(reason) => return error("invalid_filter", reason),
        };
        if chunk.is_empty() || chunk.len() > MAX_UPLOAD_CHUNK_TAPS {
            return error(
                "invalid_filter",
                &format!("a chunk carries 1 to {MAX_UPLOAD_CHUNK_TAPS} taps"),
            );
        }
        if chunk.iter().any(|t| !t.is_finite()) {
            return error("invalid_filter", "the filter holds non-finite samples");
        }
        let total = total_taps as usize;
        let index = match upload_id {
            None => {
                if offset != 0 {
                    return error("invalid_filter", "a new upload starts at offset 0");
                }
                if sample_rate != engine_rate && !FILTER_SOURCE_RATES.contains(&sample_rate) {
                    return error(
                        "invalid_filter",
                        &format!(
                            "a filter is designed at the renderer's rate ({engine_rate} Hz) \
                             or between {} and {} Hz",
                            FILTER_SOURCE_RATES.start(),
                            FILTER_SOURCE_RATES.end()
                        ),
                    );
                }
                if total == 0 || total > renderer::output_correction::max_source_taps(sample_rate) {
                    return error(
                        "invalid_filter",
                        &format!("a filter is 1 tap to {MAX_FILTER_SECONDS} s long"),
                    );
                }
                let held: usize = self.uploads.iter().map(|u| u.total_taps).sum();
                if self.uploads.len() >= MAX_UPLOADS || held + total > MAX_UPLOAD_TAPS {
                    return error(
                        "upload_limit",
                        "too many filters uploaded and not applied; apply them or wait \
                         for the unused ones to expire",
                    );
                }
                self.upload_sequence += 1;
                self.uploads.push(Upload {
                    id: format!("{}:filter:{}", self.instance_id, self.upload_sequence),
                    sample_rate,
                    total_taps: total,
                    received: 0,
                    taps: Vec::with_capacity(total),
                    complete: None,
                    touched: now,
                });
                self.uploads.len() - 1
            }
            Some(id) => {
                let Some(index) = self.uploads.iter().position(|u| u.id == id) else {
                    return error(
                        "unknown_upload",
                        "no such upload; it may have expired or been applied",
                    );
                };
                let upload = &self.uploads[index];
                if upload.sample_rate != sample_rate || upload.total_taps != total {
                    return error(
                        "invalid_filter",
                        "every chunk repeats its upload's sample rate and length",
                    );
                }
                if upload.complete.is_some() {
                    return error("invalid_filter", "the upload is already complete");
                }
                if offset as usize != upload.received {
                    return error(
                        "invalid_filter",
                        &format!(
                            "chunks arrive in order: the next one starts at {}",
                            upload.received
                        ),
                    );
                }
                index
            }
        };
        let upload = &mut self.uploads[index];
        if upload.received + chunk.len() > upload.total_taps {
            return error("invalid_filter", "the chunk runs past the filter's length");
        }
        upload.taps.extend_from_slice(&chunk);
        upload.received += chunk.len();
        upload.touched = now;
        if upload.received == upload.total_taps {
            let taps = std::mem::take(&mut upload.taps);
            match FilterSamples::new(upload.sample_rate, taps, engine_rate) {
                Ok(filter) => upload.complete = Some(Arc::new(filter)),
                Err(reason) => {
                    self.uploads.remove(index);
                    return error("invalid_filter", &reason);
                }
            }
        }
        Reply::FilterUpload {
            upload: self.uploads[index].state(),
        }
    }

    /// Replace some outputs' filters ([`Command::SetOutputFilters`]): checked
    /// against the snapshot revision under the live-state writer lock, all
    /// published together, live only. An uploaded filter becomes the
    /// output's inline filter (over a file its speaker names, until a Save
    /// writes it); `null` leaves the output unfiltered, a file included.
    fn set_output_filters(
        &mut self,
        expected_revision: &str,
        settings: &[OutputFilterSetting],
    ) -> Reply {
        if self.session.is_some() {
            return error(
                "busy",
                "end the measurement session before changing filters",
            );
        }
        // Read before the live lock, which never nests inside the layout's.
        let layout = self.control.editable_layout();
        let engine_rate = self.control.sample_rate.load(Ordering::Acquire);
        let control = self.control.clone();
        let mut live = control.live.write();
        let snapshot = self.snapshot();
        if snapshot.revision != expected_revision {
            return error(
                "stale_snapshot",
                "renderer settings changed; discover again",
            );
        }
        if let Some(reason) = &snapshot.unavailable_reason {
            return error("unavailable", reason);
        }
        if settings.is_empty() || settings.len() > snapshot.outputs.len() {
            return error(
                "invalid_settings",
                "expected a nonempty subset of physical outputs",
            );
        }
        let mut chosen: Vec<(String, Option<Arc<FilterSamples>>)> =
            Vec::with_capacity(settings.len());
        for setting in settings {
            if !snapshot.outputs.iter().any(|o| o.id == setting.output_id) {
                return error("unknown_output", "output is absent from this configuration");
            }
            if chosen.iter().any(|(id, _)| *id == setting.output_id) {
                return error("invalid_settings", "duplicate output");
            }
            let filter = match &setting.filter {
                None => None,
                Some(id) => {
                    let Some(upload) = self.uploads.iter().find(|u| u.id == *id) else {
                        return error(
                            "unknown_upload",
                            "no such upload; it may have expired or been applied",
                        );
                    };
                    let Some(filter) = &upload.complete else {
                        return error("incomplete_upload", "the upload is still missing taps");
                    };
                    if filter.sample_rate != engine_rate
                        && !FILTER_SOURCE_RATES.contains(&filter.sample_rate)
                    {
                        return error(
                            "invalid_filter",
                            "the filter's rate cannot play at the renderer's",
                        );
                    }
                    Some(Arc::clone(filter))
                }
            };
            chosen.push((setting.output_id.clone(), filter));
        }
        let mut inline = (*live.inline_filters).clone();
        let mut next_layout = layout;
        let mut changed = false;
        for (name, filter) in &chosen {
            match filter {
                Some(filter) => {
                    changed |= inline.get(name) != Some(filter);
                    inline.insert(name.clone(), Arc::clone(filter));
                }
                None => {
                    changed |= inline.remove(name).is_some();
                    for speaker in &mut next_layout.speakers {
                        if speaker.name == *name && speaker.correction_filter.take().is_some() {
                            changed = true;
                        }
                    }
                }
            }
        }
        if changed {
            let next =
                renderer::output_correction::OutputFilters::for_layout(&next_layout, &inline);
            live.inline_filters = Arc::new(inline);
            if *live.output_filters.sources != *next.sources {
                live.output_filters = next;
            }
        }
        drop(live);
        if changed {
            control.with_editable_layout(|layout| {
                for (speaker, next) in layout.speakers.iter_mut().zip(&next_layout.speakers) {
                    if speaker.name == next.name {
                        speaker.correction_filter = next.correction_filter.clone();
                    }
                }
            });
            control.config_dirty.store(true, Ordering::Release);
            control.bump_live_state();
        }
        let applied: Vec<&String> = settings.iter().filter_map(|s| s.filter.as_ref()).collect();
        self.uploads.retain(|u| !applied.contains(&&u.id));
        Reply::Snapshot {
            snapshot: self.snapshot(),
        }
    }

    /// Switch the live output from headphones to the speakers, as the Studio's
    /// output mode does: through the option registry, then a live-state bump for
    /// the other clients. The listener rebuilds the layout if a BRIR set was in use.
    fn use_speaker_output(&mut self) -> Reply {
        if self.session.is_some() {
            return error("busy", "a measurement session owns the renderer");
        }
        let Some(spec) = renderer::options::find("output_mode") else {
            return error("unavailable", "the renderer has no output mode option");
        };
        let raw = renderer::options::RawOptionValue::Str("speaker");
        match renderer::options::apply_to_control(&self.control, spec, &raw) {
            Some(applied) => {
                if applied.changed {
                    self.control.bump_live_state();
                }
                Reply::Snapshot {
                    snapshot: self.snapshot(),
                }
            }
            None => error("invalid_request", "the renderer refused speaker output"),
        }
    }

    fn command(&mut self, request: Request, owner: &Peer, now: Instant) -> Reply {
        let needs_filters_version = match &request.command {
            Command::UploadFilter { .. } | Command::SetOutputFilters { .. } => true,
            Command::Begin {
                bypass_output_filters,
                ..
            } => *bypass_output_filters,
            Command::Start { bass_managed, .. } => *bass_managed,
            _ => false,
        };
        if needs_filters_version && request.version < FILTERS_VERSION {
            return error(
                "invalid_request",
                &format!("this command needs measurement protocol version {FILTERS_VERSION}"),
            );
        }
        if let Command::UploadFilter {
            upload_id,
            sample_rate,
            total_taps,
            offset,
            taps,
        } = request.command
        {
            return self.upload_filter(upload_id, sample_rate, total_taps, offset, &taps, now);
        }
        if let Command::SetOutputFilters {
            expected_revision,
            outputs,
        } = &request.command
        {
            return self.set_output_filters(expected_revision, outputs);
        }
        if let Command::SetOutputs {
            expected_revision,
            settings,
        } = &request.command
        {
            return self.set_outputs(expected_revision, settings);
        }
        if let Command::Discover {} = request.command {
            return Reply::Snapshot {
                snapshot: self.snapshot(),
            };
        }
        if let Command::UseSpeakerOutput {} = request.command {
            return self.use_speaker_output();
        }
        if let Command::Begin {
            expected_revision,
            bypass_output_filters,
        } = &request.command
        {
            if self.session.is_some() {
                return error("busy", "another measurement session owns the renderer");
            }
            let snapshot = self.snapshot();
            if snapshot.revision != *expected_revision {
                return error(
                    "stale_snapshot",
                    "discover the current renderer configuration before beginning",
                );
            }
            if let Some(reason) = &snapshot.unavailable_reason {
                return error("unavailable", reason);
            }
            self.sequence += 1;
            let mut snapshot = snapshot;
            snapshot.output_filters_bypassed = *bypass_output_filters;
            self.session = Some(Session {
                id: format!("{}:session:{}", self.instance_id, self.sequence),
                owner: owner.clone(),
                version: request.version,
                snapshot,
                configuration: self.configuration.expect("snapshot captured configuration"),
                last_request: request.request_id,
                last_signal: 0,
                signal: None,
                noise: None,
            });
            self.control
                .measurement
                .renew(now, Duration::from_millis(LEASE_MS));
            if *bypass_output_filters {
                self.control.measurement.bypass_filters(true);
                // Clients showing the filters are told they are held off.
                self.control.bump_live_state();
            }
            return self.session_reply();
        }
        let filters_settled = self.filters_settled();
        let Some(session) = self.session.as_mut() else {
            return error("session_ended", &self.ended_reason);
        };
        let session_id = match &request.command {
            Command::Start { session_id, .. }
            | Command::StartNoise { session_id, .. }
            | Command::Status { session_id }
            | Command::Stop { session_id }
            | Command::End { session_id } => session_id,
            _ => unreachable!(),
        };
        if session.owner != *owner || session.id != *session_id {
            return error("not_owner", "this connection does not own that session");
        }
        if request.request_id <= session.last_request {
            return error(
                "stale_request",
                "request IDs must increase within a session",
            );
        }
        session.last_request = request.request_id;
        match request.command {
            Command::Start {
                signal_id,
                output_id,
                reference_output_id,
                sequence_outputs,
                signal,
                bass_managed,
                ..
            } => {
                if signal_id <= session.last_signal {
                    return error(
                        "stale_signal",
                        "signal IDs must increase; a completed signal cannot be replayed",
                    );
                }
                if session.noise.as_ref().is_some_and(|(_, s)| {
                    matches!(s.state(), SignalState::Queued | SignalState::Rendering)
                }) || session.signal.as_ref().is_some_and(|(_, s)| {
                    matches!(s.state(), SignalState::Queued | SignalState::Rendering)
                }) {
                    return error("signal_busy", "stop or finish the running signal first");
                }
                let Some(output) = session.snapshot.outputs.iter().find(|o| o.id == output_id)
                else {
                    return error(
                        "unknown_output",
                        "output is absent from the session snapshot",
                    );
                };
                if output.muted {
                    return error(
                        "muted_output",
                        "unmute the output before beginning the session",
                    );
                }
                if let Err(reason) = signal.validate(session.snapshot.sample_rate) {
                    return error("invalid_signal", reason);
                }
                // A bass-managed sweep may start below the output's own
                // band: the crossover gives that part to the subwoofers.
                let low_limit = |o: &Output| {
                    if bass_managed {
                        20.0
                    } else {
                        o.freq_low_hz.unwrap_or(20.0)
                    }
                };
                // Its sweep may reach any output through the panning and the
                // crossover (each band at most at the sweep's peak), so it is
                // held under the ceiling on the loudest unmuted one.
                let loudest_db = session
                    .snapshot
                    .outputs
                    .iter()
                    .filter(|o| !o.muted)
                    .map(|o| o.gain_db)
                    .fold(f32::NEG_INFINITY, f32::max);
                let target_gain_db = |o: &Output| {
                    if bass_managed {
                        loudest_db.max(o.gain_db)
                    } else {
                        o.gain_db
                    }
                };
                if signal.start_hz < low_limit(output)
                    || signal.end_hz > output.freq_high_hz.unwrap_or(f32::MAX)
                {
                    return error(
                        "invalid_signal",
                        "sweep must stay inside the output's declared frequency range",
                    );
                }
                if signal.peak_dbfs + target_gain_db(output) + session.snapshot.master_gain_db
                    > -6.0
                {
                    return error(
                        "level_limit",
                        "existing gains would exceed the -6 dBFS measurement output ceiling",
                    );
                }
                if session.version >= FILTERS_VERSION && !filters_settled {
                    return error(
                        "filters_settling",
                        "the output filters are still loading or switching; start again \
                         in a moment",
                    );
                }
                let reference = if let Some(id) = &reference_output_id {
                    let Some(reference) = session.snapshot.outputs.iter().find(|o| &o.id == id)
                    else {
                        return error(
                            "unknown_output",
                            "reference output is absent from the snapshot",
                        );
                    };
                    if reference.muted {
                        return error("muted_output", "reference output is muted");
                    }
                    if signal.start_hz < reference.freq_low_hz.unwrap_or(20.0)
                        || signal.end_hz > reference.freq_high_hz.unwrap_or(f32::MAX)
                    {
                        return error(
                            "invalid_signal",
                            "sweep exceeds the reference output frequency range",
                        );
                    }
                    if signal.peak_dbfs + reference.gain_db + session.snapshot.master_gain_db > -6.0
                    {
                        return error(
                            "level_limit",
                            "reference output would exceed the measurement ceiling",
                        );
                    }
                    Some(reference.index as usize)
                } else {
                    None
                };
                let topology = self.control.active_topology();
                let place = |index: u32| {
                    let speaker = &topology.speaker_layout.speakers[index as usize];
                    Route::Source([speaker.x as f64, speaker.y as f64, speaker.z as f64])
                };
                let prepared = if bass_managed && sequence_outputs.is_empty() {
                    let routes = match reference {
                        Some(reference) => vec![
                            Route::Direct(reference),
                            place(output.index),
                            Route::Direct(reference),
                        ],
                        None => vec![place(output.index)],
                    };
                    Signal::prepare_routes(
                        signal,
                        output.index as usize,
                        &routes,
                        session.configuration,
                        now,
                    )
                } else if sequence_outputs.is_empty() {
                    Signal::prepare_referenced(
                        signal,
                        output.index as usize,
                        reference,
                        session.configuration,
                        now,
                    )
                } else {
                    let Some(reference) = reference else {
                        return error("invalid_signal", "a sequence requires a reference output");
                    };
                    if sequence_outputs.first() != Some(&output_id) {
                        return error(
                            "invalid_signal",
                            "sequence first output must match output_id",
                        );
                    }
                    if let Err(reason) = omniphony_osc_contract::measurement::sequence_frames(
                        signal,
                        session.snapshot.sample_rate,
                        sequence_outputs.len(),
                    ) {
                        return error("invalid_signal", reason);
                    }
                    let mut indices = Vec::with_capacity(sequence_outputs.len());
                    for id in &sequence_outputs {
                        let Some(target) = session.snapshot.outputs.iter().find(|o| &o.id == id)
                        else {
                            return error(
                                "unknown_output",
                                "sequence output is absent from snapshot",
                            );
                        };
                        if indices.contains(&(target.index as usize)) {
                            return error("invalid_signal", "sequence outputs must be unique");
                        }
                        if target.muted
                            || signal.start_hz < low_limit(target)
                            || signal.end_hz > target.freq_high_hz.unwrap_or(f32::MAX)
                            || signal.peak_dbfs
                                + target_gain_db(target)
                                + session.snapshot.master_gain_db
                                > -6.0
                        {
                            return error(
                                "invalid_signal",
                                "sequence output is muted or exceeds frequency/level limits",
                            );
                        }
                        indices.push(target.index as usize);
                    }
                    if bass_managed {
                        let mut routes = Vec::with_capacity(2 * indices.len() + 1);
                        routes.push(Route::Direct(reference));
                        for &index in &indices {
                            routes.push(place(index as u32));
                            routes.push(Route::Direct(reference));
                        }
                        Signal::prepare_routes(
                            signal,
                            indices[0],
                            &routes,
                            session.configuration,
                            now,
                        )
                    } else {
                        Signal::prepare_sequence(
                            signal,
                            &indices,
                            reference,
                            session.configuration,
                            now,
                        )
                    }
                };
                let prepared = match prepared {
                    Ok(prepared) => prepared,
                    Err(reason) => return error("invalid_signal", reason),
                };
                session.last_signal = signal_id;
                let progress = Progress {
                    signal_id,
                    output_id,
                    reference_output_id,
                    sequence_outputs,
                    signal,
                    bass_managed,
                    state: SignalState::Queued,
                    rendered_frames: 0,
                    total_frames: prepared.total_frames(),
                };
                self.control.measurement.install(prepared.clone());
                session.noise = None;
                session.signal = Some((progress, prepared));
            }
            Command::StartNoise {
                signal_id,
                output_id,
                signal,
                ..
            } => {
                if signal_id <= session.last_signal {
                    return error("stale_signal", "signal IDs must increase");
                }
                let active = session
                    .signal
                    .as_ref()
                    .map(|(_, s)| s)
                    .or_else(|| session.noise.as_ref().map(|(_, s)| s));
                if active.is_some_and(|s| {
                    matches!(s.state(), SignalState::Queued | SignalState::Rendering)
                }) {
                    return error("signal_busy", "stop or finish the running signal first");
                }
                let Some(output) = session.snapshot.outputs.iter().find(|o| o.id == output_id)
                else {
                    return error(
                        "unknown_output",
                        "output is absent from the session snapshot",
                    );
                };
                if output.muted {
                    return error("muted_output", "output is muted");
                }
                if let Err(reason) = signal.validate(session.snapshot.sample_rate) {
                    return error("invalid_signal", reason);
                }
                if LevelNoise::LOW_HZ < output.freq_low_hz.unwrap_or(20.0)
                    || LevelNoise::HIGH_HZ > output.freq_high_hz.unwrap_or(f32::MAX)
                {
                    return error(
                        "invalid_signal",
                        "manual level noise needs an output covering 500–2000 Hz",
                    );
                }
                // Without its output gain, the noise is divided by that gain here
                // and multiplied back by it at the output: only the master applies.
                let applied_gain_db = if signal.bypass_output_gain {
                    if output.gain_db < MIN_BYPASS_GAIN_DB {
                        return error(
                            "level_limit",
                            "output gain is too low to play the noise without it",
                        );
                    }
                    0.0
                } else {
                    output.gain_db
                };
                if signal.peak_dbfs + applied_gain_db + session.snapshot.master_gain_db > -6.0 {
                    return error(
                        "level_limit",
                        "existing gains exceed the measurement output ceiling",
                    );
                }
                let prepared = match Signal::prepare_noise(
                    signal,
                    output.index as usize,
                    applied_gain_db - output.gain_db,
                    session.configuration,
                    now,
                ) {
                    Ok(s) => s,
                    Err(reason) => return error("invalid_signal", reason),
                };
                let progress = NoiseProgress {
                    signal_id,
                    output_id,
                    signal,
                    state: SignalState::Queued,
                    rendered_frames: 0,
                    total_frames: prepared.total_frames(),
                };
                session.last_signal = signal_id;
                session.signal = None;
                self.control.measurement.install(prepared.clone());
                session.noise = Some((progress, prepared));
            }
            Command::Stop { .. } => {
                if let Some(signal) = session
                    .signal
                    .as_ref()
                    .map(|(_, s)| s)
                    .or_else(|| session.noise.as_ref().map(|(_, s)| s))
                {
                    signal.cancel();
                }
            }
            Command::End { .. } => {
                self.close("measurement session ended");
                return Reply::Ended {};
            }
            Command::Status { .. } => {}
            _ => unreachable!(),
        }
        self.control
            .measurement
            .renew(now, Duration::from_millis(LEASE_MS));
        self.session_reply()
    }

    fn session_reply(&self) -> Reply {
        let session = self.session.as_ref().expect("active session");
        if let Some((progress, signal)) = &session.noise {
            let mut progress = progress.clone();
            progress.state = signal.state();
            progress.rendered_frames = signal.rendered_frames();
            return Reply::NoiseSession {
                session_id: session.id.clone(),
                snapshot: session.snapshot.clone(),
                lease_ms: LEASE_MS,
                signal: progress,
            };
        }
        let signal = session.signal.as_ref().map(|(progress, signal)| {
            let mut progress = progress.clone();
            progress.state = signal.state();
            progress.rendered_frames = signal.rendered_frames();
            progress
        });
        Reply::Session {
            session_id: session.id.clone(),
            snapshot: session.snapshot.clone(),
            lease_ms: LEASE_MS,
            signal,
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.control.measurement.end();
    }
}

/// The layout position in metres, as Studio shows it: the normalized
/// coordinates through the topology's room warp, at the layout's room scale.
/// `None` when the declared geometry is not finite.
fn position(
    room: &renderer::live_params::RoomRatios,
    radius_m: f32,
    speaker: &renderer::speaker_layout::Speaker,
) -> Option<(f32, f32, f32)> {
    let [x, y, z] = room.scale([speaker.x, speaker.y, speaker.z]);
    let (azimuth, elevation, distance) = omniphony_geometry::f32::to_spherical(x, y, z);
    let distance = distance * radius_m;
    (azimuth.is_finite() && elevation.is_finite() && distance.is_finite())
        .then_some((azimuth, elevation, distance))
}

/// One output's filter as a snapshot reports it: a live filter's taps are
/// known at once, a file's once the stage has read it (`status`, for the
/// set the live state names, `None` while it is being prepared).
fn output_filter(
    source: &FilterSource,
    status: Option<&renderer::output_correction::FilterStatus>,
) -> OutputFilter {
    let mut filter = match source {
        FilterSource::File(path) => OutputFilter {
            origin: FilterOrigin::File,
            file: Some(path.to_string()),
            state: FilterLoadState::Pending,
            sample_rate: None,
            taps: None,
            hash: None,
            error: None,
        },
        FilterSource::Samples(samples) => OutputFilter {
            origin: FilterOrigin::Live,
            file: None,
            state: FilterLoadState::Pending,
            sample_rate: Some(samples.sample_rate),
            taps: Some(samples.taps.len() as u32),
            hash: Some(format!("{:016x}", samples.hash)),
            error: None,
        },
    };
    match status {
        None => {}
        Some(status) => match &status.error {
            Some(reason) => {
                filter.state = FilterLoadState::Error;
                filter.error = Some(reason.clone());
            }
            None => {
                filter.state = FilterLoadState::Loaded;
                filter.sample_rate = Some(status.source_rate);
                filter.taps = Some(status.source_taps as u32);
                filter.hash = Some(format!("{:016x}", status.hash));
            }
        },
    }
    filter
}

/// Below this output gain, noise without it would need more than 60 dB of
/// compensation: the lowest gain `set_outputs` accepts.
const MIN_BYPASS_GAIN_DB: f32 = -60.0;

fn gain_db(gain: f32) -> f32 {
    if gain.is_finite() && gain > 0.0 {
        20.0 * gain.log10()
    } else {
        -120.0
    }
}

pub(super) fn error(code: &str, message: &str) -> Reply {
    Reply::Error {
        code: code.into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests;
