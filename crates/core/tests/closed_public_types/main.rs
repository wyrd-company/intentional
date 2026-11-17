// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Every public struct and enum in `intentional-core` is closed with
//! `#[non_exhaustive]` unless `open_types.rs` lists it with a reason.

mod extract;
mod open_types;

use extract::{scan_tree, PublicType};
use open_types::OPEN_TYPES;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Scans the crate source and fails unless the scan reached it.
fn scanned_types() -> Vec<PublicType> {
    let types = scan_tree(&source_root());
    assert!(
        types.iter().any(|found| found.path == "plan::ReleasePlan"),
        "scan of {} did not find plan::ReleasePlan; the extractor is not reading the crate",
        source_root().display()
    );
    types
}

fn location(found: &PublicType) -> String {
    format!("crates/core/src/{}:{}", found.file.display(), found.line)
}

#[test]
fn every_public_type_is_closed_or_open_on_purpose() {
    let types = scanned_types();
    let allowed: BTreeSet<&str> = OPEN_TYPES.iter().map(|(path, _)| *path).collect();
    let closed = types.iter().filter(|found| found.closed).count();
    println!(
        "public types: {}; closed: {closed}; open on purpose: {}",
        types.len(),
        types.len() - closed
    );

    let unlisted: Vec<String> = types
        .iter()
        .filter(|found| !found.closed && !allowed.contains(found.path.as_str()))
        .map(|found| {
            format!(
                "{}: {} has no #[non_exhaustive]",
                location(found),
                found.path
            )
        })
        .collect();
    assert!(
        unlisted.is_empty(),
        "open public types not in tests/closed_public_types/open_types.rs:\n{}",
        unlisted.join("\n")
    );
}

#[test]
fn every_open_type_entry_names_an_open_public_type() {
    let types = scanned_types();
    let mut seen = BTreeSet::new();
    let mut problems = Vec::new();
    for (path, reason) in OPEN_TYPES {
        if !seen.insert(*path) {
            problems.push(format!("{path}: listed twice"));
        }
        if reason.trim().is_empty() {
            problems.push(format!("{path}: no reason"));
        }
        match types.iter().find(|found| found.path == *path) {
            None => problems.push(format!(
                "{path}: no such public type; remove the stale entry"
            )),
            Some(found) if found.closed => problems.push(format!(
                "{path}: closed at {}; remove the entry",
                location(found)
            )),
            Some(_) => {}
        }
    }
    assert!(
        problems.is_empty(),
        "tests/closed_public_types/open_types.rs:\n{}",
        problems.join("\n")
    );
}
