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
    assert_eq!(current.version, 5);
    assert!(!version_4_shaped(&current), "version 5 adds fields");

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
fn versions_outside_4_to_5_are_refused_in_the_current_version() {
    let mut service = Service::new(fixture_control(), 42, None);
    let (owner, _client) = peer();
    for version in [3, 6] {
        let response = request(&mut service, &owner, version, Command::Discover {});
        assert_eq!(response.version, VERSION);
        assert_error(response.reply, "unsupported_version");
    }
}
