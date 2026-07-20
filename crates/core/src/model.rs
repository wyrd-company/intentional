// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Shared release domain values.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Semantic version change declared by an intent.
#[derive(
    Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(rename_all = "lowercase")]
pub enum Bump {
    /// No change.
    #[default]
    None,
    /// Backwards-compatible bug fix.
    Patch,
    /// Backwards-compatible feature.
    Minor,
    /// Breaking change.
    Major,
}

impl fmt::Display for Bump {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::None => "none",
            Self::Patch => "patch",
            Self::Minor => "minor",
            Self::Major => "major",
        })
    }
}

impl FromStr for Bump {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "patch" => Ok(Self::Patch),
            "minor" => Ok(Self::Minor),
            "major" => Ok(Self::Major),
            _ => Err(format!("expected major, minor, or patch; got {value}")),
        }
    }
}

/// How a version projection is materialized.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProjectionMode {
    /// Version is written for a source-controlled release commit.
    Committed,
    /// Version is written only for build-time stamping.
    Injected,
    /// No manifest version is written.
    None,
}

/// Manifest or generic format adapter used by a projection.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Adapter {
    /// npm `package.json`.
    Npm,
    /// Cargo `Cargo.toml`.
    Cargo,
    /// Dart/Flutter `pubspec.yaml`.
    Pub,
    /// PEP 621 `pyproject.toml`.
    Python,
    /// MSBuild project file.
    Msbuild,
    /// Go module file.
    Go,
    /// Generic JSON file.
    Json,
    /// Generic TOML file.
    Toml,
    /// Generic YAML file.
    Yaml,
}

impl Adapter {
    /// Whether the adapter requires an explicit pointer.
    pub fn requires_pointer(self) -> bool {
        matches!(self, Self::Json | Self::Toml | Self::Yaml)
    }

    /// Whether the projection participates in dependency range rewriting.
    pub fn ecosystem(self) -> Option<&'static str> {
        match self {
            Self::Npm => Some("npm"),
            Self::Cargo => Some("cargo"),
            Self::Pub => Some("pub"),
            Self::Python => Some("python"),
            Self::Msbuild => Some("msbuild"),
            Self::Go => Some("go"),
            Self::Json | Self::Toml | Self::Yaml => None,
        }
    }
}
