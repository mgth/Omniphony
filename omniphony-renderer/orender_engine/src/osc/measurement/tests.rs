use super::super::peer::StreamPeer;
use super::*;
use renderer::test_support::fixture_control;
use std::net::{TcpListener, TcpStream};

#[derive(Default)]
struct TestHost {
    status: std::sync::atomic::AtomicU64,
    configuration: std::sync::atomic::AtomicU64,
}

impl HostControlHandler for TestHost {
    fn handle(
        &self,
        _: &str,
        _: &rosc::OscMessage,
    ) -> Option<runtime_control::osc::ControlEffects> {
        None
    }
    fn extend_snapshot(&self) -> Vec<rosc::OscPacket> {
        Vec::new()
    }
    fn amend_saved_config(&self, _: &mut renderer::config::RenderConfig) {}
    fn state_generation(&self) -> u64 {
        self.status.load(Ordering::Relaxed)
    }
    fn measurement_state(&self) -> runtime_control::host_control::MeasurementHostState {
        runtime_control::host_control::MeasurementHostState {
            configuration: self.configuration.load(Ordering::Relaxed).into(),
            unavailable_reason: None,
        }
    }
}

#[test]
fn host_status_updates_keep_the_session_but_settings_changes_end_it() {
    let control = fixture_control();
    let host = Arc::new(TestHost::default());
    let mut service = Service::new(control.clone(), 42, Some(host.clone()));
    let (owner, _client) = peer();
    let now = Instant::now();
    let (session_id, before) = begin(&mut service, &owner, now);
    host.status.fetch_add(1, Ordering::Relaxed);
    assert_eq!(service.snapshot().revision, before.revision);
    assert!(matches!(
        command(
            &mut service,
            &owner,
            2,
            Command::Status {
                session_id: session_id.clone(),
            },
            now
        ),
        Reply::Session { .. }
    ));
    host.configuration.fetch_add(1, Ordering::Relaxed);
    assert_error(
        command(&mut service, &owner, 3, Command::Status { session_id }, now),
        "session_ended",
    );
    assert!(!control.measurement.active(now));
}

fn peer() -> (Peer, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let (server, address) = listener.accept().unwrap();
    (Peer::Tcp(StreamPeer::new(server, address)), client)
}
fn command(service: &mut Service, peer: &Peer, id: u64, command: Command, now: Instant) -> Reply {
    service
        .handle(
            Request {
                version: VERSION,
                request_id: id,
                command,
            },
            peer,
            now,
        )
        .reply
}
fn begin(service: &mut Service, peer: &Peer, now: Instant) -> (String, Snapshot) {
    let snapshot = service.snapshot();
    let reply = command(
        service,
        peer,
        1,
        Command::Begin {
            expected_revision: snapshot.revision.clone(),
            bypass_output_filters: false,
        },
        now,
    );
    let Reply::Session { session_id, .. } = reply else {
        panic!("{reply:?}")
    };
    (session_id, snapshot)
}
fn sweep(session_id: &str, snapshot: &Snapshot, id: u64) -> Command {
    Command::Start {
        bass_managed: false,
        sequence_outputs: Vec::new(),
        reference_output_id: None,
        session_id: session_id.into(),
        signal_id: id,
        output_id: snapshot.outputs[0].id.clone(),
        signal: Sweep {
            duration_ms: 100,
            peak_dbfs: -30.0,
            start_hz: 200.0,
            end_hz: 10_000.0,
        },
    }
}

#[test]
fn sequence_acknowledges_order_and_rejects_duplicate_or_unsafe_later_outputs() {
    for case in 0..4 {
        let mut service = Service::new(fixture_control(), 42, None);
        let (owner, _client) = peer();
        let now = Instant::now();
        let (id, snapshot) = begin(&mut service, &owner, now);
        let mut request = sweep(&id, &snapshot, 1);
        if let Command::Start {
            bass_managed: false,
            reference_output_id,
            sequence_outputs,
            ..
        } = &mut request
        {
            *reference_output_id = Some(snapshot.outputs[0].id.clone());
            *sequence_outputs = vec![
                snapshot.outputs[0].id.clone(),
                snapshot.outputs[1].id.clone(),
            ];
            if case == 1 {
                sequence_outputs[1] = sequence_outputs[0].clone();
            }
        }
        if case == 2 {
            service.session.as_mut().unwrap().snapshot.outputs[1].muted = true;
        }
        if case == 3 {
            service.session.as_mut().unwrap().snapshot.outputs[1].gain_db = 40.0;
        }
        let reply = command(&mut service, &owner, 2, request, now);
        if case == 0 {
            let Reply::Session {
                signal: Some(p), ..
            } = reply
            else {
                panic!("{reply:?}")
            };
            assert_eq!(p.sequence_outputs.len(), 2);
            assert_eq!(p.total_frames, p.signal.frames(snapshot.sample_rate) * 5);
        } else {
            assert_error(reply, "invalid_signal");
            assert!(service.session.as_ref().unwrap().signal.is_none());
        }
    }
}
fn assert_error(reply: Reply, expected: &str) {
    assert!(
        matches!(&reply, Reply::Error { code, .. } if code == expected),
        "{reply:?}"
    );
}

#[test]
fn sessions_preserve_settings_and_refuse_stale_snapshots() {
    let control = fixture_control();
    let mut service = Service::new(control.clone(), 42, None);
    let (peer, _client) = peer();
    let now = Instant::now();
    let before = service.snapshot();
    control.live.write().master_gain = 0.5;
    assert_error(
        command(
            &mut service,
            &peer,
            1,
            Command::Begin {
                expected_revision: before.revision,
                bypass_output_filters: false,
            },
            now,
        ),
        "stale_snapshot",
    );
    let (id, snapshot) = begin(&mut service, &peer, now);
    assert!(control.measurement.active(now));
    assert!(matches!(
        command(&mut service, &peer, 2, sweep(&id, &snapshot, 1), now),
        Reply::Session { .. }
    ));
    assert!(matches!(
        command(
            &mut service,
            &peer,
            3,
            Command::Stop {
                session_id: id.clone()
            },
            now
        ),
        Reply::Session {
            signal: Some(Progress {
                state: SignalState::Cancelled,
                ..
            }),
            ..
        }
    ));
    assert!(matches!(
        command(&mut service, &peer, 4, Command::End { session_id: id }, now),
        Reply::Ended {}
    ));
    assert!(!control.measurement.active(now));
    assert_eq!(control.live.read().master_gain, 0.5);
    assert!(!control.config_dirty.load(Ordering::Acquire));
}

#[test]
fn measurement_rejects_layouts_that_do_not_match_the_running_output() {
    for width in [0, 10, 14] {
        let control = fixture_control();
        control.set_speaker_stage_width(width);
        let mut service = Service::new(control, 42, None);
        let (owner, _client) = peer();
        let snapshot = service.snapshot();
        assert_eq!(snapshot.outputs.len(), 12);
        let reason = snapshot.unavailable_reason.unwrap();
        assert!(reason.contains(&format!("opened with {width}")), "{reason}");
        assert!(reason.contains("restart"), "{reason}");
        assert_error(
            command(
                &mut service,
                &owner,
                1,
                Command::Begin {
                    expected_revision: snapshot.revision,
                    bypass_output_filters: false,
                },
                Instant::now(),
            ),
            "unavailable",
        );
        assert!(!service.active());
    }
}

#[test]
fn changed_output_width_ends_the_session_with_an_actionable_reason() {
    let control = fixture_control();
    let mut service = Service::new(control.clone(), 42, None);
    let (owner, _client) = peer();
    let now = Instant::now();
    let (session_id, _) = begin(&mut service, &owner, now);
    control.set_speaker_stage_width(10);
    let reply = command(&mut service, &owner, 2, Command::Status { session_id }, now);
    assert!(matches!(reply, Reply::Error { code, message }
        if code == "session_ended" && message.contains("opened with 10")));
    assert!(!control.measurement.active(now));
}

#[test]
fn owner_and_request_ids_prevent_replayed_or_overlapping_signals() {
    let control = fixture_control();
    let mut service = Service::new(control.clone(), 42, None);
    let (owner, _client) = peer();
    let (other, _other_client) = peer();
    let now = Instant::now();
    let (id, snapshot) = begin(&mut service, &owner, now);
    assert_error(
        command(
            &mut service,
            &other,
            2,
            Command::Stop {
                session_id: id.clone(),
            },
            now,
        ),
        "not_owner",
    );
    assert_error(
        command(
            &mut service,
            &other,
            2,
            Command::Begin {
                expected_revision: snapshot.revision.clone(),
                bypass_output_filters: false,
            },
            now,
        ),
        "busy",
    );
    command(&mut service, &owner, 2, sweep(&id, &snapshot, 1), now);
    assert_error(
        command(&mut service, &owner, 2, sweep(&id, &snapshot, 1), now),
        "stale_request",
    );
    assert_error(
        command(&mut service, &owner, 3, sweep(&id, &snapshot, 2), now),
        "signal_busy",
    );
    command(
        &mut service,
        &owner,
        4,
        Command::Stop {
            session_id: id.clone(),
        },
        now,
    );
    assert_error(
        command(&mut service, &owner, 5, sweep(&id, &snapshot, 1), now),
        "stale_signal",
    );
    assert!(matches!(
        command(&mut service, &owner, 6, sweep(&id, &snapshot, 2), now),
        Reply::Session { .. }
    ));
}

#[test]
fn disconnect_timeout_and_external_configuration_change_release_ownership() {
    for case in 0..3 {
        let control = fixture_control();
        let mut service = Service::new(control.clone(), 42, None);
        let (owner, _client) = peer();
        let now = Instant::now();
        let (id, snapshot) = begin(&mut service, &owner, now);
        command(&mut service, &owner, 2, sweep(&id, &snapshot, 1), now);
        let later = match case {
            0 => {
                if let Peer::Tcp(p) = &owner {
                    p.close();
                }
                now
            }
            1 => now + Duration::from_millis(LEASE_MS + 1),
            _ => {
                control.live.write().master_gain = 0.5;
                now
            }
        };
        service.poll(later);
        assert!(!service.active());
        assert!(!control.measurement.active(later));
        assert_error(
            command(
                &mut service,
                &owner,
                3,
                Command::Status { session_id: id },
                later,
            ),
            "session_ended",
        );
    }
}

#[test]
fn a_host_that_does_not_render_cannot_report_completion() {
    let control = fixture_control();
    let mut service = Service::new(control, 42, None);
    let (owner, _client) = peer();
    let now = Instant::now();
    let (id, snapshot) = begin(&mut service, &owner, now);
    command(&mut service, &owner, 2, sweep(&id, &snapshot, 1), now);
    let reply = command(
        &mut service,
        &owner,
        3,
        Command::Status { session_id: id },
        now + Duration::from_millis(2700),
    );
    assert!(
        matches!(
            reply,
            Reply::Session {
                signal: Some(Progress {
                    state: SignalState::Expired,
                    rendered_frames: 0,
                    ..
                }),
                ..
            }
        ),
        "{reply:?}"
    );
}

#[test]
fn invalid_signal_levels_do_not_emit_audio() {
    let control = fixture_control();
    let mut service = Service::new(control, 42, None);
    let (owner, _client) = peer();
    let now = Instant::now();
    let (id, snapshot) = begin(&mut service, &owner, now);
    for (i, peak) in [f32::NAN, f32::INFINITY, 0.0, -80.0]
        .into_iter()
        .enumerate()
    {
        let mut request = sweep(&id, &snapshot, 1);
        if let Command::Start { signal, .. } = &mut request {
            signal.peak_dbfs = peak;
        }
        assert_error(
            command(&mut service, &owner, 2 + i as u64, request, now),
            "invalid_signal",
        );
    }
    assert!(service.session.as_ref().unwrap().signal.is_none());
}

#[test]
fn protocol_is_strict_and_udp_cannot_open_a_session() {
    assert!(
        serde_json::from_str::<Request>(
            r#"{"version":1,"request_id":1,"command":{"type":"discover","ignored":true}}"#
        )
        .is_err()
    );
    let mut service = Service::new(fixture_control(), 42, None);
    let udp = Peer::Udp("127.0.0.1:12345".parse().unwrap());
    assert_error(
        command(&mut service, &udp, 1, Command::Discover {}, Instant::now()),
        "transport_required",
    );
}

#[test]
fn referenced_start_validates_the_reference_and_reports_three_segments() {
    for (case, expected) in [
        (0, "unknown_output"),
        (1, "muted_output"),
        (2, "level_limit"),
        (3, "invalid_signal"),
    ] {
        let control = fixture_control();
        let mut service = Service::new(control.clone(), 42, None);
        let (owner, _client) = peer();
        let now = Instant::now();
        let (id, snapshot) = begin(&mut service, &owner, now);
        // The captured session is the validation boundary. Mutate it directly
        // to exercise each reference guard without triggering stale-state exit.
        let reference_id = snapshot.outputs[1].id.clone();
        let reference = &mut service.session.as_mut().unwrap().snapshot.outputs[1];
        match case {
            1 => reference.muted = true,
            2 => reference.gain_db = 30.0,
            3 => reference.freq_high_hz = Some(1000.0),
            _ => {}
        }
        let mut request = sweep(&id, &snapshot, 1);
        if let Command::Start {
            bass_managed: false,
            reference_output_id,
            ..
        } = &mut request
        {
            *reference_output_id = Some(if case == 0 {
                "missing".into()
            } else {
                reference_id
            });
        }
        assert_error(command(&mut service, &owner, 2, request, now), expected);
        assert!(service.session.as_ref().unwrap().signal.is_none());
    }
    let mut service = Service::new(fixture_control(), 42, None);
    let (owner, _client) = peer();
    let now = Instant::now();
    let (id, snapshot) = begin(&mut service, &owner, now);
    let mut request = sweep(&id, &snapshot, 1);
    if let Command::Start {
        bass_managed: false,
        reference_output_id,
        ..
    } = &mut request
    {
        *reference_output_id = Some(snapshot.outputs[1].id.clone());
    }
    let reply = command(&mut service, &owner, 2, request, now);
    assert!(matches!(
        reply,
        Reply::Session {
            signal: Some(Progress {
                total_frames: 86400,
                reference_output_id: Some(_),
                ..
            }),
            ..
        }
    ));
}

#[test]
fn profile_settings_are_absolute_atomic_and_revision_guarded() {
    let control = fixture_control();
    let mut service = Service::new(control.clone(), 42, None);
    let (owner, _client) = peer();
    let now = Instant::now();
    let before = service.snapshot();
    let settings: Vec<_> = before
        .outputs
        .iter()
        .take(2)
        .enumerate()
        .map(|(i, o)| OutputSetting {
            output_id: o.id.clone(),
            gain_db: -3.0 - i as f32,
            delay_ms: 160.25 + i as f32,
        })
        .collect();
    let request = Command::SetOutputs {
        expected_revision: before.revision.clone(),
        settings: settings.clone(),
    };
    let mut invalid = settings.clone();
    invalid[1].delay_ms = MAX_DELAY_MS + 1.0;
    assert_error(
        command(
            &mut service,
            &owner,
            1,
            Command::SetOutputs {
                expected_revision: before.revision.clone(),
                settings: invalid,
            },
            now,
        ),
        "invalid_settings",
    );
    assert_eq!(service.snapshot(), before, "no partial write");
    let Reply::Snapshot { snapshot: after } =
        command(&mut service, &owner, 2, request.clone(), now)
    else {
        panic!()
    };
    for (output, setting) in after.outputs.iter().zip(&settings) {
        assert!((output.gain_db - setting.gain_db).abs() < 0.0001);
        assert_eq!(output.delay_ms, setting.delay_ms);
    }
    assert_ne!(after.revision, before.revision);
    assert_error(
        command(&mut service, &owner, 3, request, now),
        "stale_snapshot",
    );
    let Reply::Snapshot { snapshot: again } = command(
        &mut service,
        &owner,
        4,
        Command::SetOutputs {
            expected_revision: after.revision,
            settings,
        },
        now,
    ) else {
        panic!()
    };
    assert_eq!(again.outputs, after.outputs, "reapply is not additive");
    let (_, active) = begin(&mut service, &owner, now);
    assert_error(
        command(
            &mut service,
            &owner,
            5,
            Command::SetOutputs {
                expected_revision: active.revision,
                settings: vec![OutputSetting {
                    output_id: active.outputs[0].id.clone(),
                    gain_db: 0.0,
                    delay_ms: 0.0,
                }],
            },
            now,
        ),
        "busy",
    );
}

#[test]
fn finite_level_noise_is_leased_exclusive_and_preserves_digital_settings() {
    for stop_case in 0..4 {
        let control = fixture_control();
        let mut service = Service::new(control.clone(), 42, None);
        let (owner, _client) = peer();
        let (other, _other_client) = peer();
        let now = Instant::now();
        let (id, snapshot) = begin(&mut service, &owner, now);
        let signal = LevelNoise {
            duration_seconds: 60,
            peak_dbfs: -30.0,
            bypass_output_gain: false,
        };
        let start = Command::StartNoise {
            session_id: id.clone(),
            signal_id: 1,
            output_id: snapshot.outputs[0].id.clone(),
            signal,
        };
        assert_error(
            command(&mut service, &other, 2, start.clone(), now),
            "not_owner",
        );
        let reply = command(&mut service, &owner, 2, start, now);
        assert!(
            matches!(reply, Reply::NoiseSession { signal: NoiseProgress { total_frames, .. }, .. } if total_frames == signal.frames(snapshot.sample_rate))
        );
        assert_error(
            command(&mut service, &owner, 3, sweep(&id, &snapshot, 2), now),
            "signal_busy",
        );
        let prepared = service
            .session
            .as_ref()
            .unwrap()
            .noise
            .as_ref()
            .unwrap()
            .1
            .clone();
        if stop_case == 0 {
            assert!(matches!(
                command(
                    &mut service,
                    &owner,
                    4,
                    Command::Stop {
                        session_id: id.clone()
                    },
                    now
                ),
                Reply::NoiseSession {
                    signal: NoiseProgress {
                        state: SignalState::Cancelled,
                        ..
                    },
                    ..
                }
            ));
            assert_eq!(service.snapshot(), snapshot);
            assert!(!control.config_dirty.load(Ordering::Acquire));
        } else if stop_case == 1 {
            service.poll(now + Duration::from_secs(4));
            assert!(!control.measurement.active(now + Duration::from_secs(4)));
        } else if stop_case == 2 {
            control.live.write().master_gain = 0.5;
            service.poll(now);
            assert!(!control.measurement.active(now));
        } else {
            if let Peer::Tcp(stream) = &owner {
                stream.close();
            }
            service.poll(now);
            assert!(!control.measurement.active(now));
        }
        assert!(matches!(
            prepared.state(),
            SignalState::Cancelled | SignalState::Expired
        ));
    }
}

#[test]
fn level_noise_rejects_invalid_duration_level_and_output_band_before_installing() {
    for case in 0..5 {
        let mut service = Service::new(fixture_control(), 42, None);
        let (owner, _client) = peer();
        let now = Instant::now();
        let (id, snapshot) = begin(&mut service, &owner, now);
        let session = service.session.as_mut().unwrap();
        let mut signal = LevelNoise {
            duration_seconds: 60,
            peak_dbfs: -30.0,
            bypass_output_gain: false,
        };
        match case {
            0 => signal.duration_seconds = 61,
            1 => signal.peak_dbfs = -5.0,
            2 => session.snapshot.outputs[0].freq_high_hz = Some(200.0),
            3 => session.snapshot.outputs[0].muted = true,
            _ => session.snapshot.outputs[0].gain_db = 30.0,
        }
        assert!(matches!(
            command(
                &mut service,
                &owner,
                2,
                Command::StartNoise {
                    session_id: id,
                    signal_id: 1,
                    output_id: snapshot.outputs[0].id.clone(),
                    signal
                },
                now
            ),
            Reply::Error { .. }
        ));
        assert!(service.session.as_ref().unwrap().noise.is_none());
    }
}

#[test]
fn level_noise_can_skip_the_output_gain_but_keeps_the_master() {
    // Without the flag the wire form stays the one version-4/5 renderers accept.
    let plain = LevelNoise {
        duration_seconds: 60,
        peak_dbfs: -30.0,
        bypass_output_gain: false,
    };
    assert!(!serde_json::to_string(&plain).unwrap().contains("bypass"));
    let bypass = LevelNoise {
        bypass_output_gain: true,
        ..plain
    };
    // An output gain of +27 dB puts the noise over the ceiling, unless skipped;
    // -70 dB cannot be compensated.
    for (gain_db, signal, started) in [
        (27.0, plain, false),
        (27.0, bypass, true),
        (-10.0, bypass, true),
        (-70.0, bypass, false),
    ] {
        let control = fixture_control();
        let mut service = Service::new(control.clone(), 42, None);
        let (owner, _client) = peer();
        let now = Instant::now();
        let (id, snapshot) = begin(&mut service, &owner, now);
        let session = service.session.as_mut().unwrap();
        session.snapshot.outputs[0].gain_db = gain_db;
        let configuration = session.configuration;
        let reply = command(
            &mut service,
            &owner,
            2,
            Command::StartNoise {
                session_id: id,
                signal_id: 1,
                output_id: snapshot.outputs[0].id.clone(),
                signal,
            },
            now,
        );
        if !started {
            assert!(matches!(reply, Reply::Error { .. }), "{gain_db} dB");
            continue;
        }
        assert!(matches!(reply, Reply::NoiseSession { .. }), "{gain_db} dB");
        // The prepared noise is divided by the output gain the output stage
        // multiplies it by: its peak leaves the output at the requested level.
        let width = configuration.width;
        let mut out = vec![0.0_f32; width * configuration.sample_rate as usize];
        assert!(control.measurement.render(&mut out, configuration, now));
        let peak = out
            .chunks(width)
            .map(|frame| frame[snapshot.outputs[0].index as usize].abs())
            .fold(0.0_f32, f32::max);
        let at_output_db = 20.0 * peak.log10() + gain_db;
        assert!(
            (at_output_db - signal.peak_dbfs).abs() < 0.01,
            "{at_output_db}"
        );
    }
}

#[test]
fn clients_are_told_which_output_plays_noise_and_whether_its_gain_is_skipped() {
    assert_eq!(
        noise_args(None),
        vec![rosc::OscType::Int(-1), rosc::OscType::Int(0)]
    );
    for bypass_output_gain in [false, true] {
        let mut service = Service::new(fixture_control(), 42, None);
        let (owner, _client) = peer();
        let now = Instant::now();
        assert_eq!(service.noise_output(), None);
        let (id, snapshot) = begin(&mut service, &owner, now);
        assert_eq!(service.noise_output(), None);
        let output = &snapshot.outputs[1];
        let reply = command(
            &mut service,
            &owner,
            2,
            Command::StartNoise {
                session_id: id.clone(),
                signal_id: 1,
                output_id: output.id.clone(),
                signal: LevelNoise {
                    duration_seconds: 60,
                    peak_dbfs: -30.0,
                    bypass_output_gain,
                },
            },
            now,
        );
        assert!(matches!(reply, Reply::NoiseSession { .. }));
        let noise = service.noise_output();
        assert_eq!(
            noise,
            Some(NoiseOutput {
                index: output.index,
                gain_bypassed: bypass_output_gain,
            })
        );
        assert_eq!(
            noise_args(noise),
            vec![
                rosc::OscType::Int(output.index as i32),
                rosc::OscType::Int(bypass_output_gain as i32),
            ]
        );
        command(
            &mut service,
            &owner,
            3,
            Command::Stop { session_id: id },
            now,
        );
        assert_eq!(service.noise_output(), None);
    }
}

#[test]
fn headphone_output_is_named_and_can_be_switched_to_the_speakers() {
    let control = fixture_control();
    control.live.write().binaural.output_mode = renderer::live_params::OutputMode::Binaural;
    let mut service = Service::new(control.clone(), 42, None);
    let (owner, _client) = peer();
    let now = Instant::now();
    let Reply::Snapshot { snapshot } = command(&mut service, &owner, 1, Command::Discover {}, now)
    else {
        panic!("expected a snapshot")
    };
    assert_eq!(
        snapshot.unavailable_reason.as_deref(),
        Some(SPEAKER_OUTPUT_REQUIRED)
    );
    let generation = control.live_state_generation();
    let Reply::Snapshot { snapshot } =
        command(&mut service, &owner, 2, Command::UseSpeakerOutput {}, now)
    else {
        panic!("expected a snapshot")
    };
    assert_eq!(snapshot.unavailable_reason, None);
    assert_eq!(
        control.live.read().binaural.output_mode,
        renderer::live_params::OutputMode::SpeakerArray
    );
    // The other clients hear of it; the saved configuration is not written.
    assert!(control.live_state_generation() > generation);
    // Asking again changes nothing; a session refuses it.
    let generation = control.live_state_generation();
    assert!(matches!(
        command(&mut service, &owner, 3, Command::UseSpeakerOutput {}, now),
        Reply::Snapshot { .. }
    ));
    assert_eq!(control.live_state_generation(), generation);
    begin(&mut service, &owner, now);
    assert_error(
        command(&mut service, &owner, 4, Command::UseSpeakerOutput {}, now),
        "busy",
    );
}

fn request(service: &mut Service, peer: &Peer, version: u32, command: Command) -> Response {
    service.handle(
        Request {
            version,
            request_id: 1,
            command,
        },
        peer,
        Instant::now(),
    )
}

#[test]
fn discovery_places_each_output_where_the_layout_does() {
    let mut service = Service::new(fixture_control(), 42, None);
    let (owner, _client) = peer();
    let Reply::Snapshot { snapshot } =
        request(&mut service, &owner, VERSION, Command::Discover {}).reply
    else {
        panic!("expected a snapshot")
    };
    let position = |id: &str| {
        let output = snapshot.outputs.iter().find(|o| o.id == id).unwrap();
        (
            output.azimuth_deg.unwrap(),
            output.elevation_deg.unwrap(),
            output.distance_m.unwrap(),
        )
    };
    // The fixture's 7.1.4 corners in a unit room at a 1 m scale.
    let height = 0.5_f32.sqrt().atan().to_degrees();
    for (id, expected) in [
        ("C", (0.0, 0.0, 1.0)),
        ("FL", (-45.0, 0.0, 2_f32.sqrt())),
        ("SR", (90.0, 0.0, 1.0)),
        ("BL", (-135.0, 0.0, 2_f32.sqrt())),
        ("TFR", (45.0, height, 3_f32.sqrt())),
        ("LFE", (45.0, -height, 3_f32.sqrt())),
    ] {
        let actual = position(id);
        assert!(
            (actual.0 - expected.0).abs() < 1e-3
                && (actual.1 - expected.1).abs() < 1e-3
                && (actual.2 - expected.2).abs() < 1e-4,
            "{id}: {actual:?} != {expected:?}"
        );
    }
}

#[test]
fn positions_follow_the_room_warp_at_the_layout_scale() {
    let room = renderer::live_params::RoomRatios {
        ratio: [1.0, 2.0, 0.5],
        rear: 2.0,
        lower: 0.5,
        center_blend: 0.0,
    };
    let speaker = renderer::speaker_layout::Speaker::from_cartesian("X", 0.5, 0.5, 0.5, true, 0.0);
    // Warped to (0.5, 1.0, 0.25) units, then 2 m per unit.
    let (azimuth, elevation, distance) = position(&room, 2.0, &speaker).unwrap();
    assert!((azimuth - 0.5_f32.atan2(1.0).to_degrees()).abs() < 1e-3);
    assert!((elevation - 0.25_f32.atan2(1.25_f32.sqrt()).to_degrees()).abs() < 1e-3);
    assert!((distance - 2.0 * 1.3125_f32.sqrt()).abs() < 1e-4);
    let mut broken = speaker.clone();
    broken.x = f32::NAN;
    assert_eq!(position(&room, 2.0, &broken), None);
}

#[test]
fn version_4_clients_get_their_own_reply_shape_without_positions() {
    // A version-4 client refuses any output field outside this set.
    const VERSION_4_FIELDS: [&str; 8] = [
        "id",
        "index",
        "name",
        "gain_db",
        "delay_ms",
        "muted",
        "freq_low_hz",
        "freq_high_hz",
    ];
    let version_4_shaped = |response: &Response| {
        let json = serde_json::to_value(response).unwrap();
        let outputs = json["reply"]["snapshot"]["outputs"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(outputs.len(), 12);
        outputs.iter().all(|output| {
            output
                .as_object()
                .unwrap()
                .keys()
                .all(|key| VERSION_4_FIELDS.contains(&key.as_str()))
        })
    };
    let mut service = Service::new(fixture_control(), 42, None);
    let (owner, _client) = peer();

    let current = request(&mut service, &owner, VERSION, Command::Discover {});
    assert_eq!(current.version, 8);
    assert!(!version_4_shaped(&current), "version 5 adds fields");
    let five = request(&mut service, &owner, 5, Command::Discover {});
    assert_eq!(five.version, 5);
    assert!(!version_4_shaped(&five), "version 5 keeps its positions");

    let older = request(&mut service, &owner, 4, Command::Discover {});
    assert_eq!(older.version, 4);
    assert!(version_4_shaped(&older));
    let Reply::Snapshot { snapshot } = &older.reply else {
        panic!("{older:?}")
    };
    let revision = snapshot.revision.clone();
    // A session's snapshot takes the same shape.
    let session = request(
        &mut service,
        &owner,
        4,
        Command::Begin {
            expected_revision: revision,
            bypass_output_filters: false,
        },
    );
    assert!(
        matches!(session.reply, Reply::Session { .. }),
        "{session:?}"
    );
    assert!(version_4_shaped(&session));

    // A version-5 client still parses a version-4 reply: positions are absent.
    let parsed: Response = serde_json::from_str(&serde_json::to_string(&older).unwrap()).unwrap();
    let Reply::Snapshot { snapshot } = parsed.reply else {
        panic!("expected a snapshot")
    };
    assert!(
        snapshot.outputs.iter().all(|o| o.azimuth_deg.is_none()
            && o.elevation_deg.is_none()
            && o.distance_m.is_none())
    );
}

#[test]
fn versions_outside_4_to_8_are_refused_in_the_current_version() {
    let mut service = Service::new(fixture_control(), 42, None);
    let (owner, _client) = peer();
    for version in [3, 9] {
        let response = request(&mut service, &owner, version, Command::Discover {});
        assert_eq!(response.version, VERSION);
        assert_error(response.reply, "unsupported_version");
    }
}

// ── Protocol 8: room correction filters ──────────────────────────────────

/// Upload `taps` at `rate` in chunks of `chunk` taps; the upload's ID.
fn upload(
    service: &mut Service,
    peer: &Peer,
    first_id: u64,
    rate: u32,
    taps: &[f32],
    chunk: usize,
) -> FilterUpload {
    let mut upload_id = None;
    let mut last = None;
    for (i, part) in taps.chunks(chunk).enumerate() {
        let reply = command(
            service,
            peer,
            first_id + i as u64,
            Command::UploadFilter {
                upload_id: upload_id.clone(),
                sample_rate: rate,
                total_taps: taps.len() as u32,
                offset: (i * chunk) as u32,
                taps: encode_taps(part),
            },
            Instant::now(),
        );
        let Reply::FilterUpload { upload } = reply else {
            panic!("{reply:?}")
        };
        upload_id = Some(upload.upload_id.clone());
        last = Some(upload);
    }
    last.expect("one chunk at least")
}

fn decaying(len: usize) -> Vec<f32> {
    (0..len)
        .map(|i| (-(i as f32) / 900.0).exp() * if i % 3 == 0 { 0.6 } else { -0.2 })
        .collect()
}

#[test]
fn taps_travel_as_base64_of_little_endian_floats_and_hash_alike_on_both_ends() {
    let taps = [1.0f32, -0.5, 0.25, f32::MIN_POSITIVE, 3.0e-9];
    for n in 0..taps.len() {
        let text = encode_taps(&taps[..n]);
        assert_eq!(text.len() % 4, 0);
        assert_eq!(decode_taps(&text).unwrap(), taps[..n].to_vec());
    }
    assert_eq!(encode_taps(&[1.0]), "AACAPw==");
    for bad in ["AACAPw=", "AACA*w==", "AA==AACA", "AACAPwA="] {
        assert!(decode_taps(bad).is_err(), "{bad}");
    }
    // FNV-1a over the bytes: the empty filter is the offset basis.
    assert_eq!(taps_hash_hex(&[]), "cbf29ce484222325");
    let samples = FilterSamples::new(48_000, taps.to_vec(), 48_000).unwrap();
    assert_eq!(format!("{:016x}", samples.hash), taps_hash_hex(&taps));
    // The default 8192-tap filter fits one request.
    let request = Request {
        version: VERSION,
        request_id: 1,
        command: Command::UploadFilter {
            upload_id: Some("x".repeat(64)),
            sample_rate: 384_000,
            total_taps: 576_000,
            offset: 0,
            taps: encode_taps(&vec![-1.234_567e-12; MAX_UPLOAD_CHUNK_TAPS]),
        },
    };
    assert!(serde_json::to_string(&request).unwrap().len() <= MAX_PACKET_BYTES);
}

#[test]
fn version_8_requests_round_trip_and_older_clients_keep_their_shapes() {
    let commands = [
        Command::Begin {
            expected_revision: "r".into(),
            bypass_output_filters: true,
        },
        Command::Start {
            session_id: "s".into(),
            signal_id: 2,
            output_id: "L".into(),
            reference_output_id: Some("C".into()),
            sequence_outputs: vec!["L".into()],
            signal: Sweep {
                duration_ms: 500,
                peak_dbfs: -20.0,
                start_hz: 20.0,
                end_hz: 20_000.0,
            },
            bass_managed: true,
        },
        Command::UploadFilter {
            upload_id: None,
            sample_rate: 48_000,
            total_taps: 3,
            offset: 0,
            taps: encode_taps(&[1.0, 0.5, 0.25]),
        },
        Command::SetOutputFilters {
            expected_revision: "r".into(),
            outputs: vec![
                OutputFilterSetting {
                    output_id: "L".into(),
                    filter: Some("u".into()),
                },
                OutputFilterSetting {
                    output_id: "R".into(),
                    filter: None,
                },
            ],
        },
    ];
    for command in commands {
        let request = Request {
            version: VERSION,
            request_id: 7,
            command,
        };
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), request);
    }
    // A version-7 begin is exactly the old wire form.
    let begin = serde_json::to_value(Command::Begin {
        expected_revision: "r".into(),
        bypass_output_filters: false,
    })
    .unwrap();
    assert_eq!(
        begin,
        serde_json::json!({"type": "begin", "expected_revision": "r"})
    );

    // Replies: a version-8 snapshot carries the filters and the bypass, a
    // version-7 one has neither (its client refuses unknown fields).
    let control = fixture_control();
    let mut service = Service::new(control.clone(), 42, None);
    let (owner, _client) = peer();
    let upload = upload(&mut service, &owner, 1, 48_000, &decaying(100), 64);
    let before = service.snapshot();
    let reply = command(
        &mut service,
        &owner,
        10,
        Command::SetOutputFilters {
            expected_revision: before.revision,
            outputs: vec![OutputFilterSetting {
                output_id: before.outputs[0].id.clone(),
                filter: Some(upload.upload_id),
            }],
        },
        Instant::now(),
    );
    assert!(matches!(reply, Reply::Snapshot { .. }), "{reply:?}");
    let mut snapshot = service.snapshot();
    snapshot.output_filters_bypassed = true;
    let progress = Progress {
        signal_id: 1,
        output_id: "L".into(),
        reference_output_id: None,
        sequence_outputs: Vec::new(),
        signal: Sweep {
            duration_ms: 500,
            peak_dbfs: -20.0,
            start_hz: 20.0,
            end_hz: 20_000.0,
        },
        bass_managed: true,
        state: SignalState::Queued,
        rendered_frames: 0,
        total_frames: 1,
    };
    let session = |version| {
        serde_json::to_string(&Response::for_version(
            version,
            1,
            Reply::Session {
                session_id: "s".into(),
                snapshot: snapshot.clone(),
                lease_ms: LEASE_MS,
                signal: Some(progress.clone()),
            },
        ))
        .unwrap()
    };
    let eight = session(8);
    for field in [
        "\"filter\"",
        "output_filters_bypassed",
        "bass_managed",
        "\"live\"",
    ] {
        assert!(eight.contains(field), "{field}: {eight}");
    }
    let seven = session(7);
    for field in ["\"filter\"", "output_filters_bypassed", "bass_managed"] {
        assert!(!seven.contains(field), "{field}: {seven}");
    }
    let parsed: Response = serde_json::from_str(&eight).unwrap();
    let Reply::Session { snapshot: back, .. } = parsed.reply else {
        panic!()
    };
    assert_eq!(back, snapshot);

    // A version-7 request cannot use them.
    for command in [
        Command::UploadFilter {
            upload_id: None,
            sample_rate: 48_000,
            total_taps: 1,
            offset: 0,
            taps: encode_taps(&[1.0]),
        },
        Command::Begin {
            expected_revision: service.snapshot().revision,
            bypass_output_filters: true,
        },
    ] {
        let response = request(&mut service, &owner, 7, command);
        assert_eq!(response.version, 7);
        assert_error(response.reply, "invalid_request");
    }
    assert!(!control.measurement.armed(), "no session began");
}

#[test]
fn uploaded_filters_are_set_atomically_against_the_revision_and_reported() {
    use renderer::output_correction::FilterSource;
    let control = fixture_control();
    let mut service = Service::new(control.clone(), 42, None);
    let (owner, _client) = peer();
    let taps = decaying(20_000);
    let done = upload(
        &mut service,
        &owner,
        1,
        96_000,
        &taps,
        MAX_UPLOAD_CHUNK_TAPS,
    );
    assert_eq!(
        (done.received_taps, done.total_taps, done.hash.as_deref()),
        (20_000, 20_000, Some(taps_hash_hex(&taps).as_str()))
    );
    let other = upload(&mut service, &owner, 10, 48_000, &[0.5, 0.25], 2);
    // Speaker 1 names a file the command drops (`null`).
    control.with_editable_layout(|l| l.speakers[1].correction_filter = Some("/f/old.wav".into()));
    runtime_control::osc::refresh_output_filters(&control);

    let before = service.snapshot();
    let (first, second) = (before.outputs[0].id.clone(), before.outputs[1].id.clone());
    assert_eq!(
        before.outputs[1].filter.as_ref().map(|f| f.origin),
        Some(FilterOrigin::File)
    );
    let settings = vec![
        OutputFilterSetting {
            output_id: first.clone(),
            filter: Some(done.upload_id.clone()),
        },
        OutputFilterSetting {
            output_id: second.clone(),
            filter: None,
        },
    ];
    // Any bad entry refuses the whole command.
    for (bad, code) in [
        (
            OutputFilterSetting {
                output_id: "nowhere".into(),
                filter: None,
            },
            "unknown_output",
        ),
        (
            OutputFilterSetting {
                output_id: before.outputs[2].id.clone(),
                filter: Some("unknown".into()),
            },
            "unknown_upload",
        ),
        (settings[0].clone(), "invalid_settings"),
    ] {
        let mut outputs = settings.clone();
        outputs.push(bad);
        assert_error(
            command(
                &mut service,
                &owner,
                20,
                Command::SetOutputFilters {
                    expected_revision: before.revision.clone(),
                    outputs,
                },
                Instant::now(),
            ),
            code,
        );
    }
    assert_eq!(service.snapshot(), before, "no partial write");
    assert!(!control.config_dirty.load(Ordering::Acquire));

    let apply = Command::SetOutputFilters {
        expected_revision: before.revision.clone(),
        outputs: settings,
    };
    let Reply::Snapshot { snapshot: after } =
        command(&mut service, &owner, 21, apply.clone(), Instant::now())
    else {
        panic!()
    };
    assert_ne!(after.revision, before.revision);
    let filter = after.outputs[0].filter.clone().expect("a live filter");
    assert_eq!(filter.origin, FilterOrigin::Live);
    assert_eq!(filter.file, None);
    assert_eq!(
        (filter.sample_rate, filter.taps, filter.hash),
        (Some(96_000), Some(20_000), done.hash.clone())
    );
    assert_eq!(after.outputs[1].filter, None, "null drops the file too");
    assert_eq!(
        control.editable_layout().speakers[1].correction_filter,
        None
    );
    let live = control.live.read();
    assert!(matches!(
        live.output_filters.source(0),
        Some(FilterSource::Samples(_))
    ));
    assert_eq!(live.inline_filters.len(), 1);
    drop(live);
    assert!(
        control.config_dirty.load(Ordering::Acquire),
        "the Save button lights"
    );
    // Applied uploads are consumed; the stale revision is refused.
    assert_error(
        command(&mut service, &owner, 22, apply, Instant::now()),
        "stale_snapshot",
    );

    // Another client's session refuses it, the owner's too (the snapshot
    // the session holds would no longer describe what plays).
    let (session_id, active) = begin(&mut service, &owner, Instant::now());
    let (stranger, _stranger_client) = peer();
    let change = |id: &str| Command::SetOutputFilters {
        expected_revision: active.revision.clone(),
        outputs: vec![OutputFilterSetting {
            output_id: first.clone(),
            filter: Some(id.into()),
        }],
    };
    assert_error(
        command(
            &mut service,
            &stranger,
            30,
            change(&other.upload_id),
            Instant::now(),
        ),
        "busy",
    );
    assert_error(
        command(
            &mut service,
            &owner,
            31,
            change(&other.upload_id),
            Instant::now(),
        ),
        "busy",
    );
    command(
        &mut service,
        &owner,
        32,
        Command::End { session_id },
        Instant::now(),
    );
    let revision = service.snapshot().revision;
    let Reply::Snapshot { snapshot } = command(
        &mut service,
        &owner,
        33,
        Command::SetOutputFilters {
            expected_revision: revision,
            outputs: vec![OutputFilterSetting {
                output_id: first,
                filter: Some(other.upload_id),
            }],
        },
        Instant::now(),
    ) else {
        panic!()
    };
    assert_eq!(snapshot.outputs[0].filter.as_ref().unwrap().taps, Some(2));
}

#[test]
fn uploads_are_bounded_ordered_and_checked() {
    let mut service = Service::new(fixture_control(), 42, None);
    let (owner, _client) = peer();
    let chunk = |upload_id: Option<String>, rate: u32, total: u32, offset: u32, taps: &[f32]| {
        Command::UploadFilter {
            upload_id,
            sample_rate: rate,
            total_taps: total,
            offset,
            taps: encode_taps(taps),
        }
    };
    let now = Instant::now();
    for (id, bad) in [
        chunk(None, 8_000, 1, 0, &[1.0]),
        chunk(None, 48_000, 72_065, 0, &[1.0]),
        chunk(None, 48_000, 0, 0, &[1.0]),
        chunk(None, 48_000, 4, 2, &[1.0]),
        chunk(None, 48_000, 1, 0, &[f32::INFINITY]),
        chunk(None, 48_000, 9000, 0, &[0.1; MAX_UPLOAD_CHUNK_TAPS + 1]),
        Command::UploadFilter {
            upload_id: None,
            sample_rate: 48_000,
            total_taps: 1,
            offset: 0,
            taps: "not base64!".into(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        assert_error(
            command(&mut service, &owner, 1 + id as u64, bad, now),
            "invalid_filter",
        );
    }
    let Reply::FilterUpload { upload } = command(
        &mut service,
        &owner,
        20,
        chunk(None, 44_100, 4, 0, &[0.5, 0.5]),
        now,
    ) else {
        panic!()
    };
    let id = Some(upload.upload_id.clone());
    assert_eq!((upload.received_taps, upload.hash), (2, None));
    // Out of order, another length, past the end: refused, the upload kept.
    for bad in [
        chunk(id.clone(), 44_100, 4, 1, &[0.5]),
        chunk(id.clone(), 44_100, 5, 2, &[0.5]),
        chunk(id.clone(), 44_100, 4, 2, &[0.5, 0.5, 0.5]),
    ] {
        assert_error(
            command(&mut service, &owner, 21, bad, now),
            "invalid_filter",
        );
    }
    let Reply::FilterUpload { upload } = command(
        &mut service,
        &owner,
        22,
        chunk(id.clone(), 44_100, 4, 2, &[0.25, 0.0]),
        now,
    ) else {
        panic!()
    };
    assert_eq!(upload.hash, Some(taps_hash_hex(&[0.5, 0.5, 0.25, 0.0])));
    assert_error(
        command(
            &mut service,
            &owner,
            23,
            chunk(id, 44_100, 4, 4, &[0.5]),
            now,
        ),
        "invalid_filter",
    );
    // A silent filter is refused once complete, and forgotten.
    let Reply::FilterUpload { upload } = command(
        &mut service,
        &owner,
        24,
        chunk(None, 48_000, 2, 0, &[0.0]),
        now,
    ) else {
        panic!()
    };
    assert_error(
        command(
            &mut service,
            &owner,
            25,
            chunk(Some(upload.upload_id.clone()), 48_000, 2, 1, &[0.0]),
            now,
        ),
        "invalid_filter",
    );
    assert_error(
        command(
            &mut service,
            &owner,
            26,
            chunk(Some(upload.upload_id), 48_000, 2, 1, &[0.0]),
            now,
        ),
        "unknown_upload",
    );
    // A refused first chunk holds no slot.
    for i in 0..MAX_UPLOADS as u64 + 1 {
        assert_error(
            command(
                &mut service,
                &owner,
                40 + i,
                chunk(None, 48_000, 1, 0, &[1.0, 1.0]),
                now,
            ),
            "invalid_filter",
        );
    }
    assert_eq!(service.uploads.len(), 1, "only the complete upload above");
    // At most MAX_UPLOADS kept; an unused one expires.
    for i in 0..MAX_UPLOADS - 1 {
        let reply = command(
            &mut service,
            &owner,
            100 + i as u64,
            chunk(None, 48_000, 2, 0, &[1.0]),
            now,
        );
        assert!(
            matches!(reply, Reply::FilterUpload { .. }),
            "{i}: {reply:?}"
        );
    }
    assert_error(
        command(
            &mut service,
            &owner,
            300,
            chunk(None, 48_000, 2, 0, &[1.0]),
            now,
        ),
        "upload_limit",
    );
    let later = now + UPLOAD_TTL + Duration::from_secs(1);
    assert!(matches!(
        command(
            &mut service,
            &owner,
            301,
            chunk(None, 48_000, 2, 0, &[1.0]),
            later
        ),
        Reply::FilterUpload { .. }
    ));
    assert_eq!(service.uploads.len(), 1);
}

#[test]
fn a_bypassing_session_holds_the_filters_off_until_it_ends_or_its_lease_runs_out() {
    let control = fixture_control();
    let mut service = Service::new(control.clone(), 42, None);
    let (owner, _client) = peer();
    let now = Instant::now();
    let begin_bypassing = |service: &mut Service, id: u64, now: Instant| {
        let snapshot = service.snapshot();
        assert!(!snapshot.output_filters_bypassed);
        let generation = control.live_state_generation();
        let reply = command(
            service,
            &owner,
            id,
            Command::Begin {
                expected_revision: snapshot.revision,
                bypass_output_filters: true,
            },
            now,
        );
        let Reply::Session {
            session_id,
            snapshot,
            ..
        } = reply
        else {
            panic!("{reply:?}")
        };
        assert!(snapshot.output_filters_bypassed);
        assert!(control.measurement.filters_bypassed(|| now));
        assert_ne!(
            control.live_state_generation(),
            generation,
            "clients are told"
        );
        session_id
    };
    let session_id = begin_bypassing(&mut service, 1, now);
    assert!(service.snapshot().output_filters_bypassed);
    command(&mut service, &owner, 2, Command::End { session_id }, now);
    assert!(!control.measurement.filters_bypassed(|| now));
    assert!(!control.measurement.filters_bypass_requested());

    // A lease that runs out gives them back on the render thread at once…
    begin_bypassing(&mut service, 3, now);
    let expired = now + Duration::from_millis(LEASE_MS + 1);
    assert!(!control.measurement.filters_bypassed(|| expired));
    // …and the next poll ends the session and clears the request.
    service.poll(expired);
    assert!(!service.active());
    assert!(!control.measurement.filters_bypass_requested());

    // A session without the option leaves them alone.
    begin(&mut service, &owner, now + Duration::from_secs(10));
    assert!(
        !control
            .measurement
            .filters_bypassed(|| now + Duration::from_secs(10))
    );
}

#[test]
fn a_version_8_sweep_waits_for_the_filters_to_settle() {
    use renderer::output_correction::settled_key;
    let control = fixture_control();
    let mut service = Service::new(control.clone(), 42, None);
    let (owner, _client) = peer();
    let filter = upload(&mut service, &owner, 1, 48_000, &decaying(500), 500);
    let snapshot = service.snapshot();
    let reply = command(
        &mut service,
        &owner,
        2,
        Command::SetOutputFilters {
            expected_revision: snapshot.revision,
            outputs: vec![OutputFilterSetting {
                output_id: snapshot.outputs[0].id.clone(),
                filter: Some(filter.upload_id),
            }],
        },
        Instant::now(),
    );
    assert!(matches!(reply, Reply::Snapshot { .. }), "{reply:?}");
    let generation = control.live.read().output_filters.generation;
    let now = Instant::now();
    let snapshot = service.snapshot();
    let Reply::Session { session_id, .. } = command(
        &mut service,
        &owner,
        3,
        Command::Begin {
            expected_revision: snapshot.revision.clone(),
            bypass_output_filters: true,
        },
        now,
    ) else {
        panic!()
    };
    // The stage still plays the filters (no render thread here).
    control
        .output_correction_settled
        .store(settled_key(generation, true), Ordering::Release);
    assert_error(
        command(
            &mut service,
            &owner,
            4,
            sweep(&session_id, &snapshot, 1),
            now,
        ),
        "filters_settling",
    );
    control
        .output_correction_settled
        .store(settled_key(generation, false), Ordering::Release);
    assert!(matches!(
        command(
            &mut service,
            &owner,
            5,
            sweep(&session_id, &snapshot, 1),
            now
        ),
        Reply::Session { .. }
    ));
    command(&mut service, &owner, 6, Command::End { session_id }, now);

    // A version-7 session plays as it always did.
    control
        .output_correction_settled
        .store(0, Ordering::Release);
    let snapshot = service.snapshot();
    let begun = request(
        &mut service,
        &owner,
        7,
        Command::Begin {
            expected_revision: snapshot.revision.clone(),
            bypass_output_filters: false,
        },
    );
    let Reply::Session { session_id, .. } = begun.reply else {
        panic!("{begun:?}")
    };
    let started = service.handle(
        Request {
            version: 7,
            request_id: 8,
            command: sweep(&session_id, &snapshot, 1),
        },
        &owner,
        Instant::now(),
    );
    assert!(
        matches!(started.reply, Reply::Session { .. }),
        "{started:?}"
    );
}

#[test]
fn a_bass_managed_sweep_may_start_below_the_output_band_and_is_echoed() {
    let control = fixture_control();
    let mut service = Service::new(control.clone(), 42, None);
    let (owner, _client) = peer();
    let now = Instant::now();
    let (session_id, mut snapshot) = begin(&mut service, &owner, now);
    // Output 0 is crossed over at 120 Hz.
    snapshot.outputs[0].freq_low_hz = Some(120.0);
    service.session.as_mut().unwrap().snapshot = snapshot.clone();
    let low = |id: u64, bass_managed: bool| {
        let Command::Start {
            session_id,
            signal_id,
            output_id,
            reference_output_id,
            sequence_outputs,
            mut signal,
            ..
        } = sweep(&session_id, &snapshot, id)
        else {
            unreachable!()
        };
        signal.start_hz = 20.0;
        Command::Start {
            session_id,
            signal_id,
            output_id,
            reference_output_id,
            sequence_outputs,
            signal,
            bass_managed,
        }
    };
    assert_error(
        command(&mut service, &owner, 2, low(1, false), now),
        "invalid_signal",
    );
    let reply = command(&mut service, &owner, 3, low(1, true), now);
    let Reply::Session {
        signal: Some(progress),
        ..
    } = reply
    else {
        panic!("{reply:?}")
    };
    assert!(progress.bass_managed);
    assert_eq!(progress.output_id, snapshot.outputs[0].id);
}
