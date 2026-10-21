// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Finds the public structs and enums in a source tree and whether each is closed.

use std::fs;
use std::path::{Path, PathBuf};
use syn::{Attribute, Item, Visibility};

/// One `pub struct` or `pub enum` found in a source file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicType {
    /// Module path inside the crate, such as `plan::ReleasePlan`.
    pub path: String,
    /// Source file, relative to the scanned source root.
    pub file: PathBuf,
    /// One-based line of the declaration.
    pub line: usize,
    /// Whether the type carries `#[non_exhaustive]`.
    pub closed: bool,
}

/// Extracts every public type from each `.rs` file under `root`.
pub fn scan_tree(root: &Path) -> Vec<PublicType> {
    let mut files = Vec::new();
    collect_files(root, &mut files);
    files.sort();
    let mut types = Vec::new();
    for file in files {
        let relative = file.strip_prefix(root).expect("file under root");
        let source = fs::read_to_string(&file)
            .unwrap_or_else(|error| panic!("read {}: {error}", file.display()));
        types.extend(scan_source(&source, &module_of(relative), relative));
    }
    types
}

/// Extracts every public type from one file's source.
pub fn scan_source(source: &str, module: &str, file: &Path) -> Vec<PublicType> {
    let parsed =
        syn::parse_file(source).unwrap_or_else(|error| panic!("parse {}: {error}", file.display()));
    let mut types = Vec::new();
    scan_items(&parsed.items, module, file, source, &mut types);
    types
}

fn scan_items(items: &[Item], module: &str, file: &Path, source: &str, out: &mut Vec<PublicType>) {
    for item in items {
        let (kind, vis, ident, attrs) = match item {
            Item::Struct(item) => ("struct", &item.vis, &item.ident, &item.attrs),
            Item::Enum(item) => ("enum", &item.vis, &item.ident, &item.attrs),
            Item::Mod(item) => {
                if let (Some((_, nested)), false) = (&item.content, is_cfg_test(&item.attrs)) {
                    let inner = join(module, &item.ident.to_string());
                    scan_items(nested, &inner, file, source, out);
                }
                continue;
            }
            _ => continue,
        };
        if !matches!(vis, Visibility::Public(_)) {
            continue;
        }
        let name = ident.to_string();
        out.push(PublicType {
            path: join(module, &name),
            file: file.to_path_buf(),
            line: declaration_line(source, kind, &name),
            closed: attrs
                .iter()
                .any(|attr| attr.path().is_ident("non_exhaustive")),
        });
    }
}

fn is_cfg_test(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .meta
                .require_list()
                .is_ok_and(|list| list.tokens.to_string() == "test")
    })
}

/// Line of the first `pub <kind> <name>` whose name is not a prefix of a longer one.
fn declaration_line(source: &str, kind: &str, name: &str) -> usize {
    let needle = format!("pub {kind} {name}");
    source
        .lines()
        .position(|line| {
            line.trim_start()
                .strip_prefix(&needle)
                .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
        })
        .map_or(0, |index| index + 1)
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        fs::read_dir(dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
    for entry in entries {
        let path = entry.expect("directory entry").path();
        if path.is_dir() {
            collect_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Module path of a file: `lib.rs` is the root, `a/mod.rs` and `a.rs` are `a`.
fn module_of(relative: &Path) -> String {
    let mut parts: Vec<String> = relative
        .iter()
        .map(|part| part.to_string_lossy().into_owned())
        .collect();
    let last = parts.pop().unwrap_or_default();
    let stem = last.trim_end_matches(".rs");
    if stem != "lib" && stem != "mod" {
        parts.push(stem.to_string());
    }
    parts.join("::")
}

fn join(module: &str, name: &str) -> String {
    if module.is_empty() {
        name.to_string()
    } else {
        format!("{module}::{name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(source: &str) -> Vec<(String, bool)> {
        scan_source(source, "m", Path::new("m.rs"))
            .into_iter()
            .map(|found| (found.path, found.closed))
            .collect()
    }

    #[test]
    fn attribute_above_a_doc_comment_closes_the_type() {
        let source = "#[non_exhaustive]\n/// Docs.\npub struct Closed { pub a: u8 }\n";
        assert_eq!(scan(source), [("m::Closed".to_string(), true)]);
    }

    #[test]
    fn attribute_above_a_multi_line_derive_closes_the_type() {
        let source =
            "#[non_exhaustive]\n#[derive(\n    Debug,\n    Clone,\n)]\npub enum Closed { A }\n";
        assert_eq!(scan(source), [("m::Closed".to_string(), true)]);
    }

    #[test]
    fn attribute_below_a_derive_closes_the_type() {
        let source = "/// Docs.\n#[derive(Debug)]\n#[non_exhaustive]\npub struct Closed;\n";
        assert_eq!(scan(source), [("m::Closed".to_string(), true)]);
    }

    #[test]
    fn a_type_without_the_attribute_is_open() {
        let source =
            "/// Mentions #[non_exhaustive] only in prose.\n#[derive(Debug)]\npub struct Open;\n";
        assert_eq!(scan(source), [("m::Open".to_string(), false)]);
    }

    #[test]
    fn a_closed_variant_does_not_close_its_enum() {
        let source = "pub enum Open { #[non_exhaustive] A { x: u8 } }\n";
        assert_eq!(scan(source), [("m::Open".to_string(), false)]);
    }

    #[test]
    fn restricted_and_private_items_are_not_counted() {
        let source = "pub(crate) struct A;\npub(super) enum B { X }\npub(in crate::m) struct C;\nstruct D;\n";
        assert!(scan(source).is_empty());
    }

    #[test]
    fn inline_modules_are_scanned_and_test_modules_are_not() {
        let source = "pub mod inner { pub struct Nested; }\n#[cfg(test)]\nmod tests { pub struct Fixture; }\n";
        assert_eq!(scan(source), [("m::inner::Nested".to_string(), false)]);
    }

    #[test]
    fn declaration_line_skips_longer_names_with_the_same_prefix() {
        let source = "pub struct PlanTagRole;\n\npub struct PlanTag;\n";
        let found = scan_source(source, "", Path::new("lib.rs"));
        assert_eq!(found[1].path, "PlanTag");
        assert_eq!(found[1].line, 3);
    }

    #[test]
    fn module_paths_follow_the_file_layout() {
        assert_eq!(module_of(Path::new("lib.rs")), "");
        assert_eq!(module_of(Path::new("plan.rs")), "plan");
        assert_eq!(module_of(Path::new("adapters/mod.rs")), "adapters");
        assert_eq!(
            module_of(Path::new("adapters/format.rs")),
            "adapters::format"
        );
    }
}
