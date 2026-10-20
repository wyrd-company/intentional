// ---
// relationships:
//   implements:
//     - release-plan
//     - tag-record
// ---

//! Embeds the published record schemas in the crate.
//!
//! A workspace build reads the authoritative YAML in `docs/specifications`. A
//! packaged crate has no `docs` directory; its `schemas` links hold the same
//! files, and `cargo package` stores the bytes they point to.

use std::env;
use std::fs;
use std::path::PathBuf;

const SCHEMAS: [&str; 4] = [
    "release-workspace.schema.yml",
    "release-plan.schema.yml",
    "release-model.schema.yml",
    "tag-record.schema.yml",
];

fn main() {
    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let published = manifest.join("../../docs/specifications");
    for name in SCHEMAS {
        let authoritative = published.join(name);
        let source = if authoritative.is_file() {
            authoritative
        } else {
            manifest.join("schemas").join(name)
        };
        println!("cargo:rerun-if-changed={}", source.display());
        fs::copy(&source, out.join(name))
            .unwrap_or_else(|error| panic!("failed to embed {}: {error}", source.display()));
    }
}
