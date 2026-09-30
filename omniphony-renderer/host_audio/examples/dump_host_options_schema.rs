//! Dump the schema of the options the standalone renderer's host declares
//! (audio output, adaptive resampling, live input) as JSON on stdout.
//!
//! CI pipes this into `omniphony-studio/scripts/check-options-schema.mjs`,
//! next to the core schema (`renderer`'s `dump_options_schema`), so the
//! host's options need Studio i18n coverage too.

fn main() {
    println!("{}", host_audio::host_options_schema_json());
}
