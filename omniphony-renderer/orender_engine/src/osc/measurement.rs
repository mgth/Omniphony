//! Control-thread measurement sessions, owned by one local TCP connection.
use super::peer::Peer;
use renderer::live_params::{OutputMode, RendererControl};
use renderer::measurement::{Configuration, Signal};
use runtime_control::HostControlHandler;
use runtime_control::osc_contract::measurement::*;
use std::sync::{Arc, atomic::Ordering};
use std::time::{Duration, Instant};

struct Session {
    id: String,
    owner: Peer,
    snapshot: Snapshot,
    configuration: Configuration,
    last_request: u64,
    last_signal: u64,
    signal: Option<(Progress, Arc<Signal>)>,
    noise: Option<(NoiseProgress, Arc<Signal>)>,
}

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
        }
    }

    pub fn active(&self) -> bool {
        self.session.is_some()
    }

    fn snapshot(&mut self) -> Snapshot {
        let control = &self.control;
        let layout = control.active_topology();
        let live = control.live.read();
        let sample_rate = control.sample_rate.load(Ordering::Acquire);
        let mut reason = None;
        if live.binaural.output_mode != OutputMode::SpeakerArray {
            reason = Some("measurement requires speaker output mode");
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
        // epochs for changes made outside this OSC listener.
        let signature = serde_json::to_string(&(
            &layout.speaker_layout,
            &outputs,
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
        self.control.measurement.end();
        self.session = None;
        self.ended_reason = reason.to_owned();
    }

    pub fn handle(&mut self, request: Request, owner: &Peer, now: Instant) -> Response {
        self.poll(now);
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

    fn command(&mut self, request: Request, owner: &Peer, now: Instant) -> Reply {
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
        if let Command::Begin { expected_revision } = &request.command {
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
            self.session = Some(Session {
                id: format!("{}:session:{}", self.instance_id, self.sequence),
                owner: owner.clone(),
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
            return self.session_reply();
        }
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
                if signal.start_hz < output.freq_low_hz.unwrap_or(20.0)
                    || signal.end_hz > output.freq_high_hz.unwrap_or(f32::MAX)
                {
                    return error(
                        "invalid_signal",
                        "sweep must stay inside the output's declared frequency range",
                    );
                }
                if signal.peak_dbfs + output.gain_db + session.snapshot.master_gain_db > -6.0 {
                    return error(
                        "level_limit",
                        "existing gains would exceed the -6 dBFS measurement output ceiling",
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
                let prepared = if sequence_outputs.is_empty() {
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
                            || signal.start_hz < target.freq_low_hz.unwrap_or(20.0)
                            || signal.end_hz > target.freq_high_hz.unwrap_or(f32::MAX)
                            || signal.peak_dbfs + target.gain_db + session.snapshot.master_gain_db
                                > -6.0
                        {
                            return error(
                                "invalid_signal",
                                "sequence output is muted or exceeds frequency/level limits",
                            );
                        }
                        indices.push(target.index as usize);
                    }
                    Signal::prepare_sequence(
                        signal,
                        &indices,
                        reference,
                        session.configuration,
                        now,
                    )
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
                if signal.peak_dbfs + output.gain_db + session.snapshot.master_gain_db > -6.0 {
                    return error(
                        "level_limit",
                        "existing gains exceed the measurement output ceiling",
                    );
                }
                let prepared = match Signal::prepare_noise(
                    signal,
                    output.index as usize,
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
