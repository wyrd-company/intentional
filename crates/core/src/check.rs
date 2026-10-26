// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Continuous-integration workspace validation.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::intent::Intent;
use crate::plan::ReleasePlan;
use crate::status::{Drift, WorkspaceStatus};
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
    let drift = status.committed_drift();
    if !drift.is_empty() {
        return Err(Error::Validation(format!(
            "committed projection drift: {}",
            drift
                .iter()
                .map(|drift| drift_finding(&status, drift))
                .collect::<Vec<_>>()
                .join("; ")
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

/// One drift finding: release unit, file, pointer, found and expected version.
fn drift_finding(status: &WorkspaceStatus, drift: &Drift) -> String {
    let pointer = drift
        .pointer
        .as_deref()
        .map(|pointer| format!(" pointer {pointer}"))
        .unwrap_or_default();
    let expected = match status.applied(&drift.release_unit) {
        Some(applied) => format!("{} or applied release {applied}", drift.expected),
        None => drift.expected.clone(),
    };
    format!(
        "release unit {} projection {}{pointer} is {}, expected {expected}",
        drift.release_unit,
        drift.file.display(),
        drift.actual
    )
}
