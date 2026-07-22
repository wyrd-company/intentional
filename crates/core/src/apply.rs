// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Tree-only release materialization.

use crate::adapters::{
    CargoAdapter, FormatAdapter, GoAdapter, JsonFormat, MsbuildAdapter, NpmAdapter, PubAdapter,
    PythonAdapter, TomlFormat, YamlFormat,
};
use crate::config::{Config, Projection, ReleaseUnitConfig};
use crate::error::{Error, Result};
use crate::intent::Intent;
use crate::model::{Adapter, ProjectionMode};
use crate::plan::{PlanReleaseUnit, ReleasePlan};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One planned file write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileWrite {
    /// Workspace-relative target.
    pub path: PathBuf,
    /// Complete new contents.
    pub contents: String,
}

/// Complete tree mutation produced by `apply`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyResult {
    /// Changed or created files in path order.
    pub writes: Vec<FileWrite>,
    /// Consumed intent files in path order.
    pub deletes: Vec<PathBuf>,
    /// Prominent operational notices.
    pub notices: Vec<String>,
    /// Canonical plan materialized by these changes.
    pub plan: ReleasePlan,
}

impl ApplyResult {
    /// Plan all release tree mutations.
    pub fn build(root: &Path, channel: Option<&str>) -> Result<Self> {
        let config = Config::load(root)?;
        let intents = Intent::load_all(root, &config)?;
        let plan = ReleasePlan::from_inputs(root, &config, &intents, channel)?;
        let mut tree = WorkingTree::new(root);
        let by_id: BTreeMap<_, _> = plan
            .release_units
            .iter()
            .map(|release_unit| (release_unit.id.as_str(), release_unit))
            .collect();
        let mut workspace_versions = BTreeMap::new();
        let mut notices = Vec::new();

        for release_unit in &plan.release_units {
            let config_release_unit = &config.release_units[&release_unit.id];
            for projection in &config_release_unit.projections {
                let go_major = projection.adapter == Adapter::Go
                    && release_unit.bump == crate::model::Bump::Major;
                if projection.mode != ProjectionMode::Committed && !go_major {
                    continue;
                }
                edit_projection_version(
                    &mut tree,
                    root,
                    config_release_unit,
                    projection,
                    &release_unit.new_version,
                    &mut workspace_versions,
                )?;
                if go_major {
                    notices.push(format!(
                        "rewrite Go module major path {}",
                        config_release_unit.path.join(&projection.file).display()
                    ));
                }
            }
            let changelog = config_release_unit.path.join("CHANGELOG.md");
            let current = tree.read_optional(&changelog)?;
            let next = update_changelog(
                current.as_deref(),
                &release_unit.release_notes,
                &release_unit.new_version,
                channel.is_none(),
            );
            tree.set(changelog, next);
        }

        rewrite_internal_dependencies(&mut tree, root, &config, &by_id)?;

        let deletes = if channel.is_none() {
            intents
                .iter()
                .map(|intent| {
                    intent
                        .path
                        .strip_prefix(root)
                        .unwrap_or(&intent.path)
                        .to_owned()
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok(Self {
            writes: tree.finish(),
            deletes,
            notices,
            plan,
        })
    }

    /// Human-readable operations printed identically for dry and real runs.
    pub fn operations(&self) -> Vec<String> {
        self.notices
            .iter()
            .cloned()
            .chain(
                self.writes
                    .iter()
                    .map(|write| format!("write {}", write.path.display())),
            )
            .chain(
                self.deletes
                    .iter()
                    .map(|path| format!("delete {}", path.display())),
            )
            .collect()
    }

    /// Materialize these changes unless `dry_run` is enabled.
    pub fn apply(&self, root: &Path, dry_run: bool) -> Result<()> {
        if dry_run {
            return Ok(());
        }
        for write in &self.writes {
            let path = root.join(&write.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
            }
            std::fs::write(&path, &write.contents).map_err(|error| Error::io(&path, error))?;
        }
        for relative in &self.deletes {
            let path = root.join(relative);
            std::fs::remove_file(&path).map_err(|error| Error::io(&path, error))?;
        }
        Ok(())
    }
}

struct WorkingTree<'a> {
    root: &'a Path,
    original: BTreeMap<PathBuf, Option<String>>,
    current: BTreeMap<PathBuf, String>,
}

impl<'a> WorkingTree<'a> {
    fn new(root: &'a Path) -> Self {
        Self {
            root,
            original: BTreeMap::new(),
            current: BTreeMap::new(),
        }
    }

    fn read(&mut self, relative: &Path) -> Result<String> {
        if let Some(text) = self.current.get(relative) {
            return Ok(text.clone());
        }
        let path = self.root.join(relative);
        let text = std::fs::read_to_string(&path).map_err(|error| Error::io(&path, error))?;
        self.original
            .entry(relative.to_owned())
            .or_insert_with(|| Some(text.clone()));
        self.current.insert(relative.to_owned(), text.clone());
        Ok(text)
    }

    fn read_optional(&mut self, relative: &Path) -> Result<Option<String>> {
        if let Some(text) = self.current.get(relative) {
            return Ok(Some(text.clone()));
        }
        let path = self.root.join(relative);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                self.original
                    .entry(relative.to_owned())
                    .or_insert_with(|| Some(text.clone()));
                self.current.insert(relative.to_owned(), text.clone());
                Ok(Some(text))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.original.entry(relative.to_owned()).or_insert(None);
                Ok(None)
            }
            Err(error) => Err(Error::io(path, error)),
        }
    }

    fn set(&mut self, relative: PathBuf, contents: String) {
        self.original.entry(relative.clone()).or_insert(None);
        self.current.insert(relative, contents);
    }

    fn finish(self) -> Vec<FileWrite> {
        self.current
            .into_iter()
            .filter(|(path, contents)| {
                self.original.get(path).and_then(Option::as_ref) != Some(contents)
            })
            .map(|(path, contents)| FileWrite { path, contents })
            .collect()
    }
}

fn edit_projection_version(
    tree: &mut WorkingTree<'_>,
    root: &Path,
    release_unit: &ReleaseUnitConfig,
    projection: &Projection,
    version: &str,
    workspace_versions: &mut BTreeMap<PathBuf, String>,
) -> Result<()> {
    let relative = release_unit.path.join(&projection.file);
    let text = tree.read(&relative)?;
    let edited = match projection.adapter {
        Adapter::Npm => NpmAdapter.edit_version(&text, version)?,
        Adapter::Cargo => match CargoAdapter.version(&text)? {
            Some(_) => CargoAdapter.edit_version(&text, version)?,
            None => {
                let workspace_manifest = workspace_manifest(root, &relative)?;
                if let Some(existing) = workspace_versions.get(&workspace_manifest) {
                    if existing != version {
                        return Err(Error::Validation(format!(
                            "Cargo workspace members require conflicting versions {existing} and {version}"
                        )));
                    }
                }
                workspace_versions.insert(workspace_manifest.clone(), version.to_owned());
                let workspace_text = tree.read(&workspace_manifest)?;
                let workspace_edited =
                    CargoAdapter.edit_workspace_version(&workspace_text, version)?;
                tree.set(workspace_manifest, workspace_edited);
                text
            }
        },
        Adapter::Json => JsonFormat.edit_text(
            &text,
            projection.pointer.as_deref().expect("validated pointer"),
            version,
        )?,
        Adapter::Toml => TomlFormat.edit_text(
            &text,
            projection.pointer.as_deref().expect("validated pointer"),
            version,
        )?,
        Adapter::Yaml => YamlFormat.edit_text(
            &text,
            projection.pointer.as_deref().expect("validated pointer"),
            version,
        )?,
        Adapter::Pub => PubAdapter.edit_version(&text, version)?,
        Adapter::Python => PythonAdapter.edit_version(&text, version)?,
        Adapter::Msbuild => MsbuildAdapter.edit_version(&text, version)?,
        Adapter::Go => GoAdapter.edit_major_module_path(&text, version)?,
    };
    tree.set(relative, edited);
    Ok(())
}

fn rewrite_internal_dependencies(
    tree: &mut WorkingTree<'_>,
    root: &Path,
    config: &Config,
    released: &BTreeMap<&str, &PlanReleaseUnit>,
) -> Result<()> {
    for (dependent_id, dependent) in &config.release_units {
        if !released.contains_key(dependent_id.as_str()) {
            continue;
        }
        for dependency_id in &dependent.depends_on {
            let Some(dependency_release) = released.get(dependency_id.as_str()) else {
                continue;
            };
            let dependency = &config.release_units[dependency_id];
            for adapter in [
                Adapter::Npm,
                Adapter::Cargo,
                Adapter::Pub,
                Adapter::Python,
                Adapter::Msbuild,
                Adapter::Go,
            ] {
                let Some(source_projection) = dependency
                    .projections
                    .iter()
                    .find(|projection| projection.adapter == adapter)
                else {
                    continue;
                };
                let Some(target_projection) = dependent.projections.iter().find(|projection| {
                    projection.adapter == adapter && projection.mode == ProjectionMode::Committed
                }) else {
                    continue;
                };
                let source_path = dependency.path.join(&source_projection.file);
                let source_text = tree.read(&source_path)?;
                let dependency_name = match adapter {
                    Adapter::Npm => NpmAdapter.name(&source_text)?,
                    Adapter::Cargo => CargoAdapter.name(&source_text)?,
                    Adapter::Pub => PubAdapter.name(&source_text)?,
                    Adapter::Python => PythonAdapter.name(&source_text)?,
                    Adapter::Msbuild => MsbuildAdapter
                        .name(&source_text)
                        .unwrap_or_else(|_| dependency_id.clone()),
                    Adapter::Go => GoAdapter.name(&source_text)?,
                    _ => unreachable!(),
                };
                let target_path = dependent.path.join(&target_projection.file);
                let target_text = tree.read(&target_path)?;
                let edited = match adapter {
                    Adapter::Npm => NpmAdapter.edit_dependency(
                        &target_text,
                        &dependency_name,
                        &dependency_release.new_version,
                    )?,
                    Adapter::Cargo => {
                        if CargoAdapter.dependency_is_inherited(&target_text, &dependency_name)? {
                            let workspace = workspace_manifest(root, &target_path)?;
                            let workspace_text = tree.read(&workspace)?;
                            let workspace_edited = CargoAdapter.edit_dependency(
                                &workspace_text,
                                &dependency_name,
                                &dependency_release.new_version,
                            )?;
                            tree.set(workspace, workspace_edited);
                            target_text
                        } else {
                            CargoAdapter.edit_dependency(
                                &target_text,
                                &dependency_name,
                                &dependency_release.new_version,
                            )?
                        }
                    }
                    Adapter::Pub => PubAdapter.edit_dependency(
                        &target_text,
                        &dependency_name,
                        &dependency_release.new_version,
                    )?,
                    Adapter::Python => PythonAdapter.edit_dependency(
                        &target_text,
                        &dependency_name,
                        &dependency_release.new_version,
                    )?,
                    Adapter::Msbuild => MsbuildAdapter.edit_dependency(
                        &target_text,
                        &dependency_name,
                        &dependency_release.new_version,
                    )?,
                    Adapter::Go => GoAdapter.edit_dependency(
                        &target_text,
                        &dependency_name,
                        &dependency_release.new_version,
                    )?,
                    _ => unreachable!(),
                };
                tree.set(target_path, edited);
            }
        }
    }
    Ok(())
}

pub(crate) fn workspace_manifest(root: &Path, member: &Path) -> Result<PathBuf> {
    let absolute_member = root.join(member);
    let mut directory = absolute_member.parent();
    while let Some(candidate) = directory {
        let manifest = candidate.join("Cargo.toml");
        if manifest.exists() {
            let text =
                std::fs::read_to_string(&manifest).map_err(|error| Error::io(&manifest, error))?;
            if text
                .parse::<toml_edit::DocumentMut>()
                .ok()
                .and_then(|document| document.get("workspace").cloned())
                .is_some()
            {
                return manifest
                    .strip_prefix(root)
                    .map(Path::to_owned)
                    .map_err(|error| Error::Validation(error.to_string()));
            }
        }
        if candidate == root {
            break;
        }
        directory = candidate.parent();
    }
    Err(Error::Validation(format!(
        "could not find Cargo workspace manifest for {}",
        member.display()
    )))
}

fn update_changelog(
    existing: Option<&str>,
    section: &str,
    version: &str,
    final_release: bool,
) -> String {
    let existing = existing.unwrap_or("# Changelog\n");
    let mut boundaries = existing
        .match_indices("\n## ")
        .map(|(index, _)| index + 1)
        .collect::<Vec<_>>();
    if existing.starts_with("## ") {
        boundaries.insert(0, 0);
    }
    let preamble_end = boundaries.first().copied().unwrap_or(existing.len());
    let mut kept = Vec::new();
    for (index, start) in boundaries.iter().enumerate() {
        let end = boundaries.get(index + 1).copied().unwrap_or(existing.len());
        let candidate = &existing[*start..end];
        let heading = candidate.lines().next().unwrap_or_default();
        let exact = heading == format!("## {version}");
        let prerelease = final_release && heading.starts_with(&format!("## {version}-"));
        if !exact && !prerelease {
            kept.push(candidate.trim());
        }
    }
    let mut output = String::new();
    output.push_str(existing[..preamble_end].trim_end());
    output.push_str("\n\n");
    output.push_str(section.trim());
    output.push('\n');
    for candidate in kept {
        output.push('\n');
        output.push_str(candidate);
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn final_changelog_replaces_channel_sections() {
        let existing = "# Changelog\n\n## 1.2.0-beta.2\n\nOld beta.\n\n## 1.1.0\n\nPrior.\n";
        let output = update_changelog(
            Some(existing),
            "## 1.2.0\n\n### Features\n\n- Final.",
            "1.2.0",
            true,
        );
        assert!(output.contains("## 1.2.0\n"));
        assert!(!output.contains("beta.2"));
        assert!(output.contains("## 1.1.0"));
    }

    #[test]
    fn new_changelog_has_lintable_spacing() {
        let output = update_changelog(
            None,
            "## 1.0.1\n\n### Fixes\n\n- Correct a defect.",
            "1.0.1",
            true,
        );
        assert_eq!(
            output,
            "# Changelog\n\n## 1.0.1\n\n### Fixes\n\n- Correct a defect.\n"
        );
    }
}
