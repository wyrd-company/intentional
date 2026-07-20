// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Continuous-integration workspace validation.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::intent::Intent;
use crate::plan::ReleasePlan;
use std::path::Path;

/// Validate configuration, intent references, and deterministic plan generation.
pub fn check_workspace(root: &Path) -> Result<()> {
    let config = Config::load(root)?;
    Intent::load_all(root, &config)?;
    let first = ReleasePlan::build(root, None)?.to_canonical_json()?;
    let second = ReleasePlan::build(root, None)?.to_canonical_json()?;
    if first != second {
        return Err(Error::Validation(
            "release plan generation is not deterministic".to_owned(),
        ));
    }
    Ok(())
}
