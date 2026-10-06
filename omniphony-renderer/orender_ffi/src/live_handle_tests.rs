//! The C ABI on a live session: what a host gets back when it passes NULL
//! where it should not, a buffer too small, garbage input, an unknown option,
//! or a value out of range — and that the session stays usable afterwards.
//!
//! The session runs the reference bridge (built through the dev-dependency) on
//! the bundled demo WAV, with a config of its own that turns OSC off: the
//! shell's `OMNIPHONY_OSC_PORT` would otherwise bring it up on a live port.

use super::*;
use std::ffi::CString;
use std::path::Path;
use std::sync::atomic::AtomicUsize;

/// A session on the reference bridge, and what it was created from.
struct Session {
    handle: *mut OrenderRenderer,
    dir: PathBuf,
    /// Held for the session's life: creating a session stops the process-wide
    /// degraded reporter another test may be waiting on.
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: the handle came from `orender_create` and is freed once.
        unsafe { orender_destroy(self.handle) };
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn reference_bridge() -> CString {
    let exe = std::env::current_exe().expect("test binary path");
    let name = format!(
        "{}reference_bridge{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    let path = exe
        .ancestors()
        .skip(1)
        .take(2)
        .map(|dir| dir.join(&name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("{name} not built next to {}", exe.display()));
    CString::new(path.to_str().expect("utf-8 path")).unwrap()
}

fn demo() -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../assets/demo/spatial-demo.wav");
    std::fs::read(path).expect("bundled demo")
}

fn session() -> Session {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let lock = SESSION_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join(format!(
        "orender-ffi-live-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let config_path = dir.join("config.yaml");
    std::fs::write(
        &config_path,
        "render:\n  osc: false\n  evaluation_cartesian_x_size: 9\n  \
         evaluation_cartesian_y_size: 9\n  evaluation_cartesian_z_size: 5\n",
    )
    .expect("write config");
    let config = CString::new(config_path.to_str().expect("utf-8 path")).unwrap();
    let bridge = reference_bridge();
    let cfg = OrenderConfig {
        sample_rate: 48_000,
        config_yaml_path: config.as_ptr(),
        speaker_layout_path: ptr::null(),
        bridge_path: bridge.as_ptr(),
        codec: ptr::null(),
        osc_enabled: 0,
        osc_port_in: 0,
        osc_port_out: 0,
        osc_bind: ptr::null(),
        osc_host: ptr::null(),
    };
    // SAFETY: `cfg` and the strings it points to outlive the call.
    let handle = unsafe { orender_create(&cfg) };
    assert!(!handle.is_null(), "a session on the reference bridge");
    Session {
        handle,
        dir,
        _lock: lock,
    }
}

/// What one `orender_process` call returned.
struct Processed {
    code: c_int,
    frames: usize,
    channels: u32,
}

/// Feed `packet` with an output buffer of `cap` samples.
fn process(s: &Session, packet: &[u8], out: &mut Vec<f32>, cap: usize) -> Processed {
    out.clear();
    out.resize(cap.max(1), f32::NAN);
    let (mut frames, mut channels, mut pts) = (usize::MAX, 0u32, -1i64);
    // SAFETY: a live handle; `packet` and `out` are valid for their lengths.
    let code = unsafe {
        orender_process(
            s.handle,
            packet.as_ptr(),
            packet.len(),
            0,
            out.as_mut_ptr(),
            cap,
            &mut frames,
            &mut channels,
            &mut pts,
        )
    };
    Processed {
        code,
        frames,
        channels,
    }
}

fn set_option(s: &Session, key: &str, value: &str) -> c_int {
    let (key, value) = (CString::new(key).unwrap(), CString::new(value).unwrap());
    // SAFETY: a live handle and two nul-terminated strings.
    unsafe { orender_set_option(s.handle, key.as_ptr(), value.as_ptr()) }
}

/// Query-then-fill: NULL asks the size, a short buffer is left untouched, a
/// fitting one is filled.
#[test]
fn the_layout_queries_never_write_past_the_buffer() {
    let s = session();
    // SAFETY (the block): a live handle and buffers valid for the capacity
    // passed with them.
    unsafe {
        let n = orender_channel_count(s.handle);
        assert_eq!(n, 12, "the 7.1.4 preset");
        assert_eq!(orender_channel_layout(s.handle, ptr::null_mut(), 0), n);

        let mut short = vec![0xEEu8; n as usize - 1];
        assert_eq!(
            orender_channel_layout(s.handle, short.as_mut_ptr(), n - 1),
            n
        );
        assert!(
            short.iter().all(|&b| b == 0xEE),
            "a short buffer is not written"
        );

        let mut labels = vec![0xEEu8; n as usize + 4];
        assert_eq!(
            orender_channel_layout(s.handle, labels.as_mut_ptr(), n + 4),
            n
        );
        assert!(labels[..n as usize].iter().all(|&b| b != 0xEE));
        assert!(
            labels[n as usize..].iter().all(|&b| b == 0xEE),
            "nothing past N"
        );

        // A plain multichannel stream has no bed and no format name.
        assert_eq!(orender_bed_layout(s.handle, ptr::null_mut(), 0), 0);
        let mut name = [0x7f as c_char; 4];
        let len = orender_source_label(s.handle, name.as_mut_ptr(), 4);
        assert!(len == 0 || len >= 4 || name[len as usize] == 0);
    }
}

/// The packet, the output buffer: NULL is refused with -1, and the session is
/// not disturbed by it.
#[test]
fn process_and_drain_refuse_null_buffers() {
    let s = session();
    let mut out = vec![0.0f32; 16];
    // SAFETY (the block): a live handle; NULL where the test says so.
    unsafe {
        let mut frames = 0usize;
        let p = [0u8; 4];
        assert_eq!(
            orender_process(
                s.handle,
                ptr::null(),
                4,
                0,
                out.as_mut_ptr(),
                16,
                &mut frames,
                ptr::null_mut(),
                ptr::null_mut()
            ),
            -1
        );
        assert_eq!(
            orender_process(
                s.handle,
                p.as_ptr(),
                4,
                0,
                ptr::null_mut(),
                16,
                &mut frames,
                ptr::null_mut(),
                ptr::null_mut()
            ),
            -1
        );
        assert_eq!(
            orender_drain(
                s.handle,
                ptr::null_mut(),
                16,
                &mut frames,
                ptr::null_mut(),
                ptr::null_mut()
            ),
            -1
        );
        assert_eq!(
            orender_output_packet_pts(s.handle, ptr::null_mut()),
            -1,
            "a NULL out-parameter"
        );
        let mut pts = 7i64;
        assert_eq!(orender_output_packet_pts(s.handle, &mut pts), 0);
        assert_eq!(pts, 7, "nothing written before any audio");
    }
    // Still a working session.
    let mut out = Vec::new();
    let mut frames = 0;
    for packet in demo().chunks(4096) {
        let p = process(&s, packet, &mut out, 1 << 16);
        assert_eq!(p.code, 0);
        frames += p.frames;
    }
    assert!(frames > 0, "the session renders after the refusals");
}

/// A buffer too small: >0, `*out_frames` = 0, nothing written; the retry with
/// room gets the audio, and no out-parameter is required.
#[test]
fn a_short_buffer_is_reported_and_left_unwritten() {
    let s = session();
    let data = demo();
    let mut out = Vec::new();
    let mut packets = data.chunks(4096);
    let packet = loop {
        let packet = packets.next().expect("a packet that renders");
        let p = process(&s, packet, &mut out, 1);
        if p.code == 0 {
            assert_eq!(p.frames, 0, "a packet that renders nothing fits anywhere");
            continue;
        }
        assert!(p.code > 0, "short buffer: {}", p.code);
        assert_eq!(p.frames, 0);
        assert!(out[0].is_nan(), "nothing written into a short buffer");
        break packet;
    };
    let p = process(&s, packet, &mut out, 1 << 16);
    assert_eq!(p.code, 0);
    assert!(p.frames > 0 && p.channels == 12);
    assert!(out[..p.frames * 12].iter().all(|v| v.is_finite()));
    assert!(
        out[p.frames * 12..].iter().all(|v| v.is_nan()),
        "nothing past the audio"
    );

    // Every out-parameter may be NULL.
    let next = packets.next().expect("another packet");
    // SAFETY: a live handle; `next` and `out` are valid for their lengths.
    let code = unsafe {
        orender_process(
            s.handle,
            next.as_ptr(),
            next.len(),
            0,
            out.as_mut_ptr(),
            out.len(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    assert_eq!(code, 0);
}

/// Bytes that are no stream: an error code or nothing rendered, never a
/// crash, and a reset brings the session back.
#[test]
fn garbage_input_is_an_error_not_a_crash() {
    let s = session();
    let mut out = Vec::new();
    let garbage: Vec<u8> = (0..8192u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8)
        .collect();
    for packet in garbage.chunks(1024) {
        let p = process(&s, packet, &mut out, 1 << 16);
        assert!(p.code <= 0, "garbage is not a short buffer: {}", p.code);
        if p.code == 0 {
            assert_eq!(p.frames, 0, "garbage renders nothing");
        }
    }
    let empty = process(&s, &[], &mut out, 1 << 16);
    assert_eq!((empty.code, empty.frames), (0, 0), "an empty packet");

    // SAFETY: a live handle.
    unsafe { orender_reset(s.handle) };
    let mut frames = 0;
    for packet in demo().chunks(4096) {
        let p = process(&s, packet, &mut out, 1 << 16);
        assert_eq!(p.code, 0);
        frames += p.frames;
    }
    assert!(frames > 0, "the stream after a reset renders");
}

#[test]
fn set_option_reports_unknown_keys_and_invalid_values() {
    let s = session();
    assert_eq!(set_option(&s, "no_such_option", "on"), -1);
    assert_eq!(set_option(&s, "", ""), -1);
    assert_eq!(set_option(&s, "decode_thread", "maybe"), -2);
    assert_eq!(
        set_option(&s, "decode_thread", "ON"),
        -2,
        "values are exact"
    );
    assert_eq!(set_option(&s, "heard_us", "soon"), -2);
    assert_eq!(set_option(&s, "heard_us", "99999999999999999999"), -2);
    assert_eq!(set_option(&s, "heard_us", " 1500 "), 0);
    assert_eq!(set_option(&s, "decode_thread", "on"), 0);
    assert_eq!(set_option(&s, "decode_thread", "off"), 0);

    let key = CString::new("decode_thread").unwrap();
    let not_utf8 = [0xffu8, 0xfe, 0];
    // SAFETY (the block): NULL or nul-terminated strings, a live handle or NULL.
    unsafe {
        assert_eq!(
            orender_set_option(ptr::null_mut(), key.as_ptr(), key.as_ptr()),
            -3
        );
        assert_eq!(orender_set_option(s.handle, ptr::null(), key.as_ptr()), -3);
        assert_eq!(orender_set_option(s.handle, key.as_ptr(), ptr::null()), -3);
        assert_eq!(
            orender_set_option(s.handle, not_utf8.as_ptr() as *const c_char, key.as_ptr()),
            -3,
            "a key that is not UTF-8"
        );
    }
}

/// Codes a host may get wrong leave the session as it was.
#[test]
fn an_unknown_mapping_code_changes_nothing() {
    let s = session();
    // SAFETY (the block): a live handle.
    unsafe {
        let mapping = orender_channel_mapping(s.handle);
        assert!(mapping == 0 || mapping == 1, "{mapping}");
        for code in [-1, 2, 42, c_int::MIN, c_int::MAX] {
            orender_set_channel_mapping(s.handle, code);
            assert_eq!(orender_channel_mapping(s.handle), mapping, "code {code}");
        }
        orender_set_channel_mapping(s.handle, 1 - mapping);
        assert_eq!(orender_channel_mapping(s.handle), 1 - mapping);
    }
}

/// Sessions come and go as tracks do; each one cleans up after itself.
#[test]
fn sessions_can_be_created_and_destroyed_in_turn() {
    let mut out = Vec::new();
    let packet = &demo()[..8192];
    for _ in 0..3 {
        let s = session();
        let p = process(&s, packet, &mut out, 1 << 16);
        assert_eq!(p.code, 0);
    }
    // SAFETY: NULL is documented as ignored; a NULL config as refused.
    unsafe {
        orender_destroy(ptr::null_mut());
        assert!(orender_create(ptr::null()).is_null());
    }
}
