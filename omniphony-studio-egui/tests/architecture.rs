//! Architecture ratchet for the native Studio's UI crate.
//!
//! The Studio is meant to outlive its UI toolkit: the web frontend gave way to
//! egui, and egui may one day give way to something else. That stays cheap
//! only while the code keeps two tiers apart (see `ARCHITECTURE.md`):
//!
//! - the **core**, `core/` (omniphony-studio-core), speaks the renderer's
//!   protocol and owns the application's state and behaviour. It is its own
//!   crate: the compiler stops it from importing the UI, and CI stops any UI
//!   crate from entering its dependency graph;
//! - the **UI tier**, this crate (`app`, `main`, `panels`, `prefs`, `ui`,
//!   `view`, `render`), draws and asks the core to act.
//!
//! The compiler cannot see the second half of the rule: UI code that speaks
//! the protocol, writes the model or runs behaviour of its own. This test
//! scans `src/`, counts each rule's violations per file and compares the
//! counts with `tests/architecture-baseline.txt`:
//!
//! - a count above its baseline fails: move the code, do not raise the count;
//! - a count below its baseline fails too, so that the progress is recorded:
//!   run `UPDATE_ARCHITECTURE_BASELINE=1 cargo test --test architecture` and
//!   commit the lowered file with the change that earned it.
//!
//! `UPDATE_ARCHITECTURE_BASELINE=allow-increase` rewrites the file even when a
//! count grew. It is the maintainer's escape hatch, never a way to get a
//! change through.
//!
//! The scan is lexical, not a parse: comments are skipped and string literals
//! are only looked into by the rule on OSC addresses. It is a tripwire. Each
//! rule is retired once the boundary refactor makes the compiler enforce it
//! (`docs/studio-egui-boundary-plan.md`).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use regex::Regex;

const BASELINE: &str = "tests/architecture-baseline.txt";
const UPDATE_VAR: &str = "UPDATE_ARCHITECTURE_BASELINE";

/// The UI crate's modules, matched by prefix. A file under `src/` outside all
/// of them fails the test until its module is declared here, which is the
/// moment to ask whether it draws (here) or holds protocol, state or behaviour
/// (then it belongs in `core/`).
const MODULES: &[&str] = &[
    "src/app.rs",
    "src/main.rs",
    "src/panels/",
    "src/prefs/",
    "src/ui/",
    "src/view/",
    "src/render/",
];

/// How a rule finds its violations in a scanned file.
enum Matcher {
    /// String literals whose text starts with this.
    LiteralPrefix(&'static str),
    /// Matches of these patterns in the code, comments and strings blanked.
    Code(&'static [&'static str]),
    /// Matches of this pattern whose first group is anything but the word
    /// given, for the one place another pattern already counts that word.
    CodeExcept(&'static str, &'static str),
}

struct Rule {
    id: &'static str,
    /// Why a violation matters, shown when the rule fails.
    why: &'static str,
    /// Where the code goes instead.
    fix: &'static str,
    matchers: &'static [Matcher],
    /// Path prefixes the rule applies to; empty means the whole UI crate.
    only: &'static [&'static str],
    /// Files the rule does not apply to, each for a stated reason.
    exempt: &'static [&'static str],
}

impl Rule {
    fn applies_to(&self, path: &str) -> bool {
        !self.exempt.contains(&path)
            && (self.only.is_empty() || self.only.iter().any(|p| path.starts_with(p)))
    }
}

const RULES: &[Rule] = &[
    Rule {
        id: "osc-address",
        why: "UI code spells a renderer OSC address: it speaks the wire protocol instead of asking the core",
        fix: "add a typed function in core/src/host/commands/ (clamp, update the model, send) and call it",
        matchers: &[Matcher::LiteralPrefix("/omniphony/")],
        only: &[],
        exempt: &[],
    },
    Rule {
        id: "raw-send",
        why: "UI code pushes a raw message down the control channel",
        fix: "call a typed function in core/src/host/commands/ rather than ctl.send*/control.send",
        matchers: &[Matcher::Code(&[
            r"\bctl\s*\.\s*send\w*\s*\(",
            r"\bcontrol\s*\.\s*send\s*\(",
            r"\bsend_(?:json_)?control\s*\(",
        ])],
        only: &[],
        exempt: &[],
    },
    Rule {
        id: "side-effect",
        why: "UI code spawns threads or processes, blocks, or does file or network I/O",
        fix: "run it in a host service (core/src/host/), off the UI thread when it can block, and show its result",
        matchers: &[Matcher::Code(&[
            r"\bthread::(?:spawn|sleep|Builder::new)\s*\(",
            r"\bCommand::new\s*\(",
            r"\b(?:std::)?fs::\w+\s*\(",
            r"\bFile::(?:open|create)\s*\(",
            r"\bOpenOptions::new\s*\(",
            r"\bUdpSocket::bind\s*\(",
            r"\bTcpStream::connect\w*\s*\(",
            r"\.to_socket_addrs\s*\(",
            r"\bureq::\w+",
        ])],
        // Start-up assets, read once before the first frame: the fonts the
        // composition root installs and the head mesh the renderer draws.
        only: &[],
        exempt: &["src/main.rs", "src/render/head.rs"],
    },
    Rule {
        id: "frame-tick",
        why: "periodic behaviour defined in UI code runs only when the toolkit draws a frame",
        fix: "make it a host service with `tick(now) -> Option<Instant>` whose deadline schedules the next wake",
        matchers: &[Matcher::Code(&[r"\bfn\s+maintain_\w+"])],
        only: &[],
        exempt: &[],
    },
    Rule {
        id: "toolkit-in-scene",
        why: "the 3D engine names a toolkit type: it is wgpu plus geometry, and the part of the UI tier a migration should carry over unchanged",
        fix: "say it in wgpu and `view::screen` terms, and adapt in src/ui/scene.rs",
        matchers: &[Matcher::Code(&[
            r"\begui(?:_wgpu)?\s*::",
            r"\buse\s+egui\b",
        ])],
        only: &["src/view/", "src/render/"],
        exempt: &[],
    },
];

/// A source file with its comments blanked and its string literals set
/// apart. Blanked bytes become spaces and newlines survive, so nothing joins
/// across a comment and positions stay put.
struct Scan {
    code: String,
    strings: Vec<String>,
}

fn is_ident(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphanumeric()
}

fn blank(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend(bytes.iter().map(|&c| if c == b'\n' { b'\n' } else { b' ' }));
}

/// Length of the UTF-8 character that starts with `lead`.
fn utf8_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        _ => 4,
    }
}

fn scan(src: &str) -> Scan {
    let b = src.as_bytes();
    let mut code = Vec::with_capacity(b.len());
    let mut strings = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let next = b.get(i + 1).copied();
        // Line comment, doc comments included.
        if c == b'/' && next == Some(b'/') {
            let end = b[i..]
                .iter()
                .position(|&x| x == b'\n')
                .map_or(b.len(), |p| i + p);
            blank(&mut code, &b[i..end]);
            i = end;
            continue;
        }
        // Block comment, nested as Rust allows.
        if c == b'/' && next == Some(b'*') {
            let mut depth = 0usize;
            let mut k = i;
            while k < b.len() {
                if b[k] == b'/' && b.get(k + 1) == Some(&b'*') {
                    depth += 1;
                    k += 2;
                } else if b[k] == b'*' && b.get(k + 1) == Some(&b'/') {
                    depth -= 1;
                    k += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    k += 1;
                }
            }
            let end = k.min(b.len());
            blank(&mut code, &b[i..end]);
            i = end;
            continue;
        }
        let starts_token = i == 0 || !is_ident(b[i - 1]);
        // Raw string: r"…", r#"…"#, br"…".
        if starts_token && (c == b'r' || (c == b'b' && next == Some(b'r'))) {
            let open = if c == b'b' { i + 2 } else { i + 1 };
            let quote = open + b[open..].iter().take_while(|&&x| x == b'#').count();
            if b.get(quote) == Some(&b'"') {
                let hashes = quote - open;
                let body = quote + 1;
                let mut k = body;
                while k < b.len()
                    && !(b[k] == b'"'
                        && b.len() >= k + 1 + hashes
                        && b[k + 1..k + 1 + hashes].iter().all(|&x| x == b'#'))
                {
                    k += 1;
                }
                let end = k.min(b.len());
                code.extend_from_slice(&b[i..body]);
                strings.push(String::from_utf8_lossy(&b[body..end]).into_owned());
                blank(&mut code, &b[body..end]);
                let close = (end + 1 + hashes).min(b.len());
                code.extend_from_slice(&b[end..close]);
                i = close;
                continue;
            }
        }
        // String literal (a `b` prefix has already gone out as code).
        if c == b'"' {
            let body = i + 1;
            let mut k = body;
            while k < b.len() && b[k] != b'"' {
                k += if b[k] == b'\\' { 2 } else { 1 };
            }
            let end = k.min(b.len());
            code.push(b'"');
            strings.push(String::from_utf8_lossy(&b[body..end]).into_owned());
            blank(&mut code, &b[body..end]);
            if end < b.len() {
                code.push(b'"');
            }
            i = end + 1;
            continue;
        }
        // Character literal, so that '"' does not open a string. A quote that
        // closes nothing is a lifetime or a label, and stays as code.
        if c == b'\'' {
            let close = match next {
                Some(b'\\') => b[i + 2..]
                    .iter()
                    .skip(1)
                    .position(|&x| x == b'\'' || x == b'\n')
                    .map(|p| i + 3 + p)
                    .filter(|&k| b[k] == b'\''),
                Some(lead) => {
                    let k = i + 1 + utf8_len(lead);
                    (b.get(k) == Some(&b'\'')).then_some(k)
                }
                None => None,
            };
            if let Some(k) = close {
                blank(&mut code, &b[i..=k]);
                i = k + 1;
                continue;
            }
        }
        code.push(c);
        i += 1;
    }
    Scan {
        code: String::from_utf8(code).expect("only whole characters and spaces are copied"),
        strings,
    }
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("reading {}: {e}", dir.display()))
        .map(|e| e.expect("directory entry").path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|x| x == "rs") {
            out.push(path);
        }
    }
}

fn declared(path: &str) -> bool {
    MODULES.iter().any(|prefix| path.starts_with(prefix))
}

type Counts = BTreeMap<(String, String), usize>;

fn measure(root: &Path) -> (Counts, Vec<String>) {
    let compiled: Vec<Vec<(Regex, Option<&str>)>> = RULES
        .iter()
        .map(|rule| {
            rule.matchers
                .iter()
                .flat_map(|m| match m {
                    Matcher::LiteralPrefix(_) => Vec::new(),
                    Matcher::Code(patterns) => patterns
                        .iter()
                        .map(|p| (Regex::new(p).expect("rule pattern"), None))
                        .collect(),
                    Matcher::CodeExcept(p, word) => {
                        vec![(Regex::new(p).expect("rule pattern"), Some(*word))]
                    }
                })
                .collect()
        })
        .collect();

    let mut files = Vec::new();
    rust_files(&root.join("src"), &mut files);
    let mut counts = Counts::new();
    let mut unclassified = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(root)
            .expect("under the crate")
            .to_string_lossy()
            .replace('\\', "/");
        if !declared(&rel) {
            unclassified.push(rel);
            continue;
        }
        let text = std::fs::read_to_string(&file)
            .unwrap_or_else(|e| panic!("reading {}: {e}", file.display()));
        let scanned = scan(&text);
        for (rule, patterns) in RULES.iter().zip(&compiled) {
            if !rule.applies_to(&rel) {
                continue;
            }
            let mut n = 0;
            for m in rule.matchers {
                if let Matcher::LiteralPrefix(prefix) = m {
                    n += scanned
                        .strings
                        .iter()
                        .filter(|s| s.starts_with(prefix))
                        .count();
                }
            }
            for (re, except) in patterns {
                n += match except {
                    None => re.find_iter(&scanned.code).count(),
                    Some(word) => re
                        .captures_iter(&scanned.code)
                        .filter(|c| c.get(1).is_some_and(|g| g.as_str() != *word))
                        .count(),
                };
            }
            if n > 0 {
                counts.insert((rule.id.to_owned(), rel.clone()), n);
            }
        }
    }
    (counts, unclassified)
}

fn parse_baseline(text: &str) -> Counts {
    let mut counts = Counts::new();
    for (no, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [rule, path, n] = fields[..] else {
            panic!("{BASELINE}:{}: expected `rule path count`", no + 1);
        };
        let n: usize = n
            .parse()
            .unwrap_or_else(|_| panic!("{BASELINE}:{}: bad count {n:?}", no + 1));
        counts.insert((rule.to_owned(), path.to_owned()), n);
    }
    counts
}

fn render_baseline(counts: &Counts) -> String {
    let mut out = String::from(
        "# Architecture ratchet baseline: the violations of each rule, per file,\n\
         # that the boundary refactor has not removed yet. See tests/architecture.rs,\n\
         # ARCHITECTURE.md and docs/studio-egui-boundary-plan.md.\n\
         #\n\
         # Counts only ever go down. After removing a violation, regenerate with\n\
         #   UPDATE_ARCHITECTURE_BASELINE=1 cargo test --test architecture\n\
         # and commit this file with the change.\n\
         #\n\
         # Totals:\n",
    );
    for rule in RULES {
        let total: usize = counts
            .iter()
            .filter(|((id, _), _)| id == rule.id)
            .map(|(_, n)| n)
            .sum();
        let _ = writeln!(out, "#   {:<16} {total}", rule.id);
    }
    out.push_str("#\n# rule            file                                      count\n");
    for rule in RULES {
        for ((id, path), n) in counts {
            if id == rule.id {
                let _ = writeln!(out, "{id:<17} {path:<41} {n}");
            }
        }
    }
    out
}

fn rule(id: &str) -> Option<&'static Rule> {
    RULES.iter().find(|r| r.id == id)
}

#[test]
fn ui_toolkit_boundary_ratchet() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let (current, unclassified) = measure(root);
    let baseline_path = root.join(BASELINE);
    let baseline_text = std::fs::read_to_string(&baseline_path).unwrap_or_default();
    let baseline = parse_baseline(&baseline_text);

    let mut grew = Vec::new();
    let mut shrank = Vec::new();
    let keys: BTreeSet<_> = current.keys().chain(baseline.keys()).cloned().collect();
    for key in keys {
        let now = current.get(&key).copied().unwrap_or(0);
        let was = baseline.get(&key).copied().unwrap_or(0);
        if now > was {
            grew.push((key, was, now));
        } else if now < was {
            shrank.push((key, was, now));
        }
    }

    let mut report = String::new();
    if !unclassified.is_empty() {
        let _ = writeln!(
            report,
            "\nFiles in no declared module. Declare the module in MODULES in\n\
             tests/architecture.rs if it draws; if it holds protocol, state or\n\
             behaviour, it belongs in core/ instead:"
        );
        for path in &unclassified {
            let _ = writeln!(report, "  {path}");
        }
    }

    match std::env::var(UPDATE_VAR).as_deref() {
        Ok(mode @ ("1" | "allow-increase")) => {
            if unclassified.is_empty() && (grew.is_empty() || mode == "allow-increase") {
                std::fs::write(&baseline_path, render_baseline(&current))
                    .unwrap_or_else(|e| panic!("writing {}: {e}", baseline_path.display()));
                return;
            }
        }
        Ok(other) => panic!("{UPDATE_VAR}={other:?}: expected 1 or allow-increase"),
        Err(_) => {}
    }

    if !grew.is_empty() {
        let _ = writeln!(
            report,
            "\nNew violations of the UI boundary (ARCHITECTURE.md). Move the code; do not\n\
             raise the baseline:"
        );
        for ((id, path), was, now) in &grew {
            let r = rule(id);
            let _ = writeln!(report, "  {id:<16} {path}  {was} -> {now}");
            if let Some(r) = r {
                let _ = writeln!(report, "      why: {}", r.why);
                let _ = writeln!(report, "      fix: {}", r.fix);
            }
        }
    }
    if !shrank.is_empty() {
        let _ = writeln!(
            report,
            "\nViolations removed; record the progress with\n  \
             {UPDATE_VAR}=1 cargo test --test architecture\n\
             and commit {BASELINE}:"
        );
        for ((id, path), was, now) in &shrank {
            let _ = writeln!(report, "  {id:<16} {path}  {was} -> {now}");
        }
    }
    assert!(report.is_empty(), "architecture ratchet:{report}");
}

#[test]
fn the_scanner_skips_comments_and_sets_strings_apart() {
    let src = concat!(
        "// ctl.send(\"/omniphony/a\")\n",
        "let a = \"/omniphony/b\"; /* live.app.x = 1; /* nested */ */\n",
        "let q = '\"'; let r = r#\"x \" /omniphony/c\"#; fn f<'a>(x: &'a str) {}\n",
        "live.app.y = 2;\n",
    );
    let s = scan(src);
    assert_eq!(s.strings, vec!["/omniphony/b", "x \" /omniphony/c"]);
    assert!(!s.code.contains("ctl.send"));
    assert!(!s.code.contains("live.app.x"));
    assert!(s.code.contains("live.app.y = 2;"));
    assert!(s.code.contains("fn f<'a>(x: &'a str)"));
    assert_eq!(s.code.lines().count(), src.lines().count());
}
