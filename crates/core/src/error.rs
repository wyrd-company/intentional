// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Error types shared by core operations.

use crate::schema::SchemaRecord;
use std::path::PathBuf;

/// Core operation result.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors produced by `intentional-core`.
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

    /// Git state could not be discovered or traversed.
    #[error("git operation failed: {0}")]
    Git(String),

    /// A semantic version could not be represented.
    #[error("invalid semantic version: {0}")]
    Semver(#[from] semver::Error),

    /// A durable record breaks its published schema.
    #[error(
        "{record}{} violates its published schema at #{location}: keyword `{keyword}`: {detail}",
        subject_suffix(.subject)
    )]
    Schema {
        /// Record kind whose schema rejected the document.
        record: SchemaRecord,
        /// Document that failed, such as a plan path or a tag name.
        subject: Option<String>,
        /// JSON Pointer to the failing location in the document.
        location: String,
        /// Schema keyword that failed.
        keyword: String,
        /// Validator description of the failure.
        detail: String,
    },
}

fn subject_suffix(subject: &Option<String>) -> String {
    subject
        .as_deref()
        .map(|subject| format!(" {subject}"))
        .unwrap_or_default()
}

impl Error {
    /// Attach a path to an I/O error.
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    /// Name the document a schema failure belongs to. Other errors pass through.
    pub(crate) fn in_document(self, document: impl Into<String>) -> Self {
        match self {
            Self::Schema {
                record,
                location,
                keyword,
                detail,
                ..
            } => Self::Schema {
                record,
                subject: Some(document.into()),
                location,
                keyword,
                detail,
            },
            other => other,
        }
    }
}
