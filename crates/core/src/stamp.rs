// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Build-time version stamping for injected projections.

use crate::adapters::{
    CargoAdapter, FormatAdapter, JsonFormat, MsbuildAdapter, NpmAdapter, PubAdapter, PythonAdapter,
    TomlFormat, YamlFormat,
};
use crate::apply::{workspace_manifest, FileWrite};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::intent::Intent;
use crate::model::{Adapter, ProjectionMode};
use crate::version::{aggregate_bumps, bump_version, effective_bumps, VersionRepository};
use semver::Prerelease;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Complete projection-only stamp mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StampResult {
    /// Changed injected projections.
    pub writes: Vec<FileWrite>,
}

impl StampResult {
    /// Compute all injected projection writes.
    pub fn build(root: &Path, prerelease: Option<&str>) -> Result<Self> {
        let config = Config::load(root)?;
        let intents = Intent::load_all(root, &config)?;
        let declared = aggregate_bumps(intents.iter().map(|intent| &intent.release_units));
        let bumps = effective_bumps(&config, &declared);
        let repository = VersionRepository::discover(root)?;
        let mut writes = BTreeMap::<PathBuf, String>::new();
        let mut original = BTreeMap::<PathBuf, String>::new();
        let mut workspace_versions = BTreeMap::<PathBuf, String>::new();

        for (id, release_unit) in &config.release_units {
            let (_, primary) = config.primary_tag(id)?;
            let current = repository.current_version(id, &primary.template)?;
            let mut version = bump_version(&current, bumps[id]);
            if let Some(identifier) = prerelease {
                let height = repository.height(id, &primary.template)?;
                version.pre = Prerelease::new(&format!("{identifier}.{height}"))?;
            }
            for projection in release_unit
                .projections
                .iter()
                .filter(|projection| projection.mode == ProjectionMode::Injected)
            {
                let relative = release_unit.path.join(&projection.file);
                let text = read(root, &writes, &mut original, &relative)?;
                let edited = match projection.adapter {
                    Adapter::Npm => NpmAdapter.edit_version(&text, &version.to_string())?,
                    Adapter::Cargo => match CargoAdapter.version(&text)? {
                        Some(_) => CargoAdapter.edit_version(&text, &version.to_string())?,
                        None => {
                            let workspace = workspace_manifest(root, &relative)?;
                            if let Some(existing) = workspace_versions.get(&workspace) {
                                if existing != &version.to_string() {
                                    return Err(Error::Validation(format!(
                                        "Cargo workspace stamp requires conflicting versions {existing} and {version}"
                                    )));
                                }
                            }
                            workspace_versions.insert(workspace.clone(), version.to_string());
                            let workspace_text = read(root, &writes, &mut original, &workspace)?;
                            let workspace_edited = CargoAdapter
                                .edit_workspace_version(&workspace_text, &version.to_string())?;
                            writes.insert(workspace, workspace_edited);
                            text
                        }
                    },
                    Adapter::Pub => PubAdapter.edit_version(&text, &version.to_string())?,
                    Adapter::Python => PythonAdapter.edit_version(&text, &version.to_string())?,
                    Adapter::Msbuild => MsbuildAdapter.edit_version(&text, &version.to_string())?,
                    Adapter::Json => JsonFormat.edit_text(
                        &text,
                        projection.pointer.as_deref().expect("validated pointer"),
                        &version.to_string(),
                    )?,
                    Adapter::Toml => TomlFormat.edit_text(
                        &text,
                        projection.pointer.as_deref().expect("validated pointer"),
                        &version.to_string(),
                    )?,
                    Adapter::Yaml => YamlFormat.edit_text(
                        &text,
                        projection.pointer.as_deref().expect("validated pointer"),
                        &version.to_string(),
                    )?,
                    Adapter::Go => text,
                };
                writes.insert(relative, edited);
            }
        }

        let writes = writes
            .into_iter()
            .filter(|(path, contents)| original.get(path) != Some(contents))
            .map(|(path, contents)| FileWrite { path, contents })
            .collect();
        Ok(Self { writes })
    }

    /// Human-readable operations printed identically for dry and real runs.
    pub fn operations(&self) -> Vec<String> {
        self.writes
            .iter()
            .map(|write| format!("write {}", write.path.display()))
            .collect()
    }

    /// Materialize projection writes unless `dry_run` is enabled.
    pub fn apply(&self, root: &Path, dry_run: bool) -> Result<()> {
        if dry_run {
            return Ok(());
        }
        for write in &self.writes {
            let path = root.join(&write.path);
            std::fs::write(&path, &write.contents).map_err(|error| Error::io(&path, error))?;
        }
        Ok(())
    }
}

fn read(
    root: &Path,
    writes: &BTreeMap<PathBuf, String>,
    original: &mut BTreeMap<PathBuf, String>,
    relative: &Path,
) -> Result<String> {
    if let Some(text) = writes.get(relative) {
        return Ok(text.clone());
    }
    let path = root.join(relative);
    let text = std::fs::read_to_string(&path).map_err(|error| Error::io(&path, error))?;
    original
        .entry(relative.to_owned())
        .or_insert_with(|| text.clone());
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prerelease_composes_identifier_and_height() {
        let mut version = semver::Version::new(1, 3, 0);
        version.pre = Prerelease::new(&format!("{}.{}", "alpha", 5)).expect("prerelease");
        assert_eq!(version.to_string(), "1.3.0-alpha.5");
    }
}
