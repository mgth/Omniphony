//! `orender render` with a bridge that cannot be loaded: instead of exiting,
//! the CLI idles on the no-bridge runtime, serving the bridge error over OSC so
//! Studio can show it, and still quits cleanly on request.

use rosc::{OscMessage, OscPacket, OscType};
use runtime_control::osc_contract::{CONTROL_QUIT, REGISTER, STATE_RENDER_BRIDGE_ERROR};
use std::net::UdpSocket;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Far above the no-bridge renderer build, even in a debug build.
const READY_DEADLINE: Duration = Duration::from_secs(120);
const EXIT_DEADLINE: Duration = Duration::from_secs(30);
const BRIDGE: &str = "/nonexistent/libnone_bridge.so";

fn free_port() -> u16 {
    UdpSocket::bind("127.0.0.1:0")
        .and_then(|socket| socket.local_addr())
        .expect("free port")
        .port()
}

fn send(socket: &UdpSocket, port: u16, addr: &str, args: Vec<OscType>) {
    let packet = OscPacket::Message(OscMessage {
        addr: addr.to_string(),
        args,
    });
    let bytes = rosc::encoder::encode(&packet).expect("encode OSC");
    socket
        .send_to(&bytes, ("127.0.0.1", port))
        .expect("send OSC");
}

/// Every message of `packet`, bundles flattened.
fn messages(packet: OscPacket, out: &mut Vec<OscMessage>) {
    match packet {
        OscPacket::Message(msg) => out.push(msg),
        OscPacket::Bundle(bundle) => {
            for packet in bundle.content {
                messages(packet, out);
            }
        }
    }
}

/// Register the way Studio does and return the published bridge error, once
/// the runtime answers.
fn registered_bridge_error(child: &mut Child, rx_port: u16, log: &Path) -> String {
    let client = UdpSocket::bind("127.0.0.1:0").expect("client socket");
    client
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let client_port = client.local_addr().unwrap().port() as i32;
    let started = Instant::now();
    let mut buf = vec![0u8; 65_536];
    loop {
        if let Some(status) = child.try_wait().expect("wait on orender") {
            panic!(
                "orender exited with {status} instead of idling\n{}",
                std::fs::read_to_string(log).unwrap_or_default()
            );
        }
        assert!(
            started.elapsed() < READY_DEADLINE,
            "no live state from the no-bridge runtime after {READY_DEADLINE:?}\n{}",
            std::fs::read_to_string(log).unwrap_or_default()
        );
        send(&client, rx_port, REGISTER, vec![OscType::Int(client_port)]);
        let reply_deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < reply_deadline {
            let Ok((len, _)) = client.recv_from(&mut buf) else {
                break;
            };
            let Ok((_, packet)) = rosc::decoder::decode_udp(&buf[..len]) else {
                continue;
            };
            let mut received = Vec::new();
            messages(packet, &mut received);
            for msg in received {
                if msg.addr == STATE_RENDER_BRIDGE_ERROR
                    && let Some(OscType::String(text)) = msg.args.first()
                {
                    return text.clone();
                }
            }
        }
    }
}

/// With neither a working bridge nor `--enable-vbap`, the CLI still comes up
/// on the no-bridge runtime (it used to build no renderer without the flag, so
/// it idled with no OSC listener: Studio saw nothing), publishes why, and ends
/// successfully on an OSC quit.
#[test]
fn an_unloadable_bridge_idles_with_the_error_published_and_quits_cleanly() {
    let work = Path::new(env!("CARGO_TARGET_TMPDIR")).join("no_bridge_idle");
    let _ = std::fs::remove_dir_all(&work);
    // An empty config directory keeps the run off any per-user config.
    let config_dir = work.join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let log = work.join("stderr.log");
    let rx_port = free_port();

    let mut child = Command::new(env!("CARGO_BIN_EXE_orender"))
        .arg("render")
        .arg(work.join("in.thd"))
        .args(["--bridge-path", BRIDGE])
        .args(["--output-backend", "file", "--output-file"])
        .arg(work.join("out.f32"))
        .args(["--osc", "--osc-rx-port", &rx_port.to_string()])
        .args(["--osc-port", &free_port().to_string()])
        .args(["--loglevel", "info"])
        .env("OMNIPHONY_CONFIG_DIR", &config_dir)
        .env_remove("OMNIPHONY_OSC_PORT")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).unwrap())
        .spawn()
        .expect("spawn orender");

    let error = registered_bridge_error(&mut child, rx_port, &log);
    assert!(
        error.contains(BRIDGE),
        "the published bridge error does not name the bridge: {error:?}"
    );

    let client = UdpSocket::bind("127.0.0.1:0").expect("client socket");
    send(&client, rx_port, CONTROL_QUIT, Vec::new());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait on orender") {
            break status;
        }
        if started.elapsed() > EXIT_DEADLINE {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "orender still running {EXIT_DEADLINE:?} after an OSC quit\n{}",
                std::fs::read_to_string(&log).unwrap_or_default()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(
        status.success(),
        "orender exited with {status}\n{}",
        std::fs::read_to_string(&log).unwrap_or_default()
    );
}
