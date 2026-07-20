// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Workspace configuration and validation.

use crate::error::{Error, Result};
use crate::model::{Adapter, Bump, ProjectionMode};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// Location of the workspace configuration.
pub const CONFIG_PATH: &str = ".intentional/config.yml";

/// Published configuration schema identifier.
pub const CONFIG_SCHEMA: &str = "https://intentional.foo/schemas/config.yml";

/// Complete workspace configuration.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Config {
    /// Optional schema URL for editor and validation tooling.
    #[serde(rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Workspace-wide release settings.
    #[serde(default)]
    pub settings: Settings,
    /// Logical package inventory keyed by stable package id.
    pub packages: BTreeMap<String, PackageConfig>,
}

/// Workspace-wide release behavior.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Settings {
    /// Create an additional plain `X.Y.Z` tag.
    #[serde(default)]
    pub global_tag: bool,
    /// Minimum bump propagated to internal dependents.
    #[serde(default = "default_dependency_bump")]
    pub internal_dependency_bump: Bump,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            global_tag: false,
            internal_dependency_bump: default_dependency_bump(),
        }
    }
}

const fn default_dependency_bump() -> Bump {
    Bump::Patch
}

/// One logical package, potentially projected into several ecosystems.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PackageConfig {
    /// Package directory relative to the workspace root.
    pub path: PathBuf,
    /// Version-bearing ecosystem and format projections.
    pub projections: Vec<Projection>,
    /// Tag template. Defaults to `{id}@{version}`.
    #[serde(default = "default_tag_template")]
    pub tag: String,
    /// Authored internal dependency edges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
}

fn default_tag_template() -> String {
    "{id}@{version}".to_owned()
}

/// A version projection into a manifest or arbitrary file.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Projection {
    /// Adapter specialization.
    pub adapter: Adapter,
    /// File relative to the logical package path.
    pub file: PathBuf,
    /// Projection materialization mode.
    pub mode: ProjectionMode,
    /// JSON Pointer or dotted TOML/YAML key path for generic formats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
}

impl Config {
    /// Load and validate configuration rooted at `root`.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(CONFIG_PATH);
        let text = std::fs::read_to_string(&path).map_err(|error| Error::io(&path, error))?;
        Self::from_yaml(&text)
    }

    /// Parse and validate configuration YAML.
    pub fn from_yaml(text: &str) -> Result<Self> {
        let config: Self = serde_yaml::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    /// Serialize configuration deterministically.
    pub fn to_yaml(&self) -> Result<String> {
        self.validate()?;
        Ok(serde_yaml::to_string(self)?)
    }

    /// Validate package ids, paths, projections, tag templates, and dependency graph.
    pub fn validate(&self) -> Result<()> {
        if self.packages.is_empty() {
            return Err(Error::Validation(
                "config must declare at least one package".to_owned(),
            ));
        }
        if self.settings.internal_dependency_bump == Bump::None {
            return Err(Error::Validation(
                "internal-dependency-bump must be major, minor, or patch".to_owned(),
            ));
        }

        for (id, package) in &self.packages {
            validate_id(id)?;
            validate_relative_path(&package.path, &format!("package {id} path"))?;
            if package.projections.is_empty() {
                return Err(Error::Validation(format!(
                    "package {id} must declare at least one projection"
                )));
            }
            validate_tag_template(id, &package.tag)?;

            let mut projection_keys = BTreeSet::new();
            for projection in &package.projections {
                validate_relative_path(&projection.file, &format!("package {id} projection file"))?;
                if projection.adapter.requires_pointer()
                    && projection.pointer.as_deref().is_none_or(str::is_empty)
                {
                    return Err(Error::Validation(format!(
                        "package {id} generic {:?} projection requires a pointer",
                        projection.adapter
                    )));
                }
                let key = (&projection.file, projection.pointer.as_deref());
                if !projection_keys.insert(key) {
                    return Err(Error::Validation(format!(
                        "package {id} repeats projection {}",
                        projection.file.display()
                    )));
                }
            }

            let mut dependencies = BTreeSet::new();
            for dependency in &package.depends_on {
                if dependency == id {
                    return Err(Error::Validation(format!(
                        "package {id} cannot depend on itself"
                    )));
                }
                if !self.packages.contains_key(dependency) {
                    return Err(Error::Validation(format!(
                        "package {id} depends on unknown package {dependency}"
                    )));
                }
                if !dependencies.insert(dependency) {
                    return Err(Error::Validation(format!(
                        "package {id} repeats dependency {dependency}"
                    )));
                }
            }
        }

        let mut resolved_tag_patterns = BTreeMap::new();
        if self.settings.global_tag {
            resolved_tag_patterns.insert("{version}".to_owned(), "the global tag".to_owned());
        }
        for (id, package) in &self.packages {
            let pattern = package.tag.replace("{id}", id);
            if let Some(other) = resolved_tag_patterns.insert(pattern, format!("package {id}")) {
                return Err(Error::Validation(format!(
                    "package {id} tag template collides with {other}"
                )));
            }
        }

        self.validate_acyclic()
    }

    fn validate_acyclic(&self) -> Result<()> {
        fn visit<'a>(
            id: &'a str,
            packages: &'a BTreeMap<String, PackageConfig>,
            visiting: &mut BTreeSet<&'a str>,
            visited: &mut BTreeSet<&'a str>,
        ) -> Result<()> {
            if visited.contains(id) {
                return Ok(());
            }
            if !visiting.insert(id) {
                return Err(Error::Validation(format!(
                    "dependency cycle includes package {id}"
                )));
            }
            for dependency in &packages[id].depends_on {
                visit(dependency, packages, visiting, visited)?;
            }
            visiting.remove(id);
            visited.insert(id);
            Ok(())
        }

        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        for id in self.packages.keys() {
            visit(id, &self.packages, &mut visiting, &mut visited)?;
        }
        Ok(())
    }
}

fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.@/".contains(character))
    {
        return Err(Error::Validation(format!("invalid package id {id:?}")));
    }
    Ok(())
}

fn validate_relative_path(path: &Path, description: &str) -> Result<()> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(Error::Validation(format!(
            "{description} must be a non-empty relative path without .."
        )));
    }
    Ok(())
}

fn validate_tag_template(id: &str, template: &str) -> Result<()> {
    if template.matches("{version}").count() != 1 || template.matches("{id}").count() > 1 {
        return Err(Error::Validation(format!(
            "package {id} tag template must contain exactly one {{version}} and at most one {{id}}"
        )));
    }
    if template.contains("v{version}") {
        return Err(Error::Validation(format!(
            "package {id} tag template must not prefix versions with v"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
$schema: https://intentional.foo/schemas/config.yml
settings:
  global-tag: true
  internal-dependency-bump: patch
packages:
  library:
    path: packages/library
    projections:
      - adapter: npm
        file: package.json
        mode: committed
  application:
    path: packages/application
    depends-on: [library]
    projections:
      - adapter: json
        file: metadata.json
        pointer: /version
        mode: injected
"#;

    #[test]
    fn parses_and_defaults_tag_templates() {
        let config = Config::from_yaml(VALID).expect("valid config");
        assert_eq!(config.packages["library"].tag, "{id}@{version}");
        assert!(config.settings.global_tag);
    }

    #[test]
    fn rejects_unknown_dependencies() {
        let invalid = VALID.replace("depends-on: [library]", "depends-on: [missing]");
        let error = Config::from_yaml(&invalid).expect_err("unknown package rejected");
        assert!(error.to_string().contains("unknown package missing"));
    }

    #[test]
    fn rejects_dependency_cycles() {
        let invalid = VALID.replace(
            "path: packages/library",
            "path: packages/library\n    depends-on: [application]",
        );
        let error = Config::from_yaml(&invalid).expect_err("cycle rejected");
        assert!(error.to_string().contains("dependency cycle"));
    }

    #[test]
    fn accepts_plain_version_tag_template() {
        let valid = VALID
            .replace("global-tag: true", "global-tag: false")
            .replace(
                "path: packages/library",
                "path: packages/library\n    tag: '{version}'",
            );
        let config = Config::from_yaml(&valid).expect("plain version template accepted");
        assert_eq!(config.packages["library"].tag, "{version}");
    }

    #[test]
    fn rejects_colliding_tag_templates() {
        let invalid = VALID
            .replace("global-tag: true", "global-tag: false")
            .replace(
                "path: packages/library",
                "path: packages/library\n    tag: 'shared@{version}'",
            )
            .replace(
                "path: packages/application",
                "path: packages/application\n    tag: 'shared@{version}'",
            );
        let error = Config::from_yaml(&invalid).expect_err("collision rejected");
        assert!(error.to_string().contains("collides"));
    }

    #[test]
    fn rejects_plain_version_template_colliding_with_global_tag() {
        let invalid = VALID.replace(
            "path: packages/library",
            "path: packages/library\n    tag: '{version}'",
        );
        let error = Config::from_yaml(&invalid).expect_err("global tag collision rejected");
        assert!(error.to_string().contains("collides with the global tag"));
    }

    #[test]
    fn rejects_v_prefixed_tag_templates() {
        let invalid = VALID.replace(
            "path: packages/library",
            "path: packages/library\n    tag: '{id}@v{version}'",
        );
        let error = Config::from_yaml(&invalid).expect_err("v prefix rejected");
        assert!(error
            .to_string()
            .contains("must not prefix versions with v"));
    }

    #[test]
    fn generic_formats_require_pointers() {
        let invalid = VALID.replace("        pointer: /version\n", "");
        let error = Config::from_yaml(&invalid).expect_err("pointer required");
        assert!(error.to_string().contains("requires a pointer"));
    }
}
