// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Continuous-integration workspace validation.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::intent::Intent;
use crate::model::{Adapter, ProjectionMode};
use crate::plan::ReleasePlan;
use crate::status::{read_projection_version, WorkspaceStatus};
use crate::tag::release_version;
use crate::workspace_files::read_contained;
use std::path::Path;

/// Validate configuration, intent references, tag records, baselines,
/// committed projection versions, and deterministic plan generation.
pub fn check_workspace(root: &Path) -> Result<()> {
    let config = Config::load(root)?;
    Intent::load_all(root, &config)?;
    let status = WorkspaceStatus::load(root)?;
    if !status.missing_baselines.is_empty() {
        return Err(Error::Validation(format!(
            "[{}] missing baseline tags for {}; run {}",
            crate::status::MISSING_BASELINE_CODE,
            status.missing_baselines.join(", "),
            crate::status::MISSING_BASELINE_NEXT_ACTION
        )));
    }
    if !status.tag_record_issues.is_empty() {
        return Err(Error::Validation(format!(
            "tag record validation failed: {}",
            status.tag_record_issues.join("; ")
        )));
    }
    let drift = committed_drift(root, &config, &status)?;
    if !drift.is_empty() {
        return Err(Error::Validation(format!(
            "committed projection drift: {}",
            drift.join("; ")
        )));
    }
    let first = ReleasePlan::build(root, None)?.to_canonical_json()?;
    let second = ReleasePlan::build(root, None)?.to_canonical_json()?;
    if first != second {
        return Err(Error::Validation(
            "release plan generation is not deterministic".to_owned(),
        ));
    }
    Ok(())
}

/// Findings for committed projections that do not hold the release version
/// of their unit. The release version is the version of an applied release
/// that waits for its tag, as `tag` detects it, or else the tag-derived
/// current version. Every committed projection of a unit must hold the same
/// version, so a partly applied release fails.
fn committed_drift(root: &Path, config: &Config, status: &WorkspaceStatus) -> Result<Vec<String>> {
    let mut findings = Vec::new();
    for unit in &status.release_units {
        let release_unit = &config.release_units[&unit.id];
        let applied = release_version(root, &release_unit.path, &unit.current)?
            .filter(|version| version > &unit.current);
        let expected = match &applied {
            Some(applied) => format!("applied release {applied}"),
            None => unit.current.to_string(),
        };
        let expected_version = applied.as_ref().unwrap_or(&unit.current).to_string();
        for projection in &release_unit.projections {
            if projection.mode != ProjectionMode::Committed || projection.adapter == Adapter::Go {
                continue;
            }
            let relative = projection.file.workspace_path(&release_unit.path);
            let text = read_contained(root, &relative)?;
            let actual = read_projection_version(root, &relative, projection, &text)?;
            if actual != expected_version {
                let pointer = projection
                    .pointer
                    .as_deref()
                    .map(|pointer| format!(" pointer {pointer}"))
                    .unwrap_or_default();
                findings.push(format!(
                    "release unit {} projection {}{pointer} is {actual}, expected {expected}",
                    unit.id,
                    relative.display(),
                ));
            }
        }
    }
    Ok(findings)
}
