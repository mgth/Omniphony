//! Tripwire: nothing in the device callbacks may block, log or format.
//!
//! The output callbacks are the engine's only hard-realtime code. A `log::`
//! macro there takes the process logger's mutex and allocates its record; a
//! `Mutex::lock` waits on whichever thread holds it. Both used to sit in the
//! callbacks — the cpal one cloned its config under a lock twice per call, and
//! the PipeWire one logged on its underrun and recovery paths, the moment it
//! was already late. The callbacks now report through
//! `audio_output::callback_log` and read their config with `try_lock`.
//!
//! The compiler cannot hold that, so this scans the sources: the bodies of the
//! two callbacks, and what they call into — whole modules where the module is
//! realtime code throughout, single functions where it is not. A failure names
//! the line. Move the work out of the callback (a `callback_event!` for a log
//! line, a `try_lock` with a kept copy for shared state) rather than narrowing
//! what is scanned here.

use std::path::{Path, PathBuf};

/// What a callback must never reach: a lock, the standard streams (locked,
/// too), or a formatted string.
const FORBIDDEN: &[&str] = &[
    ".lock()",
    ".read()",
    ".write()",
    "println!",
    "eprintln!",
    "eprint!(",
    "print!(",
    "dbg!(",
    "format!(",
    "anyhow!(",
    "bail!(",
];

/// The `log` macros, banned qualified (`log::warn!`) or imported (`warn!(`).
const LOG_MACROS: &[&str] = &["trace", "debug", "info", "warn", "error", "log"];

/// The callback closures: file, and the text that opens each one. The body
/// is everything up to the brace that closes the one this text ends with.
const CALLBACKS: &[(&str, &str)] = &[
    ("src/pipewire.rs", ".process(move |stream, _| {"),
    (
        "src/cpal_output.rs",
        "let render = move |data: &mut [f32]| {",
    ),
    (
        "src/cpal_output.rs",
        "move |data: &mut [T], _: &cpal::OutputCallbackInfo| {",
    ),
];

/// Functions the callbacks call that share their module with code for normal
/// threads (the PipeWire setup, the drain side of the callback log), named the
/// same way as the callbacks.
const CALLEE_FUNCTIONS: &[(&str, &str)] = &[
    (
        "src/pipewire.rs",
        "fn pipewire_rate_for_consume_adjust(consume_adjust: f64) -> f32 {",
    ),
    (
        "src/callback_log.rs",
        "pub fn new(level: log::Level, message: &'static str) -> Self {",
    ),
    (
        "src/callback_log.rs",
        "pub fn with(mut self, name: &'static str, value: impl Into<f64>) -> Self {",
    ),
    (
        "src/callback_log.rs",
        "pub fn with_error(mut self, error: rubato::ResampleError) -> Self {",
    ),
    (
        "src/callback_log.rs",
        "pub fn enabled(&self, level: log::Level) -> bool {",
    ),
    (
        "src/callback_log.rs",
        "pub fn push(&mut self, event: CallbackEvent) {",
    ),
];

/// Modules the callbacks call into, scanned whole (their tests excepted).
const CALLEE_MODULES: &[&str] = &[
    "src/lib.rs",
    "src/adaptive_runtime.rs",
    "src/callback_state.rs",
    "src/iir.rs",
    "src/output_telemetry.rs",
    "src/resampler_fifo.rs",
    "src/ring_buffer_io.rs",
];

fn source(relative: &str) -> String {
    let path: PathBuf = Path::new(env!("CARGO_MANIFEST_DIR")).join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// `text` with every comment, string literal and char literal blanked out,
/// byte for byte and newlines kept: offsets and line numbers still match, and
/// a brace or a macro name inside one of them is no longer there to be found.
/// `text` must start in code (a file, or a body cut out of one).
fn code_only(text: &str) -> String {
    let src = text.as_bytes();
    let mut out = src.to_vec();
    let mut i = 0;
    while i < src.len() {
        let rest = &src[i..];
        let end = if rest.starts_with(b"//") {
            i + rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len())
        } else if rest.starts_with(b"/*") {
            block_comment_end(src, i)
        } else if let Some(end) = raw_string_end(src, i) {
            end
        } else if src[i] == b'"' {
            string_end(src, i)
        } else if let Some(end) = char_literal_end(src, i) {
            end
        } else {
            i += 1;
            continue;
        };
        for byte in &mut out[i..end] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
        i = end;
    }
    // Every blanked span starts and ends on an ASCII delimiter, so whole
    // characters were replaced.
    String::from_utf8(out).expect("blanking keeps the text valid UTF-8")
}

/// The end of the block comment opening at `start`. They nest.
fn block_comment_end(src: &[u8], start: usize) -> usize {
    let mut depth = 0usize;
    let mut i = start;
    while i < src.len() {
        if src[i..].starts_with(b"/*") {
            depth += 1;
            i += 2;
        } else if src[i..].starts_with(b"*/") {
            depth -= 1;
            i += 2;
            if depth == 0 {
                return i;
            }
        } else {
            i += 1;
        }
    }
    src.len()
}

/// The end of the string literal whose opening quote is at `start`.
fn string_end(src: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < src.len() {
        match src[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    src.len()
}

/// The end of the raw string (`r"…"`, `r#"…"#`, with or without a `b`) that
/// starts at `start`, if one does.
fn raw_string_end(src: &[u8], start: usize) -> Option<usize> {
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    if start > 0 && is_ident(src[start - 1]) {
        return None;
    }
    let mut i = start;
    if src.get(i) == Some(&b'b') {
        i += 1;
    }
    if src.get(i) != Some(&b'r') {
        return None;
    }
    i += 1;
    let hashes = src[i..].iter().take_while(|&&b| b == b'#').count();
    i += hashes;
    if src.get(i) != Some(&b'"') {
        return None;
    }
    i += 1;
    while i < src.len() {
        if src[i] == b'"' && src[i + 1..].iter().take_while(|&&b| b == b'#').count() >= hashes {
            return Some(i + 1 + hashes);
        }
        i += 1;
    }
    Some(src.len())
}

/// The end of the char literal that starts at `start`, if one does: a quote
/// there may as well open a lifetime.
fn char_literal_end(src: &[u8], start: usize) -> Option<usize> {
    if src[start] != b'\'' {
        return None;
    }
    let first = *src.get(start + 1)?;
    if first == b'\\' {
        // `'\n'`, `'\''`, `'\u{1F600}'`: the escaped character, then up to
        // the closing quote.
        let close = src.get(start + 3..)?.iter().position(|&b| b == b'\'')?;
        return Some(start + 3 + close + 1);
    }
    let width = match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    };
    (src.get(start + 1 + width) == Some(&b'\'')).then_some(start + 1 + width + 1)
}

/// The text from `opening` to the brace that closes it, with the line the
/// body starts on (1-based).
fn closure_body<'a>(text: &'a str, opening: &str, file: &str) -> (&'a str, usize) {
    let start = text
        .find(opening)
        .unwrap_or_else(|| panic!("{file}: `{opening}` not found; update the tripwire"));
    let open = start + opening.len() - 1;
    let code = code_only(text);
    let mut depth = 0usize;
    for (offset, byte) in code.bytes().enumerate().skip(open) {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    let line = text[..start].matches('\n').count() + 1;
                    return (&text[start..=offset], line);
                }
            }
            _ => {}
        }
    }
    panic!("{file}: `{opening}` is never closed");
}

fn calls_a_log_macro(code: &str) -> Option<&'static str> {
    LOG_MACROS.iter().copied().find(|name| {
        let bang = format!("{name}!(");
        code.match_indices(&bang).any(|(at, _)| {
            let before = code[..at].chars().next_back();
            // `log::warn!(` or a bare `warn!(`, not `callback_event!(` & co.
            code[..at].ends_with("log::")
                || !before.is_some_and(|c| c.is_alphanumeric() || c == '_' || c == ':')
        })
    })
}

/// The lines of `body` that reach something forbidden. `body` starts on
/// `first_line` of `file`.
fn offences(body: &str, first_line: usize, file: &str) -> Vec<String> {
    let mut found = Vec::new();
    let code = code_only(body);
    for (i, (code, line)) in code.lines().zip(body.lines()).enumerate() {
        let what = FORBIDDEN
            .iter()
            .copied()
            .find(|token| code.contains(token))
            .or_else(|| calls_a_log_macro(code));
        if let Some(what) = what {
            found.push(format!(
                "{file}:{}: `{what}` in a device callback: {}",
                first_line + i,
                line.trim()
            ));
        }
    }
    found
}

#[test]
fn the_device_callbacks_neither_lock_nor_log() {
    let mut found = Vec::new();
    for (file, opening) in CALLBACKS.iter().chain(CALLEE_FUNCTIONS) {
        let text = source(file);
        let (body, line) = closure_body(&text, opening, file);
        found.extend(offences(body, line, file));
    }
    for file in CALLEE_MODULES {
        let text = source(file);
        let code = text.split("#[cfg(test)]").next().unwrap_or(&text);
        found.extend(offences(code, 1, file));
    }
    assert!(found.is_empty(), "\n{}\n", found.join("\n"));
}

#[test]
fn the_scan_sees_what_it_bans() {
    let body = [
        "{",                                        // 10
        "    log::warn!(\"x\");",                   // 11: a log macro
        "    let g = m.lock();",                    // 12: a lock
        "    callback_event!(l, Warn, \"ok\");",    // 13
        "    let c = m.try_lock();",                // 14
        "    // log::info!(\"comment\")",           // 15
        "    error!(\"bare\");",                    // 16: an imported log macro
        "    let s = \"log::warn!(in a string)\";", // 17
        "    eprint!(\"x\");",                      // 18: stderr, without the `ln`
        "    dbg!(1);",                             // 19
        "    let e = anyhow::anyhow!(\"{}\", 1);",  // 20: formats and allocates
        "    let r = shared.read();",               // 21: an `RwLock`
        "    /* m.lock() */ let ok = 1;",           // 22
        "}",
    ]
    .join("\n");
    let found = offences(&body, 10, "f.rs");
    let reported: Vec<&str> = found
        .iter()
        .map(|line| line.split(" in a device callback").next().unwrap())
        .collect();
    assert_eq!(
        reported,
        [
            "f.rs:11: `warn`",
            "f.rs:12: `.lock()`",
            "f.rs:16: `error`",
            "f.rs:18: `eprint!(`",
            "f.rs:19: `dbg!(`",
            "f.rs:20: `anyhow!(`",
            "f.rs:21: `.read()`",
        ],
        "{found:#?}"
    );
}

#[test]
fn a_body_ends_at_its_own_brace_on_its_own_line() {
    // Braces in a comment, a char literal, a string and a raw string, each
    // enough to end the body early if it were counted; a lifetime, which is
    // not a char literal; and an offence after all of them.
    let text = [
        "fn before() {}",                       // 1
        "    let f = move |x: &'static str| {", // 2
        "        // not the end }",             // 3
        "        let c = '}';",                 // 4
        "        let q = '\\'';",               // 5
        "        let s = \"}}} \\\" }\";",      // 6
        "        let r = r#\"} \" }\"#;",       // 7
        "        log::warn!(\"x\");",           // 8
        "    };",                               // 9
        "fn after() { m.lock(); }",             // 10
    ]
    .join("\n");
    let (body, line) = closure_body(&text, "move |x: &'static str| {", "f.rs");
    assert_eq!(line, 2);
    assert!(body.ends_with("log::warn!(\"x\");\n    }"), "{body}");
    let found = offences(body, line, "f.rs");
    assert_eq!(found.len(), 1, "{found:#?}");
    assert!(found[0].starts_with("f.rs:8: `warn`"), "{found:#?}");
}
