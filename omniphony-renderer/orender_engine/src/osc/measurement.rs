//! Control-thread measurement sessions, owned by one local TCP connection.
use super::peer::Peer;
use renderer::live_params::{OutputMode, RendererControl};
use renderer::measurement::{Configuration, Signal};
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
}

pub(super) struct Service {
    control: Arc<RendererControl>,
    instance_id: String,
    revision: u64,
    sequence: u64,
    signature: String,
    configuration: Option<Configuration>,
    session: Option<Session>,
    ended_reason: &'static str,
}

impl Service {
    pub fn new(control: Arc<RendererControl>, instance_epoch: i32) -> Self {
        Self {
            control,
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
            ended_reason: "no active measurement session",
        }
    }

    pub fn active(&self) -> bool {
        self.session.is_some()
    }

    fn snapshot(&mut self, host_generation: u64) -> Snapshot {
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
                Output {
                    id: speaker.name.clone(),
                    index: index as u32,
                    name: speaker.name.clone(),
                    gain_db: gain_db(gain),
                    delay_ms,
                    muted: state.is_some_and(|s| s.muted),
                    freq_low_hz: speaker.freq_low,
                    freq_high_hz: speaker.freq_high,
                }
            })
            .collect();
        if outputs.is_empty() {
            reason = Some("no physical outputs are available");
        }
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
            host_generation,
            reason,
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
            unavailable_reason: reason.map(str::to_owned),
        }
    }

    pub fn poll(&mut self, now: Instant, host_generation: u64) {
        if self.session.is_none() {
            return;
        }
        let snapshot = self.snapshot(host_generation);
        let session = self.session.as_ref().expect("checked");
        if let Some((_, signal)) = &session.signal {
            signal.poll(now);
        }
        let reason = if session.owner.is_closed() {
            Some("measurement owner disconnected")
        } else if !self.control.measurement.active(now) {
            Some("measurement session lease expired")
        } else if snapshot.revision != session.snapshot.revision {
            Some("renderer configuration changed during measurement")
        } else {
            None
        };
        if let Some(reason) = reason {
            self.close(reason);
        }
    }

    fn close(&mut self, reason: &'static str) {
        self.control.measurement.end();
        self.session = None;
        self.ended_reason = reason;
    }

    pub fn handle(
        &mut self,
        request: Request,
        owner: &Peer,
        now: Instant,
        host_generation: u64,
    ) -> Response {
        self.poll(now, host_generation);
        let request_id = request.request_id;
        let reply = if request.version != VERSION {
            error(
                "unsupported_version",
                "unsupported measurement protocol version",
            )
        } else if request_id == 0 {
            error("invalid_request", "request_id must be positive")
        } else if !owner.is_stream() || !owner.is_loopback() {
            error(
                "transport_required",
                "measurement requires the local TCP control connection",
            )
        } else {
            self.command(request, owner, now, host_generation)
        };
        Response {
            version: VERSION,
            request_id,
            reply,
        }
    }

    fn command(
        &mut self,
        request: Request,
        owner: &Peer,
        now: Instant,
        host_generation: u64,
    ) -> Reply {
        if let Command::Discover {} = request.command {
            return Reply::Snapshot {
                snapshot: self.snapshot(host_generation),
            };
        }
        if let Command::Begin { expected_revision } = &request.command {
            if self.session.is_some() {
                return error("busy", "another measurement session owns the renderer");
            }
            let snapshot = self.snapshot(host_generation);
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
            });
            self.control
                .measurement
                .renew(now, Duration::from_millis(LEASE_MS));
            return self.session_reply();
        }
        let Some(session) = self.session.as_mut() else {
            return error("session_ended", self.ended_reason);
        };
        let session_id = match &request.command {
            Command::Start { session_id, .. }
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
                signal,
                ..
            } => {
                if signal_id <= session.last_signal {
                    return error(
                        "stale_signal",
                        "signal IDs must increase; a completed signal cannot be replayed",
                    );
                }
                if session.signal.as_ref().is_some_and(|(_, s)| {
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
                let prepared = match Signal::prepare_referenced(
                    signal,
                    output.index as usize,
                    reference,
                    session.configuration,
                    now,
                ) {
                    Ok(prepared) => prepared,
                    Err(reason) => return error("invalid_signal", reason),
                };
                session.last_signal = signal_id;
                let progress = Progress {
                    signal_id,
                    output_id,
                    reference_output_id,
                    signal,
                    state: SignalState::Queued,
                    rendered_frames: 0,
                    total_frames: prepared.total_frames(),
                };
                self.control.measurement.install(prepared.clone());
                session.signal = Some((progress, prepared));
            }
            Command::Stop { .. } => {
                if let Some((_, signal)) = &session.signal {
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
