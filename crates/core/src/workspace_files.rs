// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Workspace files by physical identity, kept inside the repository.

use crate::apply::FileWrite;
use crate::error::{Error, Result};
use crate::projection_file::normalized;
use same_file::Handle;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// Absolute path of a workspace-relative file that resolves inside the
/// repository.
///
/// The nearest existing ancestor of the file is resolved through every
/// symlink and must stay below the resolved repository root. A missing
/// tail cannot leave the repository, because configured paths have no `..`.
/// A symlink whose target does not exist cannot be checked, so it is
/// refused.
pub(crate) fn contained(root: &Path, relative: &Path) -> Result<PathBuf> {
    let relative = normalized(relative);
    let absolute = root.join(&relative);
    let canonical_root = root
        .canonicalize()
        .map_err(|error| Error::io(root, error))?;
    let mut existing = absolute.as_path();
    loop {
        match existing.canonicalize() {
            Ok(canonical) if canonical.starts_with(&canonical_root) => return Ok(absolute),
            Ok(_) => break,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                if existing.symlink_metadata().is_ok() {
                    return Err(Error::Validation(format!(
                        "{} reaches a symlink whose target does not exist, so it cannot be shown to resolve inside the repository",
                        relative.display()
                    )));
                }
                match existing.parent() {
                    Some(parent) => existing = parent,
                    None => break,
                }
            }
            Err(error) => return Err(Error::io(existing, error)),
        }
    }
    Err(Error::Validation(format!(
        "{} resolves outside the repository",
        relative.display()
    )))
}

/// Write planned files, each only while it still resolves inside the
/// repository.
///
/// Every target is checked before any is written, so that a target that
/// moved outside since planning leaves the tree unchanged. Each target is
/// checked again right before its write, after its directories exist.
pub(crate) fn write_contained(root: &Path, writes: &[FileWrite]) -> Result<()> {
    for write in writes {
        contained(root, &write.path)?;
    }
    for write in writes {
        let path = contained(root, &write.path)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
        }
        let path = contained(root, &write.path)?;
        std::fs::write(&path, &write.contents).map_err(|error| Error::io(&path, error))?;
    }
    Ok(())
}

/// Read a workspace file that resolves inside the repository.
pub(crate) fn read_contained(root: &Path, relative: &Path) -> Result<String> {
    let path = contained(root, relative)?;
    std::fs::read_to_string(&path).map_err(|error| Error::io(&path, error))
}

/// The physical file a path reaches, whatever symlinks or letter case it
/// goes through.
///
/// A file that exists is its own handle. A file that does not exist yet is
/// its nearest existing ancestor plus the remaining names, compared by
/// letter case only as far as that ancestor ignores case.
#[derive(Debug)]
pub(crate) enum FileIdentity {
    Existing(Handle),
    Absent {
        ancestor: Handle,
        names: Vec<OsString>,
        case: CaseRule,
    },
}

/// How a directory compares the names of its entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CaseRule {
    Distinguishes,
    Ignores,
    /// No entry of the directory could show the rule.
    Unknown,
}

impl CaseRule {
    /// Probe a directory through one of its entries whose name has letter
    /// case.
    ///
    /// Two entries whose names differ only in case show that the directory
    /// distinguishes case. Otherwise the entry's name with its case swapped
    /// is looked up: if it is not found, the directory distinguishes case;
    /// if it reaches the same entry, the directory ignores case. Equal
    /// handles alone prove nothing, because two distinct entries can be
    /// hard links or symlinks to one file, so the swapped name must not be
    /// an entry of its own. Any other outcome leaves the rule unknown.
    pub(crate) fn of(directory: &Path) -> Self {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return Self::Unknown;
        };
        let mut names = BTreeSet::new();
        for entry in entries {
            let Ok(entry) = entry else {
                return Self::Unknown;
            };
            if names.len() == MAX_PROBED_ENTRIES {
                return Self::Unknown;
            }
            names.insert(entry.file_name());
        }
        for name in &names {
            let Some(text) = name.to_str() else { continue };
            let upper = text.to_uppercase();
            let swapped = if upper == text {
                text.to_lowercase()
            } else {
                upper
            };
            if swapped == text {
                continue;
            }
            if names.contains(std::ffi::OsStr::new(&swapped)) {
                return Self::Distinguishes;
            }
            let Ok(original) = Handle::from_path(directory.join(name)) else {
                continue;
            };
            return match Handle::from_path(directory.join(&swapped)) {
                Ok(other) if other == original => Self::Ignores,
                Err(error) if error.kind() == ErrorKind::NotFound => Self::Distinguishes,
                _ => Self::Unknown,
            };
        }
        Self::Unknown
    }
}

/// A larger directory is not listed in full; its case rule stays unknown.
const MAX_PROBED_ENTRIES: usize = 4096;

fn folded(names: &[OsString]) -> Vec<String> {
    names
        .iter()
        .map(|name| name.to_string_lossy().to_lowercase())
        .collect()
}

impl PartialEq for FileIdentity {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Existing(left), Self::Existing(right)) => left == right,
            (
                Self::Absent {
                    ancestor: left,
                    names: left_names,
                    case,
                },
                Self::Absent {
                    ancestor: right,
                    names: right_names,
                    ..
                },
            ) => {
                left == right
                    && (left_names == right_names
                        || (*case == CaseRule::Ignores
                            && folded(left_names) == folded(right_names)))
            }
            _ => false,
        }
    }
}

impl Eq for FileIdentity {}

impl FileIdentity {
    pub(crate) fn of(absolute: &Path) -> Result<Self> {
        let mut missing = Vec::new();
        let mut current = absolute;
        loop {
            match Handle::from_path(current) {
                Ok(handle) if missing.is_empty() => return Ok(Self::Existing(handle)),
                Ok(handle) => {
                    missing.reverse();
                    return Ok(Self::Absent {
                        ancestor: handle,
                        names: missing,
                        case: CaseRule::of(current),
                    });
                }
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    let (Some(parent), Some(name)) = (current.parent(), current.file_name()) else {
                        return Err(Error::io(current, error));
                    };
                    missing.push(name.to_owned());
                    current = parent;
                }
                Err(error) => return Err(Error::io(current, error)),
            }
        }
    }

    /// Whether two different identities may still be one file: absent
    /// names that differ only by letter case, in a directory whose case
    /// rule is unknown.
    fn may_alias(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Absent {
                    ancestor: left,
                    names: left_names,
                    case: CaseRule::Unknown,
                },
                Self::Absent {
                    ancestor: right,
                    names: right_names,
                    ..
                },
            ) => left == right && folded(left_names) == folded(right_names),
            _ => false,
        }
    }
}

/// In-memory edits to workspace files, one entry per physical file.
///
/// Every path that reaches one file shares its edits. The file is written
/// once, at the first of its paths in path order, so that output stays
/// deterministic whatever order the paths were reached in.
pub(crate) struct EditTree<'a> {
    root: &'a Path,
    files: Vec<TreeFile>,
}

struct TreeFile {
    identity: FileIdentity,
    absolute: PathBuf,
    paths: BTreeSet<PathBuf>,
    original: Option<Option<String>>,
    current: Option<String>,
}

impl<'a> EditTree<'a> {
    pub(crate) fn new(root: &'a Path) -> Self {
        Self {
            root,
            files: Vec::new(),
        }
    }

    fn entry(&mut self, relative: &Path) -> Result<&mut TreeFile> {
        let relative = normalized(relative);
        let absolute = contained(self.root, &relative)?;
        let identity = FileIdentity::of(&absolute)?;
        let index = match self.files.iter().position(|file| file.identity == identity) {
            Some(index) => index,
            None => {
                if let Some(file) = self
                    .files
                    .iter()
                    .find(|file| identity.may_alias(&file.identity))
                {
                    let known = file.paths.iter().next().expect("reached by a path");
                    return Err(Error::Validation(format!(
                        "{} and {} differ only in letter case, and Intentional cannot tell whether their directory ignores case",
                        known.display(),
                        relative.display()
                    )));
                }
                self.files.push(TreeFile {
                    identity,
                    absolute,
                    paths: BTreeSet::new(),
                    original: None,
                    current: None,
                });
                self.files.len() - 1
            }
        };
        let file = &mut self.files[index];
        file.paths.insert(relative);
        Ok(file)
    }

    /// Current text of a file, read from disk on first use.
    pub(crate) fn read_optional(&mut self, relative: &Path) -> Result<Option<String>> {
        let file = self.entry(relative)?;
        if file.original.is_none() {
            let text = match std::fs::read_to_string(&file.absolute) {
                Ok(text) => Some(text),
                Err(error) if error.kind() == ErrorKind::NotFound => None,
                Err(error) => return Err(Error::io(&file.absolute, error)),
            };
            file.current = text.clone();
            file.original = Some(text);
        }
        Ok(file.current.clone())
    }

    /// Current text of a file that must exist.
    pub(crate) fn read(&mut self, relative: &Path) -> Result<String> {
        let file = self.entry(relative)?;
        if file.original.is_none() {
            let text = std::fs::read_to_string(&file.absolute)
                .map_err(|error| Error::io(&file.absolute, error))?;
            file.current = Some(text.clone());
            file.original = Some(Some(text));
        }
        file.current
            .clone()
            .ok_or_else(|| Error::io(&file.absolute, ErrorKind::NotFound.into()))
    }

    /// Replace the text of a file.
    pub(crate) fn set(&mut self, relative: &Path, contents: String) -> Result<()> {
        let file = self.entry(relative)?;
        if file.original.is_none() {
            file.original = Some(None);
        }
        file.current = Some(contents);
        Ok(())
    }

    /// Changed files in path order.
    pub(crate) fn finish(self) -> Vec<FileWrite> {
        let mut writes = self
            .files
            .into_iter()
            .filter_map(|file| {
                let contents = file.current?;
                (file.original.flatten().as_ref() != Some(&contents)).then(|| FileWrite {
                    path: file.paths.into_iter().next().expect("reached by a path"),
                    contents,
                })
            })
            .collect::<Vec<_>>();
        writes.sort_by(|left, right| left.path.cmp(&right.path));
        writes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn symlink(target: &Path, link: &Path) {
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, link).expect("symlink");
        #[cfg(windows)]
        if target.is_dir() {
            std::os::windows::fs::symlink_dir(target, link).expect("directory symlink");
        } else {
            std::os::windows::fs::symlink_file(target, link).expect("file symlink");
        }
    }

    fn workspace() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("workspace");
        fs::create_dir_all(root.join("site")).expect("site directory");
        fs::write(root.join("site/meta.yml"), "a: 1\nb: 1\n").expect("shared file");
        fs::write(root.join("site/other.yml"), "a: 1\n").expect("other file");
        (temp, root)
    }

    #[test]
    fn contained_refuses_files_that_resolve_outside_the_workspace() {
        let (temp, root) = workspace();
        fs::write(temp.path().join("outside.yml"), "a: 1\n").expect("outside file");
        symlink(&temp.path().join("outside.yml"), &root.join("site/out.yml"));
        symlink(temp.path(), &root.join("parent"));
        symlink(
            &root.join("site/missing.yml"),
            &root.join("site/dangling.yml"),
        );
        for (relative, message) in [
            ("site/out.yml", "resolves outside the repository"),
            ("parent/outside.yml", "resolves outside the repository"),
            ("parent/new/CHANGELOG.md", "resolves outside the repository"),
            (
                "site/dangling.yml",
                "reaches a symlink whose target does not exist",
            ),
        ] {
            let error = contained(&root, Path::new(relative))
                .expect_err("outside file refused")
                .to_string();
            assert!(error.contains(message), "{relative}: {error}");
        }
        let error = contained(&root, Path::new("./parent/outside.yml"))
            .expect_err("outside file refused")
            .to_string();
        let reported = Path::new("parent").join("outside.yml");
        assert!(
            error.contains(&format!("failed: {} resolves", reported.display())),
            "{error}"
        );
    }

    #[test]
    fn contained_accepts_files_inside_the_workspace() {
        let (temp, root) = workspace();
        symlink(&root.join("site"), &root.join("docs"));
        symlink(&root, &temp.path().join("alias"));
        for relative in ["site/meta.yml", "docs/meta.yml", "site/new/CHANGELOG.md"] {
            assert_eq!(
                contained(&root, Path::new(relative)).expect("inside file"),
                root.join(relative)
            );
        }
        let alias = temp.path().join("alias");
        assert_eq!(
            contained(&alias, Path::new("docs/meta.yml")).expect("root through a symlink"),
            alias.join("docs/meta.yml")
        );
    }

    #[test]
    fn identity_follows_symlinks_and_hard_links() {
        let (_temp, root) = workspace();
        symlink(&root.join("site/meta.yml"), &root.join("alias.yml"));
        fs::hard_link(root.join("site/meta.yml"), root.join("linked.yml")).expect("hard link");
        let shared = FileIdentity::of(&root.join("site/meta.yml")).expect("identity");
        assert_eq!(
            FileIdentity::of(&root.join("alias.yml")).expect("identity"),
            shared
        );
        assert_eq!(
            FileIdentity::of(&root.join("linked.yml")).expect("identity"),
            shared
        );
        assert_ne!(
            FileIdentity::of(&root.join("site/other.yml")).expect("identity"),
            shared
        );
    }

    #[test]
    fn absent_files_compare_by_existing_ancestor_and_names() {
        let (_temp, root) = workspace();
        symlink(&root.join("site"), &root.join("docs"));
        let real = FileIdentity::of(&root.join("site/new/CHANGELOG.md")).expect("identity");
        assert_eq!(
            FileIdentity::of(&root.join("docs/new/CHANGELOG.md")).expect("identity"),
            real
        );
        assert_ne!(
            FileIdentity::of(&root.join("site/new/OTHER.md")).expect("identity"),
            real
        );
        assert_ne!(
            FileIdentity::of(&root.join("site/CHANGELOG.md")).expect("identity"),
            real
        );
    }

    #[test]
    fn edit_tree_shares_edits_across_paths_to_one_file() {
        let (_temp, root) = workspace();
        symlink(&root.join("site"), &root.join("docs"));
        let mut tree = EditTree::new(&root);
        let text = tree.read(Path::new("./site/meta.yml")).expect("read");
        tree.set(Path::new("site/meta.yml"), text.replace("a: 1", "a: 2"))
            .expect("set");
        let text = tree.read(Path::new("docs/meta.yml")).expect("shared read");
        tree.set(Path::new("docs/meta.yml"), text.replace("b: 1", "b: 2"))
            .expect("set");
        let unchanged = tree.read(Path::new("site/other.yml")).expect("read");
        tree.set(Path::new("site/other.yml"), unchanged)
            .expect("set");
        tree.set(Path::new("docs/new.yml"), "c: 1\n".to_owned())
            .expect("set");
        assert_eq!(
            tree.finish(),
            [
                FileWrite {
                    path: ["docs", "meta.yml"].iter().collect(),
                    contents: "a: 2\nb: 2\n".to_owned(),
                },
                FileWrite {
                    path: ["docs", "new.yml"].iter().collect(),
                    contents: "c: 1\n".to_owned(),
                },
            ]
        );
    }

    #[test]
    fn case_rule_follows_the_directory() {
        let (_temp, root) = workspace();
        let ignores = root.join("SITE").exists();
        let expected = if ignores {
            CaseRule::Ignores
        } else {
            CaseRule::Distinguishes
        };
        assert_eq!(CaseRule::of(&root), expected);
        fs::create_dir(root.join("empty")).expect("empty directory");
        assert_eq!(CaseRule::of(&root.join("empty")), CaseRule::Unknown);
        let new = FileIdentity::of(&root.join("New/CHANGELOG.md")).expect("identity");
        let lower = FileIdentity::of(&root.join("new/CHANGELOG.md")).expect("identity");
        assert_eq!(new == lower, ignores);
    }

    #[test]
    fn case_rule_is_not_fooled_by_entries_that_differ_only_in_case() {
        let (_temp, root) = workspace();
        for (directory, link) in [("hard", "hard link"), ("soft", "symlink")] {
            let directory = root.join(directory);
            fs::create_dir(&directory).expect("probe directory");
            fs::write(directory.join("probe"), "x").expect("probe entry");
            let made = if link == "hard link" {
                fs::hard_link(directory.join("probe"), directory.join("PROBE"))
            } else {
                #[cfg(unix)]
                let made = std::os::unix::fs::symlink("probe", directory.join("PROBE"));
                #[cfg(windows)]
                let made = std::os::windows::fs::symlink_file("probe", directory.join("PROBE"));
                made
            };
            let expected = match made {
                Ok(()) => CaseRule::Distinguishes,
                // Only a directory that ignores case refuses the second name.
                Err(error) if error.kind() == ErrorKind::AlreadyExists => CaseRule::Ignores,
                Err(error) => panic!("{link}: {error}"),
            };
            assert_eq!(CaseRule::of(&directory), expected, "{link}");
        }
    }

    #[test]
    fn edit_tree_refuses_case_variants_where_the_case_rule_is_unknown() {
        let (_temp, root) = workspace();
        fs::create_dir(root.join("empty")).expect("empty directory");
        let mut tree = EditTree::new(&root);
        tree.read_optional(Path::new("empty/New/CHANGELOG.md"))
            .expect("first path");
        let error = tree
            .read_optional(Path::new("empty/new/CHANGELOG.md"))
            .expect_err("case variant refused")
            .to_string();
        assert!(
            error.contains("differ only in letter case, and Intentional cannot tell"),
            "{error}"
        );
    }

    #[test]
    fn edit_tree_reports_a_missing_file() {
        let (_temp, root) = workspace();
        let mut tree = EditTree::new(&root);
        assert_eq!(
            tree.read_optional(Path::new("absent.yml")).expect("read"),
            None
        );
        let error = tree
            .read(Path::new("absent.yml"))
            .expect_err("missing")
            .to_string();
        assert!(error.contains("absent.yml"), "{error}");
    }
}
