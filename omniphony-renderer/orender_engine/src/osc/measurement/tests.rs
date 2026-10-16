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
            delay_ms: 2.25 + i as f32,
        })
        .collect();
    let request = Command::SetOutputs {
        expected_revision: before.revision.clone(),
        settings: settings.clone(),
    };
    let mut invalid = settings.clone();
    invalid[1].delay_ms = 101.0;
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
