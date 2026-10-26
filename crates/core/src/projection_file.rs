// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Projection file paths and their one resolver.

use crate::error::{Error, Result};
use crate::model::Adapter;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Component, Path, PathBuf};

/// The `file` of a projection, kept as written in configuration.
///
/// A file that starts with `/` is a root file: it resolves from the
/// workspace root. Any other file resolves below the release-unit path.
///
/// The text is private, so that [`ProjectionFile::workspace_path`] is the
/// only way to turn it into a path. No caller joins a release-unit path and
/// a projection file by hand. That matters for a root file: joining `/x`
/// onto any path gives the filesystem root, outside the workspace.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(transparent)]
pub struct ProjectionFile(String);

impl ProjectionFile {
    /// A file relative to the release-unit path.
    pub fn unit_relative(path: &Path) -> Self {
        Self(path.to_string_lossy().into_owned())
    }

    /// Whether this file resolves from the workspace root.
    pub fn is_root(&self) -> bool {
        self.0.starts_with('/')
    }

    /// Workspace-relative path of this file for a release unit at `unit_path`.
    pub fn workspace_path(&self, unit_path: &Path) -> PathBuf {
        match self.0.strip_prefix('/') {
            Some(segments) => segments.split('/').collect(),
            None => normalized(&unit_path.join(&self.0)),
        }
    }

    /// Refuse a file that this release unit and adapter cannot project.
    ///
    /// A root file must be in its one canonical form, and only the generic
    /// `json`, `toml`, and `yaml` adapters may use it. The check reads the
    /// text, not a `Path`, so that every platform gives the same result. A
    /// unit-relative file must be non-empty, relative, and free of `..`. A
    /// leading `\`, or a drive prefix on any segment, is refused in either
    /// form, because on Windows it leaves the workspace.
    pub(crate) fn validate(&self, release_unit_id: &str, adapter: Adapter) -> Result<()> {
        let description = format!("release unit {release_unit_id} projection file");
        if self.is_root() || self.0.starts_with('\\') || has_drive_prefix(&self.0) {
            if !is_canonical_root(&self.0) {
                return Err(Error::Validation(format!(
                    "{description} {} must be a root path of the form /dir/file with no empty, . or .. segments, no \\ and no drive prefix",
                    self.0
                )));
            }
            if !adapter.requires_pointer() {
                return Err(Error::Validation(format!(
                    "release unit {release_unit_id} {} projection file must be relative to the release-unit path; only json, toml, and yaml projections may use a root path",
                    format!("{adapter:?}").to_lowercase()
                )));
            }
            return Ok(());
        }
        validate_relative_path(Path::new(&self.0), &description)
    }

    fn as_path(&self) -> &Path {
        Path::new(&self.0)
    }
}

impl PartialEq for ProjectionFile {
    fn eq(&self, other: &Self) -> bool {
        self.as_path() == other.as_path()
    }
}

impl Eq for ProjectionFile {}

impl PartialEq<&str> for ProjectionFile {
    fn eq(&self, other: &&str) -> bool {
        self.as_path() == Path::new(other)
    }
}

impl fmt::Display for ProjectionFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Exactly one `/`, then `/`-separated segments that are not empty, `.`,
/// or `..`, and do not start with a drive prefix, with no `\` anywhere.
fn is_canonical_root(text: &str) -> bool {
    text.strip_prefix('/').is_some_and(|segments| {
        !segments.contains('\\')
            && segments
                .split('/')
                .all(|segment| !matches!(segment, "" | "." | "..") && !has_drive_prefix(segment))
    })
}

/// `C:` and similar: a Windows drive prefix. On Windows, a path segment
/// with a drive prefix replaces the path it is pushed onto.
fn has_drive_prefix(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// One path per file, so `./docs/x` and `docs/x` are the same key.
pub(crate) fn normalized(relative: &Path) -> PathBuf {
    relative
        .components()
        .filter(|component| !matches!(component, Component::CurDir))
        .collect()
}

/// Refuse a configured relative path that is empty, absolute, contains
/// `..`, or has a segment that starts with a drive prefix. Segments split on
/// both `/` and `\`, so that every platform refuses the same text.
pub(crate) fn validate_relative_path(path: &Path, description: &str) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(Error::Validation(format!(
            "{description} must be a non-empty relative path without .."
        )));
    }
    let text = path.to_string_lossy();
    if let Some(segment) = text
        .split(['/', '\\'])
        .find(|segment| has_drive_prefix(segment))
    {
        return Err(Error::Validation(format!(
            "{description} {text} has a segment {segment} that starts with a drive prefix; on Windows the path would leave the repository"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_relative_file_resolves_below_the_unit_path() {
        let file = ProjectionFile::unit_relative(Path::new("./meta.yml"));
        assert_eq!(
            file.workspace_path(Path::new("site")),
            PathBuf::from("site/meta.yml")
        );
        assert_eq!(
            file.workspace_path(Path::new(".")),
            PathBuf::from("meta.yml")
        );
    }
}
