// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Error types shared by core operations.

use std::path::PathBuf;

/// Core operation result.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced by `itentional-core`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A configuration or intent is invalid.
    #[error("validation failed: {0}")]
    Validation(String),

    /// A required file could not be read or written.
    #[error("failed to access {path}: {source}")]
    Io {
        /// File involved in the operation.
        path: PathBuf,
        /// Underlying filesystem error.
        source: std::io::Error,
    },

    /// YAML could not be parsed or serialized.
    #[error("invalid YAML: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

impl Error {
    /// Attach a path to an I/O error.
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
