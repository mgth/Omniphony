//! The options tables of the documentation, generated from the registry.
//!
//! `docs/live-options-registry.md` and `docs/osc-control-contract.md` carry
//! these tables between `<!-- BEGIN GENERATED <name> -->` and
//! `<!-- END GENERATED <name> -->` markers. `host_audio/src/doc_tables.rs`
//! (the crate that sees both the core rows and the host's) fails when a
//! block differs from what the registry says, and rewrites the blocks with
//! `UPDATE_DOC_TABLES=1`.

use super::{
    HostOptionSpec, LegacyAddr, OptionDefault, OptionFlags, OptionGroup, OptionKind, OptionSpec,
};

/// What a table row shows of an option, core or host.
pub struct DocRow {
    pub key: &'static str,
    pub kind: OptionKind,
    pub default: OptionDefault,
    pub flags: OptionFlags,
    pub group: Option<&'static OptionGroup>,
    pub alias: LegacyAddr,
}

impl From<&OptionSpec> for DocRow {
    fn from(spec: &OptionSpec) -> Self {
        Self {
            key: spec.key,
            kind: spec.kind,
            default: spec.default,
            flags: spec.flags,
            group: spec.group,
            alias: spec.legacy_control_addr,
        }
    }
}

impl<H> From<&HostOptionSpec<H>> for DocRow {
    fn from(spec: &HostOptionSpec<H>) -> Self {
        Self {
            key: spec.key,
            kind: spec.kind,
            default: spec.default,
            flags: spec.flags,
            group: spec.group,
            alias: spec.legacy_control_addr,
        }
    }
}

/// A markdown table of `rows`, one line per option.
pub fn markdown(rows: impl IntoIterator<Item = DocRow>) -> String {
    let mut out = String::from(
        "| Key | Value | Default | Group (mode, effect) | Flags | Alias |\n\
         |---|---|---|---|---|---|\n",
    );
    for row in rows {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | {} |\n",
            row.key,
            kind_cell(row.kind),
            default_cell(row.default),
            group_cell(row.group),
            flags_cell(row.flags),
            alias_cell(row.alias),
        ));
    }
    out
}

/// What a value of `kind` is, in a few words (`float [0, 1], step 0.01`).
pub fn kind_cell(kind: OptionKind) -> String {
    match kind {
        OptionKind::Bool => "bool".into(),
        OptionKind::Enum(values) => values
            .iter()
            .map(|v| format!("`{v}`"))
            .collect::<Vec<_>>()
            .join(" \\| "),
        OptionKind::Str => "string".into(),
        OptionKind::Float { min, max, step } => format!("float [{min}, {max}], step {step}"),
        OptionKind::Int { min, max } => int_range("int", min, max),
        OptionKind::OptionalInt { min, max } => int_range("int or null", min, max),
        OptionKind::FloatArray {
            len,
            min,
            max,
            step,
        } => format!("{len} floats [{min}, {max}], step {step}"),
        OptionKind::DynamicEnum { source } => format!("one of `{source}`"),
    }
}

fn int_range(name: &str, min: i64, max: i64) -> String {
    if max >= i64::from(i32::MAX) {
        format!("{name} ≥ {min}")
    } else {
        format!("{name} [{min}, {max}]")
    }
}

fn default_cell(default: OptionDefault) -> String {
    match default {
        OptionDefault::Bool(b) => format!("`{b}`"),
        OptionDefault::Str(s) => format!("`{s:?}`"),
        OptionDefault::Float(f) => format!("`{f}`"),
        OptionDefault::Int(i) => format!("`{i}`"),
        OptionDefault::FloatArray(values) => format!(
            "`[{}]`",
            values
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        OptionDefault::Build => "as built".into(),
        OptionDefault::Unset => "unset".into(),
    }
}

fn group_cell(group: Option<&OptionGroup>) -> String {
    match group {
        Some(group) => format!(
            "`{}` ({}, {})",
            group.key,
            group.mode.as_str(),
            group.effect.as_str()
        ),
        None => "—".into(),
    }
}

fn flags_cell(flags: OptionFlags) -> String {
    let mut names = Vec::new();
    if flags.contains(OptionFlags::REPLAN) {
        names.push("replan");
    }
    if flags.contains(OptionFlags::EMBEDDED_ONLY) {
        names.push("embedded only");
    }
    if names.is_empty() {
        "—".into()
    } else {
        names.join(", ")
    }
}

/// The alias address without the `/omniphony` root the contract's tables
/// leave out.
fn alias_cell(alias: LegacyAddr) -> String {
    let short = |addr: &str| addr.strip_prefix("/omniphony").unwrap_or(addr).to_string();
    match alias {
        LegacyAddr::Exact(addr) => format!("`{}`", short(addr)),
        LegacyAddr::Prefixed { prefix, tail } => format!("`{}{tail}`", short(prefix)),
        LegacyAddr::None => "—".into(),
    }
}

/// Replace the block named `name` in `doc` with `table`. `None` when the doc
/// has no such block.
pub fn replace_block(doc: &str, name: &str, table: &str) -> Option<String> {
    let begin = format!("<!-- BEGIN GENERATED {name} -->\n");
    let end = format!("<!-- END GENERATED {name} -->");
    let start = doc.find(&begin)? + begin.len();
    let stop = start + doc[start..].find(&end)?;
    Some(format!("{}{}{}", &doc[..start], table, &doc[stop..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_block_is_replaced_between_its_markers_only() {
        let doc = "intro\n<!-- BEGIN GENERATED t -->\nold\n<!-- END GENERATED t -->\noutro\n";
        assert_eq!(
            replace_block(doc, "t", "new\n").unwrap(),
            "intro\n<!-- BEGIN GENERATED t -->\nnew\n<!-- END GENERATED t -->\noutro\n"
        );
        assert!(replace_block(doc, "other", "x").is_none());
    }
}
