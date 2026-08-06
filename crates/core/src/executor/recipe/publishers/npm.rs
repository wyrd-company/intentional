// ---
// relationships:
//   implements: github-release-executor
// ---

// npm publication recipe selection moved from `executor::recipe` so publisher
// routes can change independently.

use super::*;

pub(super) fn node_package_is_publishable(root: &Path, relative: &Path) -> Result<bool> {
    let path = root.join(relative);
    if !path.is_file() {
        return Ok(false);
    }
    let Some(text) = probed_file_text(&path)? else {
        return Ok(false);
    };
    let value = serde_json::from_str::<serde_json::Value>(&text).map_err(|error| {
        Error::Validation(format!("{} is not valid JSON: {error}", relative.display()))
    })?;
    Ok(value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .is_some()
        && value.get("private").and_then(serde_json::Value::as_bool) != Some(true))
}
