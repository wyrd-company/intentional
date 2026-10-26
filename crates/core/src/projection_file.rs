// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Projection file paths and their one resolver.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Component, Path, PathBuf};

/// The `file` of a projection, kept as written in configuration.
///
/// The text is private, so that [`ProjectionFile::workspace_path`] is the
/// only way to turn it into a path. No caller joins a release-unit path and
/// a projection file by hand.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(transparent)]
pub struct ProjectionFile(String);

impl ProjectionFile {
    /// A file relative to the release-unit path.
    pub fn unit_relative(path: &Path) -> Self {
        Self(path.to_string_lossy().into_owned())
    }

    /// Workspace-relative path of this file for a release unit at `unit_path`.
    pub fn workspace_path(&self, unit_path: &Path) -> PathBuf {
        normalized(&unit_path.join(&self.0))
    }

    /// Refuse a file that is empty, absolute, or contains `..`.
    pub(crate) fn validate(&self, description: &str) -> Result<()> {
        validate_relative_path(Path::new(&self.0), description)
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

/// One path per file, so `./docs/x` and `docs/x` are the same key.
pub(crate) fn normalized(relative: &Path) -> PathBuf {
    relative
        .components()
        .filter(|component| !matches!(component, Component::CurDir))
        .collect()
}

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
