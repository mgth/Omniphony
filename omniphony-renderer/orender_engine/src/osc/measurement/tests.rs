use super::super::peer::StreamPeer;
use super::*;
use renderer::test_support::fixture_control;
use std::net::{TcpListener, TcpStream};

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
            0,
        )
        .reply
}
fn begin(service: &mut Service, peer: &Peer, now: Instant) -> (String, Snapshot) {
    let snapshot = service.snapshot(0);
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
fn assert_error(reply: Reply, expected: &str) {
    assert!(
        matches!(&reply, Reply::Error { code, .. } if code == expected),
        "{reply:?}"
    );
}

#[test]
fn sessions_preserve_settings_and_refuse_stale_snapshots() {
    let control = fixture_control();
    let mut service = Service::new(control.clone(), 42);
    let (peer, _client) = peer();
    let now = Instant::now();
    let before = service.snapshot(0);
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
fn owner_and_request_ids_prevent_replayed_or_overlapping_signals() {
    let control = fixture_control();
    let mut service = Service::new(control.clone(), 42);
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
        let mut service = Service::new(control.clone(), 42);
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
        service.poll(later, 0);
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
    let mut service = Service::new(control, 42);
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
    let mut service = Service::new(control, 42);
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
    let mut service = Service::new(fixture_control(), 42);
    let udp = Peer::Udp("127.0.0.1:12345".parse().unwrap());
    assert_error(
        command(&mut service, &udp, 1, Command::Discover {}, Instant::now()),
        "transport_required",
    );
}
