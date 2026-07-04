//! Dump the declared live-options schema as JSON on stdout.
//!
//! CI pipes this into `omniphony-studio/scripts/check-options-schema.mjs`,
//! which asserts every declared option has Studio i18n coverage — the
//! options-schema contract check from `docs/live-options-registry.md`.

fn main() {
    println!("{}", renderer::options::schema_json());
}
