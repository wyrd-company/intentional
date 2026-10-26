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
/// its nearest existing ancestor plus the remaining names.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FileIdentity {
    Existing(Handle),
    Absent(Handle, Vec<OsString>),
}

impl FileIdentity {
    pub(crate) fn of(absolute: &Path) -> Result<Self> {
        let mut missing = Vec::new();
        let mut current = absolute;
        loop {
            match Handle::from_path(current) {
                Ok(handle) if missing.is_empty() => return Ok(Self::Existing(handle)),
                Ok(handle) => {
                    missing.reverse();
                    return Ok(Self::Absent(handle, missing));
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
