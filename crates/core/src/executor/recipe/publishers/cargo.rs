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

/// One executable a Cargo package can distribute as a native archive.
///
/// An explicit binary target owns its own name. When Cargo's conventional
/// `src/main.rs` target is the only binary, the package name owns it instead.
/// Libraries and packages with several binaries need an explicit package
/// boundary before one publisher can claim a primary executable.
pub(crate) fn cargo_binary_identity(
    root: &Path,
    directory: &Path,
    identity: &str,
) -> std::result::Result<String, String> {
    let absolute_directory = root.join(directory);
    let manifest = directory.join("Cargo.toml");
    let document = std::fs::read_to_string(absolute_directory.join("Cargo.toml"))
        .ok()
        .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok());
    let Some(document) = document else {
        return Err(format!(
            "publication {identity} cannot derive a native executable identity from {}",
            manifest.display()
        ));
    };
    let explicit = document
        .get("bin")
        .and_then(toml_edit::Item::as_array_of_tables)
        .into_iter()
        .flat_map(|bins| bins.iter())
        .filter_map(|bin| {
            let name = bin.get("name").and_then(toml_edit::Item::as_str)?;
            let path = bin
                .get("path")
                .and_then(toml_edit::Item::as_str)
                .map(PathBuf::from);
            Some((name.to_owned(), path))
        })
        .collect::<Vec<_>>();
    let package_name = document
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(toml_edit::Item::as_str);
    let autobins = document
        .get("package")
        .and_then(|package| package.get("autobins"))
        .and_then(toml_edit::Item::as_bool)
        .unwrap_or(true);
    let mut claimed_paths = explicit
        .iter()
        .filter_map(|(_, path)| path.clone())
        .collect::<std::collections::BTreeSet<_>>();
    for (name, path) in &explicit {
        if path.is_some() {
            continue;
        }
        let candidates = [
            (name == package_name.unwrap_or_default()).then_some(PathBuf::from("src/main.rs")),
            Some(PathBuf::from(format!("src/bin/{name}.rs"))),
            Some(PathBuf::from(format!("src/bin/{name}/main.rs"))),
        ];
        if let Some(path) = candidates
            .into_iter()
            .flatten()
            .find(|path| absolute_directory.join(path).is_file())
        {
            claimed_paths.insert(path);
        }
    }
    let mut automatic = Vec::new();
    if autobins {
        if absolute_directory.join("src/main.rs").is_file() {
            automatic
                .extend(package_name.map(|name| (name.to_owned(), PathBuf::from("src/main.rs"))));
        }
        let bin_directory = absolute_directory.join("src/bin");
        if let Ok(entries) = std::fs::read_dir(bin_directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() && path.extension().is_some_and(|extension| extension == "rs") {
                    if let Some(name) = path.file_stem().and_then(std::ffi::OsStr::to_str) {
                        automatic.push((
                            name.to_owned(),
                            PathBuf::from("src/bin").join(path.file_name().expect("file name")),
                        ));
                    }
                } else if path.join("main.rs").is_file() {
                    if let Some(name) = path.file_name().and_then(std::ffi::OsStr::to_str) {
                        automatic.push((
                            name.to_owned(),
                            PathBuf::from("src/bin").join(name).join("main.rs"),
                        ));
                    }
                }
            }
        }
        automatic.retain(|(_, path)| !claimed_paths.contains(path));
        automatic.sort_unstable();
    }
    let targets = explicit
        .iter()
        .map(|(name, _)| name.as_str())
        .chain(automatic.iter().map(|(name, _)| name.as_str()))
        .collect::<Vec<_>>();
    let name = (targets.len() == 1).then(|| targets[0]);
    let Some(name) = name else {
        let found = if targets.is_empty() {
            "none".to_owned()
        } else {
            targets.join(", ")
        };
        return Err(format!(
            "publication {identity} cannot derive one native executable identity from {}; found Cargo binary targets [{found}], but the maintained native packager requires exactly one [[bin]].name or Cargo auto-binary",
            manifest.display(),
        ));
    };
    names::cargo_crate(&names::SuppliedName {
        origin: &format!("{} binary name", manifest.display()),
        value: name,
    })
}
