// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Manifest discovery for `intentional init`.

use crate::config::{Config, PackageConfig, Projection, Settings, CONFIG_SCHEMA};
use crate::error::{Error, Result};
use crate::model::{Adapter, ProjectionMode};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

/// Planned initialization output.
#[derive(Debug, Clone)]
pub struct InitResult {
    /// Discovered configuration.
    pub config: Config,
    /// Target config path relative to the workspace.
    pub path: PathBuf,
    /// Serialized config contents.
    pub contents: String,
}

impl InitResult {
    /// Write the configuration and intent directory unless this is a dry run.
    pub fn apply(&self, root: &Path, dry_run: bool) -> Result<()> {
        if dry_run {
            return Ok(());
        }
        let path = root.join(&self.path);
        if path.exists() {
            return Err(Error::Validation(format!(
                "configuration already exists at {}",
                self.path.display()
            )));
        }
        let parent = path.parent().expect("config path has a parent");
        std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
        std::fs::write(&path, &self.contents).map_err(|error| Error::io(&path, error))?;
        std::fs::create_dir_all(root.join(crate::intent::INTENTS_PATH))
            .map_err(|error| Error::io(root.join(crate::intent::INTENTS_PATH), error))
    }
}

/// Scan supported manifests and build a logical package inventory.
pub fn discover_config(root: &Path) -> Result<InitResult> {
    let mut packages: BTreeMap<String, PackageConfig> = BTreeMap::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(should_visit)
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let Some(adapter) = adapter_for(entry.path()) else {
            continue;
        };
        if !is_project_manifest(entry.path(), adapter)? {
            continue;
        }
        let directory = entry.path().parent().expect("manifest has parent");
        let relative_directory = directory.strip_prefix(root).map_err(|error| {
            Error::Validation(format!("manifest is outside workspace: {error}"))
        })?;
        let id = if relative_directory.as_os_str().is_empty() {
            root.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("root")
                .to_owned()
        } else {
            directory
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| Error::Validation("non-Unicode package directory".to_owned()))?
                .to_owned()
        };
        let package = packages.entry(id.clone()).or_insert_with(|| PackageConfig {
            path: if relative_directory.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                relative_directory.to_owned()
            },
            projections: Vec::new(),
            tag: "{id}@{version}".to_owned(),
            depends_on: Vec::new(),
        });
        if package.path != relative_directory && package.path != Path::new(".") {
            return Err(Error::Validation(format!(
                "multiple package directories have id {id}; edit ids explicitly"
            )));
        }
        let file = entry
            .path()
            .file_name()
            .map(PathBuf::from)
            .expect("manifest has filename");
        package.projections.push(Projection {
            adapter,
            file,
            mode: if adapter == Adapter::Go {
                ProjectionMode::None
            } else {
                ProjectionMode::Committed
            },
            pointer: None,
        });
    }
    if packages.is_empty() {
        return Err(Error::Validation("no supported manifests found".to_owned()));
    }
    let config = Config {
        schema: Some(CONFIG_SCHEMA.to_owned()),
        settings: Settings::default(),
        packages,
    };
    let contents = config.to_yaml()?;
    Ok(InitResult {
        config,
        path: PathBuf::from(crate::config::CONFIG_PATH),
        contents,
    })
}

fn should_visit(entry: &DirEntry) -> bool {
    !matches!(
        entry.file_name().to_str(),
        Some(".git" | ".intentional" | "node_modules" | "target")
    )
}

fn adapter_for(path: &Path) -> Option<Adapter> {
    let name = path.file_name()?.to_str()?;
    match name {
        "package.json" => Some(Adapter::Npm),
        "Cargo.toml" => Some(Adapter::Cargo),
        "pubspec.yaml" => Some(Adapter::Pub),
        "pyproject.toml" => Some(Adapter::Python),
        "go.mod" => Some(Adapter::Go),
        name if name.ends_with(".csproj") => Some(Adapter::Msbuild),
        _ => None,
    }
}

fn is_project_manifest(path: &Path, adapter: Adapter) -> Result<bool> {
    if adapter != Adapter::Cargo {
        return Ok(true);
    }
    let text = std::fs::read_to_string(path).map_err(|error| Error::io(path, error))?;
    let document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| Error::Validation(format!("invalid Cargo.toml: {error}")))?;
    Ok(document.get("package").is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_supported_manifests() {
        assert_eq!(adapter_for(Path::new("package.json")), Some(Adapter::Npm));
        assert_eq!(
            adapter_for(Path::new("module.csproj")),
            Some(Adapter::Msbuild)
        );
        assert_eq!(adapter_for(Path::new("README.md")), None);
    }
}
