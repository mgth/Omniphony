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
//! two callbacks, and the modules whose functions they call into. A failure
//! names the line. Move the work out of the callback (a `callback_event!` for a
//! log line, a `try_lock` with a kept copy for shared state) rather than
//! widening what is scanned here.

use std::path::{Path, PathBuf};

/// What a callback must never reach.
const FORBIDDEN: &[&str] = &[".lock()", "println!", "eprintln!", "format!("];

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

/// Modules the callbacks call into, scanned whole (their tests excepted).
const CALLEE_MODULES: &[&str] = &[
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

/// The text from `opening` to the brace that closes it, with the line the
/// body starts on (1-based).
fn closure_body<'a>(text: &'a str, opening: &str, file: &str) -> (&'a str, usize) {
    let start = text
        .find(opening)
        .unwrap_or_else(|| panic!("{file}: callback `{opening}` not found; update the tripwire"));
    let open = start + opening.len() - 1;
    let mut depth = 0usize;
    for (offset, ch) in text[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let line = text[..start].lines().count() + 1;
                    return (&text[start..open + offset + 1], line);
                }
            }
            _ => {}
        }
    }
    panic!("{file}: callback `{opening}` is never closed");
}

/// The code part of a line: nothing after `//`.
fn code(line: &str) -> &str {
    line.split("//").next().unwrap_or("")
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

fn offences(body: &str, first_line: usize, file: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (i, line) in body.lines().enumerate() {
        let code = code(line);
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
    for (file, opening) in CALLBACKS {
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
    let body = "{\n    log::warn!(\"x\");\n    let g = m.lock();\n    callback_event!(l, Warn, \"ok\");\n    let c = m.try_lock();\n    // log::info!(\"comment\")\n    error!(\"bare\");\n}";
    let found = offences(body, 10, "f.rs");
    assert_eq!(found.len(), 3, "{found:#?}");
    assert!(found[0].starts_with("f.rs:11: `warn`"));
    assert!(found[1].starts_with("f.rs:12: `.lock()`"));
    assert!(found[2].starts_with("f.rs:16: `error`"));
}
