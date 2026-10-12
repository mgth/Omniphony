//! `BRIDGE_API.md` quotes the ABI's types and trait. Those listings drifted
//! from the code once (#681: methods and fields documented long after they
//! were removed), so every `pub struct` and `pub trait` quoted in a ```rust
//! block of the document must match its definition in `src/lib.rs`, field by
//! field and method by method. Comments, attributes and default method bodies
//! are ignored, so the document may stay terser than the code.
//!
//! When this fails, update the listing in `BRIDGE_API.md` to the code.

use std::path::Path;

const DOC: &str = include_str!("../../BRIDGE_API.md");
const SOURCE: &str = include_str!("../src/lib.rs");

/// The contents of every ```rust fenced block.
fn rust_blocks(markdown: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in markdown.lines() {
        let fence = line.trim_start();
        match &mut current {
            None if fence.starts_with("```rust") => current = Some(String::new()),
            Some(block) if fence.starts_with("```") => {
                blocks.push(std::mem::take(block));
                current = None;
            }
            Some(block) => {
                block.push_str(line);
                block.push('\n');
            }
            None => {}
        }
    }
    blocks
}

/// Drop comments and attributes, line by line.
fn strip(code: &str) -> String {
    code.lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .filter(|line| !line.trim_start().starts_with("#["))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The item `pub <kind> <name>` in `code`, from its keyword to its closing
/// brace, attributes and comments already stripped.
fn item(code: &str, kind: &str, name: &str) -> Option<String> {
    let code = strip(code);
    let start = [" ", "<", ":", "{"]
        .iter()
        .find_map(|next| code.find(&format!("pub {kind} {name}{next}")))?;
    let open = start + code[start..].find('{')?;
    let mut depth = 0usize;
    for (offset, c) in code[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(code[start..=open + offset].to_owned());
                }
            }
            _ => {}
        }
    }
    None
}

/// Default method bodies become `;`, then whitespace is collapsed, so a
/// listing compares equal to its definition however each is laid out.
fn normalize(item: &str) -> String {
    let mut out = String::new();
    let mut depth = 0usize;
    for c in item.chars() {
        match c {
            '{' => {
                depth += 1;
                if depth == 1 {
                    out.push('{');
                }
            }
            '}' => {
                depth -= 1;
                if depth == 1 {
                    out.push(';');
                } else if depth == 0 {
                    out.push('}');
                }
            }
            c if depth <= 1 => out.push(c),
            _ => {}
        }
    }
    let collapsed = out.split_whitespace().collect::<Vec<_>>().join(" ");
    // A trailing comma before the closing brace is layout, not content.
    collapsed
        .replace(" ;", ";")
        .replace(", }", " }")
        .replace(",}", "}")
}

/// Every `pub struct` and `pub trait` named in the document's listings.
fn documented_items() -> Vec<(&'static str, String, String)> {
    let mut items = Vec::new();
    for block in rust_blocks(DOC) {
        let stripped = strip(&block);
        for kind in ["struct", "trait"] {
            let keyword = format!("pub {kind} ");
            let mut rest = stripped.as_str();
            while let Some(at) = rest.find(&keyword) {
                let after = &rest[at + keyword.len()..];
                let name: String = after
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                let listing = item(&block, kind, &name)
                    .unwrap_or_else(|| panic!("unterminated listing of {name}"));
                items.push((kind, name, listing));
                rest = after;
            }
        }
    }
    items
}

#[test]
fn every_listing_in_bridge_api_md_matches_the_code() {
    let items = documented_items();
    // The document quotes the root module, the trait and the four types a
    // decoded chunk is made of; finding fewer means the parser broke.
    assert!(
        items.len() >= 6,
        "found only {} listings in BRIDGE_API.md",
        items.len()
    );
    let mut drifted = Vec::new();
    for (kind, name, listing) in &items {
        let Some(definition) = item(SOURCE, kind, name) else {
            drifted.push(format!(
                "{kind} {name}: not defined in bridge_api/src/lib.rs"
            ));
            continue;
        };
        let (documented, defined) = (normalize(listing), normalize(&definition));
        if documented != defined {
            drifted.push(format!(
                "{kind} {name}:\n  BRIDGE_API.md: {documented}\n  src/lib.rs:    {defined}"
            ));
        }
    }
    assert!(
        drifted.is_empty(),
        "BRIDGE_API.md no longer matches {}:\n{}",
        Path::new("bridge_api/src/lib.rs").display(),
        drifted.join("\n")
    );
}

#[test]
fn the_comparison_sees_a_missing_field_and_ignores_layout() {
    let code =
        "/// doc\n#[repr(C)]\npub struct A {\n    /// x\n    pub x: u32,\n    pub y: u8,\n}\n";
    let same = "pub struct A { pub x: u32, pub y: u8 }";
    let short = "pub struct A {\n    pub x: u32,\n}";
    let defined = normalize(&item(code, "struct", "A").unwrap());
    assert_eq!(normalize(&item(same, "struct", "A").unwrap()), defined);
    assert_ne!(normalize(&item(short, "struct", "A").unwrap()), defined);

    let with_body =
        "pub trait T {\n    fn a(&self) -> u8;\n    fn b(&self) -> u8 {\n        0\n    }\n}";
    let listed = "pub trait T {\n    fn a(&self) -> u8;\n    // default\n    fn b(&self) -> u8;\n}";
    assert_eq!(
        normalize(&item(with_body, "trait", "T").unwrap()),
        normalize(&item(listed, "trait", "T").unwrap())
    );
}
