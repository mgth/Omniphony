//! The ABI a bridge is built against, pinned to the crate's minor version.
//!
//! The host loads only a bridge built against its own `bridge_api` minor
//! (`BRIDGE_API.md`, "Versioning"): abi_stable refuses a type whose package
//! minor differs from the host's, whatever its layout. So a change to the
//! layout a bridge sees — a trait method, a root-module field, a field or a
//! variant of a type they carry — must come with a minor bump, or bridges of
//! the same version number stop loading in each other's hosts.
//!
//! This test walks that layout from the root module — and from what crosses
//! the boundary outside it, the log callback `set_host_log_sink` receives as
//! a `usize` — writes it as text, and compares it with `abi-baseline.txt`:
//!
//! - same layout: passes (the header must name the current minor);
//! - layout changed, minor not bumped: fails, and no environment variable
//!   lets it through — bump the minor;
//! - layout changed, minor bumped: regenerate the baseline with
//!   `UPDATE_BRIDGE_ABI_BASELINE=1 cargo test -p bridge_api --test abi_baseline`
//!   and commit it with the change, so the review shows the ABI diff.
//!
//! Sizes and alignments are part of the text: `repr_attr` does not report an
//! `align(N)`, and a type can change both without a field changing. They
//! depend on the pointer width, so the baseline describes 64-bit targets and
//! the test runs there; a source change moves the 64-bit layout as well, so
//! that is where it is caught. The text leaves out what does not reach the
//! ABI or differs between builds: source lines, module paths, type ids,
//! parameter names.

#![cfg(target_pointer_width = "64")]
#![allow(non_local_definitions)]

use abi_stable::StableAbi;
use abi_stable::std_types::RStr;
use abi_stable::std_types::UTypeId;
use abi_stable::type_layout::{TLData, TLField, TLFields, TypeLayout};
use bridge_api::{BridgeHostLogSink, BridgeLibRef, RLogLevel};
use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::path::PathBuf;

const UPDATE_VAR: &str = "UPDATE_BRIDGE_ABI_BASELINE";

fn baseline_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("abi-baseline.txt")
}

/// `major.minor` of this crate: the part of the version a bridge must match.
fn current_minor() -> (u32, u32) {
    let parse = |s: &str| s.parse::<u32>().expect("numeric crate version");
    (
        parse(env!("CARGO_PKG_VERSION_MAJOR")),
        parse(env!("CARGO_PKG_VERSION_MINOR")),
    )
}

/// What crosses the boundary outside the root module's layout: the host's
/// log callback, which `set_host_log_sink` receives as a `usize`, so neither
/// abi_stable's load check nor a walk from the root module sees its
/// signature. A field of a struct, because a function pointer's own layout
/// is opaque: only a field carries its parameter and return types.
///
/// The derive reads a signature only when it is spelled out in the field, not
/// through the `BridgeHostLogSink` alias, so it is spelled out here and
/// [`same_signature`] keeps it equal to the alias.
#[repr(C)]
#[derive(StableAbi)]
struct OutOfBand {
    host_log_sink: extern "C" fn(level: RLogLevel, target: RStr<'_>, message: RStr<'_>),
}

/// Compiles only while `OutOfBand::host_log_sink` and `BridgeHostLogSink`
/// are the same type, in both directions.
#[allow(dead_code)]
fn same_signature(sink: BridgeHostLogSink) -> BridgeHostLogSink {
    let spelled_out: extern "C" fn(RLogLevel, RStr<'_>, RStr<'_>) = sink;
    spelled_out
}

/// The whole ABI a bridge sees: the root module, then what goes around it.
fn describe_abi() -> String {
    describe(&[
        <BridgeLibRef as StableAbi>::LAYOUT,
        <OutOfBand as StableAbi>::LAYOUT,
    ])
}

/// Every type reachable from `roots`, once each, in the order the walk first
/// meets them. Types are told apart by their type id, not their printed name:
/// abi_stable prints `RVec<u8>` and `RVec<REvent>` both as `RVec`, so a type
/// whose name is already taken gets a `#2`, `#3`… suffix.
fn describe(roots: &[&'static TypeLayout]) -> String {
    let mut walk = Walk::default();
    for root in roots {
        walk.name_of(root);
    }
    while let Some(layout) = walk.queue.pop_front() {
        walk.describe_type(layout);
    }
    walk.out
}

#[derive(Default)]
struct Walk {
    names: HashMap<UTypeId, String>,
    taken: HashMap<String, usize>,
    queue: VecDeque<&'static TypeLayout>,
    out: String,
}

impl Walk {
    /// The name `layout` goes by in the text; queues it on first sight.
    fn name_of(&mut self, layout: &'static TypeLayout) -> String {
        let id = layout.get_utypeid();
        if let Some(name) = self.names.get(&id) {
            return name.clone();
        }
        let printed = layout.full_type().to_string().trim().to_string();
        let count = self.taken.entry(printed.clone()).or_insert(0);
        *count += 1;
        let name = if *count == 1 {
            printed
        } else {
            format!("{printed}#{count}")
        };
        self.names.insert(id, name.clone());
        self.queue.push_back(layout);
        name
    }

    fn describe_type(&mut self, layout: &'static TypeLayout) {
        let name = self.name_of(layout);
        let _ = writeln!(
            self.out,
            "type {name} [{} repr={:?} size={} align={}{}]",
            layout.package(),
            layout.repr_attr(),
            layout.size(),
            layout.alignment(),
            if layout.is_nonzero() { " nonzero" } else { "" },
        );
        match layout.data() {
            TLData::Primitive(primitive) => {
                let _ = writeln!(self.out, "  primitive {primitive:?}");
            }
            TLData::Opaque => {
                let _ = writeln!(self.out, "  opaque");
            }
            TLData::Struct { fields } => self.describe_fields("field", fields),
            TLData::Union { fields } => self.describe_fields("union field", fields),
            TLData::Enum(tl_enum) => {
                let _ = writeln!(
                    self.out,
                    "  enum exhaustive={} discriminants={:?}",
                    tl_enum.exhaustiveness.is_exhaustive(),
                    tl_enum.discriminants,
                );
                let counts = tl_enum.field_count.as_slice();
                for (variant, count) in tl_enum.variant_names_iter().zip(counts) {
                    let _ = writeln!(self.out, "  variant {variant} fields={count}");
                }
                self.describe_fields("field", tl_enum.fields);
            }
            TLData::PrefixType(prefix) => {
                let _ = writeln!(
                    self.out,
                    "  prefix, first suffix field {}",
                    prefix.first_suffix_field
                );
                self.describe_fields("field", prefix.fields);
            }
        }
        self.describe_fields("phantom", layout.phantom_fields());
        let tag = format!("{:?}", layout.tag());
        if tag != "Tag { variant: Primitive(Null) }" {
            let _ = writeln!(self.out, "  tag {tag}");
        }
        self.out.push('\n');
    }

    fn describe_fields(&mut self, kind: &str, fields: TLFields) {
        for field in fields.iter() {
            self.describe_field(kind, &field);
        }
    }

    fn describe_field(&mut self, kind: &str, field: &TLField) {
        let ty = self.name_of(field.layout());
        let _ = writeln!(self.out, "  {kind} {}: {ty}", field.name());
        // Function pointers (a vtable's methods, the root module's entries):
        // their signature, by parameter and return types. Parameter names and
        // lifetimes are left out; they do not reach the ABI.
        for function in field.function_range().iter() {
            let params: Vec<String> = function
                .param_type_layouts
                .iter()
                .map(|get| self.name_of(get()))
                .collect();
            let returns = match function.return_type_layout() {
                Some(get) => self.name_of(get()),
                None => String::from("()"),
            };
            let _ = writeln!(
                self.out,
                "    {}fn {}({}) -> {returns}",
                if function.fn_qualifs.is_unsafe() {
                    "unsafe "
                } else {
                    ""
                },
                function.name,
                params.join(", "),
            );
        }
    }
}

fn render(minor: (u32, u32), body: &str) -> String {
    format!(
        "# bridge_api {}.{} — generated by tests/abi_baseline.rs; do not edit.\n\
         # A change below needs a minor bump of bridge_api (see BRIDGE_API.md).\n\n{body}",
        minor.0, minor.1
    )
}

/// The minor named in a baseline's first line, and the body after the header.
fn parse_baseline(text: &str) -> ((u32, u32), &str) {
    let first = text.lines().next().unwrap_or_default();
    let version = first
        .strip_prefix("# bridge_api ")
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("malformed baseline header: {first:?}"));
    let mut parts = version
        .split('.')
        .map(|p| p.parse::<u32>().expect("numeric"));
    let minor = (parts.next().unwrap(), parts.next().unwrap());
    let body = text.splitn(4, '\n').nth(3).unwrap_or_default();
    (minor, body)
}

/// First line where `a` and `b` differ, for the failure message.
fn first_difference(a: &str, b: &str) -> String {
    let (mut la, mut lb) = (a.lines(), b.lines());
    for line in 1.. {
        match (la.next(), lb.next()) {
            (None, None) => break,
            (x, y) if x == y => continue,
            (x, y) => {
                return format!(
                    "first difference at body line {line}:\n  baseline: {}\n  current:  {}",
                    x.unwrap_or("<end>"),
                    y.unwrap_or("<end>")
                );
            }
        }
    }
    String::from("no difference")
}

#[test]
fn bridge_abi_changes_only_with_a_minor_bump() {
    let current = describe_abi();
    let minor = current_minor();
    let path = baseline_path();
    let update = std::env::var_os(UPDATE_VAR).is_some();

    let stored = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(_) if update => {
            std::fs::write(&path, render(minor, &current)).expect("write baseline");
            return;
        }
        Err(err) => panic!(
            "{} missing ({err}); create it with {UPDATE_VAR}=1 cargo test -p bridge_api --test abi_baseline",
            path.display()
        ),
    };
    let (baseline_minor, baseline_body) = parse_baseline(&stored);

    if baseline_body != current && minor <= baseline_minor {
        panic!(
            "the bridge ABI changed but bridge_api is still {}.{}: bump its minor in \
             bridge_api/Cargo.toml and in the workspace dependency, then regenerate the \
             baseline with {UPDATE_VAR}=1. A bridge built against the old layout under the \
             same version number would be refused by this host.\n{}",
            minor.0,
            minor.1,
            first_difference(baseline_body, &current)
        );
    }
    if baseline_body == current && baseline_minor == minor {
        return;
    }
    if update {
        std::fs::write(&path, render(minor, &current)).expect("write baseline");
        return;
    }
    panic!(
        "abi-baseline.txt is for bridge_api {}.{}, the crate is {}.{}: regenerate it with \
         {UPDATE_VAR}=1 cargo test -p bridge_api --test abi_baseline and commit it.\n{}",
        baseline_minor.0,
        baseline_minor.1,
        minor.0,
        minor.1,
        first_difference(baseline_body, &current)
    );
}

/// The walk reaches the trait's methods through the root module, and the log
/// callback's signature through `OutOfBand`: without this, the vtable or the
/// callback could change behind a baseline that only lists the root module.
#[test]
fn the_walk_reaches_the_trait_vtable() {
    let text = describe_abi();
    for method in [
        "push_packet",
        "fixed_channel_poses",
        "channel_tags",
        "source_families",
        "host_log_sink",
        "RLogLevel",
    ] {
        assert!(
            text.contains(method),
            "`{method}` not found in the described layout"
        );
    }
}
