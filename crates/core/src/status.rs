// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Workspace status computation.

use crate::adapters::{
    CargoAdapter, FormatAdapter, JsonFormat, MsbuildAdapter, NpmAdapter, PubAdapter, PythonAdapter,
    TomlFormat, YamlFormat,
};
use crate::config::Config;
use crate::error::Result;
use crate::intent::Intent;
use crate::model::{Adapter, Bump, ProjectionMode};
use crate::ownership::check_physical_ownership;
use crate::version::{aggregate_bumps, resolve_versions, VersionRepository};
use crate::workspace_files::read_contained;
use semver::Version;
use std::path::Path;

/// Stable diagnostic code emitted while primary baseline authority is absent.
pub const MISSING_BASELINE_CODE: &str = "missing-baseline";

/// Stable recovery action emitted with missing-baseline diagnostics.
pub const MISSING_BASELINE_NEXT_ACTION: &str = "intentional tag --baseline";

/// Version status for one release unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseUnitStatus {
    /// Release-unit id.
    pub id: String,
    /// Tag-derived current version.
    pub current: Version,
    /// Intent-derived next version.
    pub next: Version,
    /// Effective bump after dependency propagation.
    pub bump: Bump,
    /// Version of an applied release that waits for its primary tag: the
    /// leading changelog version, when it is above the current version.
    pub applied: Option<Version>,
}

/// Complete read-only workspace status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceStatus {
    /// Pending intent ids.
    pub intents: Vec<String>,
    /// Release-unit versions in id order.
    pub release_units: Vec<ReleaseUnitStatus>,
    /// Manifest versions that differ from tag-derived current versions.
    pub drift: Vec<Drift>,
    /// Release units whose primary baseline tag has not been established.
    pub missing_baselines: Vec<String>,
    /// Recoverable missing or inconsistent annotated release records.
    pub tag_record_issues: Vec<String>,
}

/// One manifest drift finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drift {
    /// Release-unit id.
    pub release_unit: String,
    /// Workspace-relative manifest path.
    pub file: std::path::PathBuf,
    /// Pointer of a generic-format projection.
    pub pointer: Option<String>,
    /// Projection materialization mode.
    pub mode: ProjectionMode,
    /// Tag-derived version.
    pub expected: String,
    /// Manifest-projected version.
    pub actual: String,
}

impl WorkspaceStatus {
    /// Load config, intents, Git tags, and compute effective versions.
    pub fn load(root: &Path) -> Result<Self> {
        let config = Config::load(root)?;
        let intents = Intent::load_all(root, &config)?;
        Self::compute(root, &config, &intents)
    }

    /// Compute status from already-loaded inputs.
    pub fn compute(root: &Path, config: &Config, intents: &[Intent]) -> Result<Self> {
        check_physical_ownership(root, config)?;
        let repository = VersionRepository::discover(root)?;
        let declared = aggregate_bumps(intents.iter().map(|intent| &intent.release_units));
        let mut current_versions = std::collections::BTreeMap::new();
        let mut missing_baselines = Vec::new();
        for id in config.release_units.keys() {
            let (_, primary) = config.primary_tag(id)?;
            let current = repository.current_version(id, &primary.template)?;
            if !repository.has_matching_tag(id, &primary.template)? {
                missing_baselines.push(id.clone());
            }
            current_versions.insert(id.clone(), current);
        }
        let resolved = resolve_versions(config, &declared, &current_versions)?;
        let mut release_units = Vec::with_capacity(config.release_units.len());
        let mut drift = Vec::new();
        for (id, release_unit) in &config.release_units {
            let versions = &resolved[id];
            let expected = versions.current.to_string();
            release_units.push(ReleaseUnitStatus {
                id: id.clone(),
                current: versions.current.clone(),
                next: versions.next.clone(),
                bump: versions.bump,
                applied: applied_version(root, &release_unit.path, &versions.current),
            });
            for projection in &release_unit.projections {
                if projection.mode == ProjectionMode::None || projection.adapter == Adapter::Go {
                    continue;
                }
                let relative = projection.file.workspace_path(&release_unit.path);
                let text = read_contained(root, &relative)?;
                let actual = read_projection_version(root, &relative, projection, &text)?;
                if actual != expected {
                    drift.push(Drift {
                        release_unit: id.clone(),
                        file: relative,
                        pointer: projection.pointer.clone(),
                        mode: projection.mode,
                        expected: expected.clone(),
                        actual,
                    });
                }
            }
        }
        Ok(Self {
            intents: intents.iter().map(|intent| intent.id.clone()).collect(),
            release_units,
            drift,
            missing_baselines,
            tag_record_issues: crate::tag::tag_record_issues(root, config)?,
        })
    }
}

impl WorkspaceStatus {
    /// Drift that fails `check`: a committed projection whose version is
    /// neither the tag-derived current version nor the version of an applied
    /// release that waits for its tag. Injected projections are placeholders
    /// between stamps, so their drift is reported but does not fail.
    pub fn committed_drift(&self) -> Vec<&Drift> {
        self.drift
            .iter()
            .filter(|drift| drift.mode == ProjectionMode::Committed)
            .filter(|drift| {
                self.applied(&drift.release_unit)
                    .is_none_or(|applied| applied.to_string() != drift.actual)
            })
            .collect()
    }

    /// Applied release version of one release unit, if any.
    pub fn applied(&self, release_unit: &str) -> Option<&Version> {
        self.release_units
            .iter()
            .find(|status| status.id == release_unit)
            .and_then(|status| status.applied.as_ref())
    }
}

/// The leading changelog version of a release unit, when it is above the
/// tag-derived current version. `apply` writes it before the harness commits
/// and `tag` records it. An absent changelog, or a leading heading that is not
/// a version, means no applied release.
fn applied_version(root: &Path, unit_path: &Path, current: &Version) -> Option<Version> {
    let changelog = read_contained(root, &unit_path.join("CHANGELOG.md")).ok()?;
    crate::tag::leading_changelog_version(&changelog)
        .ok()
        .flatten()
        .filter(|version| version > current)
}

pub(crate) fn read_projection_version(
    root: &Path,
    relative: &Path,
    projection: &crate::Projection,
    text: &str,
) -> Result<String> {
    match projection.adapter {
        Adapter::Npm => NpmAdapter.version(text),
        Adapter::Cargo => match CargoAdapter.version(text)? {
            Some(version) => Ok(version),
            None => {
                let workspace_path = crate::apply::workspace_manifest(root, relative)?;
                let workspace = read_contained(root, &workspace_path)?;
                TomlFormat.read_text(&workspace, "/workspace/package/version")
            }
        },
        Adapter::Pub => PubAdapter.version(text),
        Adapter::Python => PythonAdapter.version(text),
        Adapter::Msbuild => MsbuildAdapter.version(text),
        Adapter::Json => JsonFormat.read_text(
            text,
            projection.pointer.as_deref().expect("validated pointer"),
        ),
        Adapter::Toml => TomlFormat.read_text(
            text,
            projection.pointer.as_deref().expect("validated pointer"),
        ),
        Adapter::Yaml => YamlFormat.read_text(
            text,
            projection.pointer.as_deref().expect("validated pointer"),
        ),
        Adapter::Go => unreachable!("Go has no manifest version"),
    }
}
