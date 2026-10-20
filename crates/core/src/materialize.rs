// ---
// relationships:
//   implements: release-planning-and-records
// ---

//! A scratch checkout of one commit, so that workspace operations that read
//! the working tree and HEAD can run against historical state.
//!
//! The checkout is a new repository whose object database borrows the source
//! repository's objects through `objects/info/alternates`. Its HEAD is the
//! commit, detached, and it carries only the tags whose target the commit
//! reaches, so later history cannot change what it reports. Nothing in the
//! source repository changes.

use crate::error::{Error, Result};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_CHECKOUT: AtomicU64 = AtomicU64::new(0);

/// A scratch checkout removed when dropped.
pub(crate) struct Checkout {
    root: PathBuf,
}

impl Checkout {
    /// Check out `commit` of `repository` with every tag whose target `commit`
    /// reaches.
    pub(crate) fn of(repository: &gix::Repository, commit: gix::ObjectId) -> Result<Self> {
        let checkout = Self {
            root: scratch_directory()?,
        };
        let scratch = gix::init(&checkout.root).map_err(|error| {
            Error::Git(format!("failed to initialize a scratch checkout: {error}"))
        })?;
        let git_dir = scratch.git_dir().to_owned();
        let objects = std::fs::canonicalize(repository.common_dir().join("objects"))
            .map_err(|error| Error::io(repository.common_dir().join("objects"), error))?;
        write(
            &git_dir.join("objects/info/alternates"),
            format!("{}\n", objects.display()).as_bytes(),
        )?;
        write(&git_dir.join("HEAD"), format!("{commit}\n").as_bytes())?;
        let reachable = reachable_commits(repository, commit)?;
        for (name, object) in reachable_tags(repository, &reachable)? {
            write(
                &git_dir.join("refs/tags").join(&name),
                format!("{object}\n").as_bytes(),
            )?;
        }
        let tree = repository
            .find_commit(commit)
            .map_err(|error| Error::Git(format!("failed to read commit {commit}: {error}")))?
            .tree()
            .map_err(|error| Error::Git(format!("failed to read the tree of {commit}: {error}")))?;
        write_tree(&tree, &checkout.root)?;
        Ok(checkout)
    }

    /// Root of the checked-out working tree.
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
}

impl Drop for Checkout {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn scratch_directory() -> Result<PathBuf> {
    let base = std::env::temp_dir();
    loop {
        let path = base.join(format!(
            "intentional-checkout-{}-{}",
            std::process::id(),
            NEXT_CHECKOUT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(Error::io(path, error)),
        }
    }
}

fn write(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
    }
    std::fs::write(path, contents).map_err(|error| Error::io(path, error))
}

fn reachable_commits(
    repository: &gix::Repository,
    commit: gix::ObjectId,
) -> Result<HashSet<gix::ObjectId>> {
    let walk = repository
        .rev_walk([commit])
        .all()
        .map_err(|error| Error::Git(format!("failed to walk history from {commit}: {error}")))?;
    walk.map(|info| {
        info.map(|info| info.id)
            .map_err(|error| Error::Git(format!("failed to walk history from {commit}: {error}")))
    })
    .collect()
}

/// Tag names and the objects they name, for tags whose peeled target is in
/// `reachable`.
fn reachable_tags(
    repository: &gix::Repository,
    reachable: &HashSet<gix::ObjectId>,
) -> Result<Vec<(String, gix::ObjectId)>> {
    let references = repository
        .references()
        .map_err(|error| Error::Git(format!("failed to read references: {error}")))?;
    let tags = references
        .tags()
        .map_err(|error| Error::Git(format!("failed to read tags: {error}")))?;
    let mut selected = Vec::new();
    for mut reference in tags.flatten() {
        let Some(object) = reference.try_id().map(|id| id.detach()) else {
            continue;
        };
        let Ok(target) = reference.peel_to_id() else {
            continue;
        };
        if reachable.contains(&target.detach()) {
            selected.push((reference.name().shorten().to_string(), object));
        }
    }
    Ok(selected)
}

fn write_tree(tree: &gix::Tree<'_>, directory: &Path) -> Result<()> {
    for entry in tree.iter() {
        let entry =
            entry.map_err(|error| Error::Git(format!("failed to read a tree entry: {error}")))?;
        let name = gix::path::from_bstr(entry.filename())
            .map_err(|error| Error::Git(format!("tree entry name is not a path: {error}")))?;
        let path = directory.join(name.as_ref());
        let object = || {
            entry
                .object()
                .map_err(|error| Error::Git(format!("failed to read {}: {error}", path.display())))
        };
        match entry.mode().kind() {
            gix::object::tree::EntryKind::Tree => {
                std::fs::create_dir(&path).map_err(|error| Error::io(&path, error))?;
                write_tree(&object()?.into_tree(), &path)?;
            }
            gix::object::tree::EntryKind::Blob | gix::object::tree::EntryKind::BlobExecutable => {
                write(&path, &object()?.data)?;
            }
            gix::object::tree::EntryKind::Link => write_link(&path, &object()?.data)?,
            gix::object::tree::EntryKind::Commit => {
                std::fs::create_dir(&path).map_err(|error| Error::io(&path, error))?;
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn write_link(path: &Path, target: &[u8]) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    std::os::unix::fs::symlink(std::ffi::OsStr::from_bytes(target), path)
        .map_err(|error| Error::io(path, error))
}

#[cfg(not(unix))]
fn write_link(path: &Path, target: &[u8]) -> Result<()> {
    write(path, target)
}
