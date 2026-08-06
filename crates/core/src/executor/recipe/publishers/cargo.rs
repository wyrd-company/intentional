// ---
// relationships:
//   implements: github-release-executor
// ---

// Cargo publication recipe selection moved from `executor::recipe` so
// publisher routes can change independently.

use super::*;

/// Publication-relevant contents of one Cargo package manifest.
pub(super) struct CargoManifest {
    /// Registries named by `package.publish`, empty when it selects the default.
    registries: Vec<String>,
    /// Whether `package.publish` permits publication at all.
    permitted: bool,
}

impl CargoManifest {
    pub(super) fn publishable(&self) -> bool {
        self.permitted
    }
}

/// Read one Cargo package manifest, or `None` when it declares no package.
pub(super) fn cargo_manifest(root: &Path, relative: &Path) -> Result<Option<CargoManifest>> {
    let path = root.join(relative);
    if !path.is_file() {
        return Ok(None);
    }
    let Some(text) = probed_file_text(&path)? else {
        return Ok(None);
    };
    let document = text.parse::<toml_edit::DocumentMut>().map_err(|error| {
        Error::Validation(format!("{} is not valid TOML: {error}", relative.display()))
    })?;
    let Some(package) = document.get("package") else {
        return Ok(None);
    };
    if package.get("name").is_none() {
        return Ok(None);
    }
    // Cargo's `publish` accepts false, true, and a registry array. false and an
    // empty array both mean the crate must never be published.
    let publish = package.get("publish");
    let registries = publish
        .and_then(toml_edit::Item::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(toml_edit::Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let permitted = match publish {
        None => true,
        Some(item) => match item.as_bool() {
            Some(permitted) => permitted,
            None => !registries.is_empty(),
        },
    };
    Ok(Some(CargoManifest {
        registries,
        permitted,
    }))
}

pub(super) fn cargo_registry(
    root: &Path,
    release_unit: &ReleaseUnitConfig,
    package: &PackageConfig,
) -> Result<String> {
    let relative = package_path(release_unit, package).join("Cargo.toml");
    let Some(manifest) = cargo_manifest(root, &relative)? else {
        return Ok(CRATES_IO.to_owned());
    };
    match manifest.registries.as_slice() {
        [] => Ok(CRATES_IO.to_owned()),
        [registry] => Ok(registry.clone()),
        many => Err(Error::Validation(format!(
            "release unit {} publishes to {} Cargo registries ({}); Cargo publication requires exactly one primary destination",
            release_unit.path.display(),
            many.len(),
            many.join(", ")
        ))),
    }
}
