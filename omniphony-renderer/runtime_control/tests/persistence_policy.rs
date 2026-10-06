//! Tripwires for `docs/persistence-policy.md`: what reaches `config.yaml`
//! without the Save button.
//!
//! The policy's rule is that render and engine state is written only by an
//! explicit Save; the few things written at once are view state, each named
//! with its reason in the policy's *Exceptions*. The compiler cannot see that
//! rule, so these tests scan the sources for the two ways around it and fail
//! on anything they were not told about:
//!
//! - a new `PersistOp`, the targeted write a control handler asks for;
//! - a new caller of the whole-state writers, outside the Save handler, the
//!   profile operations and the shutdown handoff.
//!
//! Failing here is the moment to decide, not to extend a list: if the new
//! write really is view state, add it to the policy's exceptions with its
//! reason, then here.

use std::path::{Path, PathBuf};

/// The targeted writes that bypass the Save button, all view state.
const VIEW_PERSIST_OPS: &[&str] = &[
    // The head-tracker calibration: a measurement of the sensor on the head.
    "HEAD_CENTER",
    "HEAD_AXES",
    // Publication cadences: they shape what clients display.
    "METER_RATE",
    "DIAG_RATE",
];

/// The whole-state writers.
const WRITERS: &[&str] = &[
    "save_live_config(",
    "save_live_config_to_path(",
    "commit_config(",
];

/// Where they may be called from, outside `persist.rs` itself.
const WRITER_CALLERS: &[(&str, &str)] = &[
    ("orender_engine/src/osc/export.rs", "the Save handler"),
    (
        "orender_engine/src/osc/dispatch.rs",
        "routes /control/save_config to the Save handler",
    ),
    (
        "orender_engine/src/osc/profiles.rs",
        "a profile switch sent with \"save\", and the switch's own commit",
    ),
    (
        "orender_engine/src/osc.rs",
        "the shutdown handoff, which writes the sidecar, not config.yaml",
    ),
];

/// The crates whose sources handle controls or write the config.
const SCANNED: &[&str] = &[
    "runtime_control/src",
    "orender_engine/src",
    "host_audio/src",
    "renderer/src",
    "src",
];

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The file's production code: comments dropped, and every `#[cfg(test)]` item
/// left out.
fn production_code(path: &Path) -> String {
    production_code_of(&std::fs::read_to_string(path).expect("readable source"))
}

/// [`production_code`] on source text. A `#[cfg(test)]` item is skipped up to
/// its `;` or its matching closing brace, wherever it sits: a test module is not
/// always the end of its file, and code after it is scanned like any other.
/// Braces are counted naively, which holds for the sources scanned here (no
/// unbalanced brace in a string or char literal inside a test item).
fn production_code_of(src: &str) -> String {
    let mut code = String::new();
    let mut lines = src.lines().map(|l| l.split("//").next().unwrap_or(""));
    while let Some(line) = lines.next() {
        if !line.trim_start().starts_with("#[cfg(test)]") {
            code.push_str(line);
            code.push('\n');
            continue;
        }
        // Skip the item the attribute applies to (and any further attribute).
        let mut depth = 0i32;
        let mut opened = false;
        let mut rest = line.trim_start()["#[cfg(test)]".len()..].to_string();
        loop {
            let item = rest.trim();
            depth += item.matches('{').count() as i32 - item.matches('}').count() as i32;
            opened |= item.contains('{');
            let attribute_only = item.is_empty() || item.starts_with("#[");
            if (opened && depth <= 0) || (!opened && !attribute_only && item.ends_with(';')) {
                break;
            }
            match lines.next() {
                Some(next) => rest = next.to_string(),
                None => break,
            }
        }
    }
    code
}

#[test]
fn the_scan_keeps_code_after_a_test_item() {
    let src = "fn before() {}\n\
               #[cfg(test)]\n\
               mod tests {\n    fn inner() { save_live_config(); }\n}\n\
               fn after() { commit_config(); }\n\
               #[cfg(test)]\n\
               #[allow(dead_code)]\n\
               use something::Else;\n\
               #[cfg(test)]\n\
               mod more_tests;\n\
               fn last() {} // save_live_config(\n";
    let code = production_code_of(src);
    assert!(code.contains("fn before()"), "{code}");
    assert!(
        code.contains("fn after() { commit_config(); }"),
        "code after a test module must be scanned: {code}"
    );
    assert!(code.contains("fn last()"), "{code}");
    assert!(
        !code.contains("inner"),
        "the test module is left out: {code}"
    );
    assert!(
        !code.contains("Else") && !code.contains("more_tests"),
        "{code}"
    );
    assert!(
        !code.contains("save_live_config"),
        "comments are dropped: {code}"
    );
}

#[test]
fn only_view_state_is_written_without_save() {
    let persist = production_code(&workspace().join("runtime_control/src/persist.rs"));
    let start = persist
        .find("impl PersistOp {")
        .expect("impl PersistOp in persist.rs");
    let body = &persist[start..];
    let body = &body[..body.find("\n}\n").expect("end of impl PersistOp")];
    let declared: Vec<&str> = body
        .lines()
        .filter_map(|l| l.trim().strip_prefix("pub const "))
        .filter_map(|l| l.split(':').next())
        .collect();
    for op in &declared {
        assert!(
            VIEW_PERSIST_OPS.contains(op),
            "PersistOp::{op} writes config.yaml without the Save button. Only view state may \
             (docs/persistence-policy.md, \"Exceptions, and why\"): if it is, name it there \
             with its reason, then in VIEW_PERSIST_OPS; if it changes what is heard, mark the \
             config dirty and let the Save write it."
        );
    }
    assert!(
        !body.contains("pub fn "),
        "a PersistOp constructor can mint targeted writes this test cannot list; declare \
         each one as a named constant"
    );
    assert_eq!(declared.len(), VIEW_PERSIST_OPS.len(), "{declared:?}");
}

#[test]
fn only_save_profiles_and_the_handoff_write_the_whole_config() {
    let root = workspace();
    let mut files = Vec::new();
    for dir in SCANNED {
        rust_files(&root.join(dir), &mut files);
    }
    assert!(
        files.len() > 50,
        "the scan found too few sources: {}",
        files.len()
    );
    for file in files {
        let rel = file
            .strip_prefix(&root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        if rel == "runtime_control/src/persist.rs" {
            continue;
        }
        let code = production_code(&file);
        let calls: Vec<&str> = WRITERS
            .iter()
            .copied()
            .filter(|w| {
                code.match_indices(w).any(|(i, _)| {
                    // A call, not the definition or a longer name.
                    let before = &code[..i];
                    !before.ends_with("fn ") && !before.ends_with('_')
                })
            })
            .collect();
        if calls.is_empty() {
            continue;
        }
        assert!(
            WRITER_CALLERS.iter().any(|(allowed, _)| *allowed == rel),
            "{rel} writes the whole live state to config.yaml ({calls:?}). Only the Save \
             button may (docs/persistence-policy.md): mark the config dirty instead and let \
             the Save write it."
        );
    }
}
