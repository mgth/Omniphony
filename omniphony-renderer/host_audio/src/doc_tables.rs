//! The options tables of the documentation must say what the registry says
//! (`renderer::options::doc_table`). Run with `UPDATE_DOC_TABLES=1` to
//! rewrite them after changing a row. Here because this crate sees both the
//! core rows and the host's.

use renderer::options::{LIVE_OPTIONS, doc_table};

use crate::options::HOST_OPTIONS;

/// The generated blocks: (document under `docs/`, block name, table).
fn blocks() -> Vec<(&'static str, &'static str, String)> {
    let core = doc_table::markdown(LIVE_OPTIONS.iter().map(Into::into));
    let host = doc_table::markdown(HOST_OPTIONS.iter().map(Into::into));
    vec![
        ("live-options-registry.md", "live-options", core.clone()),
        ("live-options-registry.md", "host-options", host.clone()),
        ("osc-control-contract.md", "live-options", core),
        ("osc-control-contract.md", "host-options", host),
    ]
}

#[test]
fn the_documented_options_tables_match_the_registry() {
    let docs = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs");
    let update = std::env::var_os("UPDATE_DOC_TABLES").is_some_and(|v| v == "1");
    let mut stale = Vec::new();
    for (file, name, table) in blocks() {
        let path = docs.join(file);
        // A checkout may turn the line endings into CRLF (Windows,
        // `core.autocrlf`): the tables are compared, and written, in LF.
        let doc = std::fs::read_to_string(&path)
            .expect("doc readable")
            .replace("\r\n", "\n");
        let fresh = doc_table::replace_block(&doc, name, &table)
            .unwrap_or_else(|| panic!("docs/{file} has no generated block `{name}`"));
        if fresh != doc {
            if update {
                std::fs::write(&path, fresh).expect("doc written");
            } else {
                stale.push(format!("docs/{file} ({name})"));
            }
        }
    }
    assert!(
        stale.is_empty(),
        "stale options tables: {}. Regenerate them with \
         `UPDATE_DOC_TABLES=1 cargo test -p host_audio doc_tables` and commit the docs.",
        stale.join(", ")
    );
}
