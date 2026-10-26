// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Workspace configuration and validation.

use crate::error::{Error, Result};
use crate::model::{
    Adapter, Bump, Pre1BumpMapping, ProjectionMode, ReleaseUnitDisposition, TagPhase, TagRole,
};
use crate::ownership::check_ownership;
use crate::projection_file::validate_relative_path;
pub use crate::projection_file::ProjectionFile;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// Location of the workspace configuration.
pub const CONFIG_PATH: &str = ".intentional/config.yml";

/// Published configuration schema identifier.
pub const CONFIG_SCHEMA: &str = "https://intentional.foo/schemas/config.yml";

/// Current interpretation contract written by initialization.
pub const CURRENT_CONTRACT: &str = "contract-1";

/// Complete workspace configuration.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Config {
    /// Optional schema URL for editor and validation tooling.
    #[serde(rename = "$schema", skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Versioned release-semantics contract.
    pub contract: String,
    /// Workspace-wide release settings.
    #[serde(default)]
    pub settings: Settings,
    /// Fixed release groups using Changesets semantics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixed: Vec<Vec<String>>,
    /// Linked release groups using Changesets semantics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked: Vec<Vec<String>>,
    /// Repository-level release tag streams.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub workspace_tags: BTreeMap<String, WorkspaceTagConfig>,
    /// Receipts for discovery candidates already resolved by initialization.
    #[serde(default, skip_serializing_if = "DiscoveryConfig::is_empty")]
    pub discovery: DiscoveryConfig,
    /// Release-unit inventory keyed by stable release-unit id.
    pub release_units: BTreeMap<String, ReleaseUnitConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema: Some(CONFIG_SCHEMA.to_owned()),
            contract: CURRENT_CONTRACT.to_owned(),
            settings: Settings::default(),
            fixed: Vec::new(),
            linked: Vec::new(),
            workspace_tags: BTreeMap::new(),
            discovery: DiscoveryConfig::default(),
            release_units: BTreeMap::new(),
        }
    }
}

/// Durable receipts for resolved discovery candidates.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DiscoveryConfig {
    /// Candidate paths incorporated into configured release units.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub managed_paths: Vec<ManagedPathReceipt>,
    /// Candidate paths explicitly excluded at an exact evidence digest.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_paths: Vec<ExcludedPathReceipt>,
}

impl DiscoveryConfig {
    fn is_empty(&self) -> bool {
        self.managed_paths.is_empty() && self.excluded_paths.is_empty()
    }
}

/// Receipt connecting one detector/path identity to a configured release unit.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ManagedPathReceipt {
    /// Stable detector id.
    pub detector: String,
    /// Exact workspace-relative candidate path.
    pub path: PathBuf,
    /// Configured release unit that owns the candidate projection.
    pub release_unit: String,
}

/// Receipt excluding one detector/path identity while its evidence is unchanged.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExcludedPathReceipt {
    /// Stable detector id.
    pub detector: String,
    /// Exact workspace-relative candidate path.
    pub path: PathBuf,
    /// SHA-256 digest of the evidence at the time of exclusion.
    pub evidence_digest: String,
}

/// Workspace-wide release behavior.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Settings {
    /// Minimum bump propagated to internal dependents.
    #[serde(default = "default_dependency_bump")]
    pub internal_dependency_bump: Bump,
    /// Interpretation of bump names before semantic version 1.0.0.
    #[serde(default)]
    pub pre_1_0_bump_mapping: Pre1BumpMapping,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            internal_dependency_bump: default_dependency_bump(),
            pre_1_0_bump_mapping: Pre1BumpMapping::default(),
        }
    }
}

const fn default_dependency_bump() -> Bump {
    Bump::Patch
}

/// One independently versioned release unit.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ReleaseUnitConfig {
    /// Release-unit directory relative to the workspace root.
    pub path: PathBuf,
    /// Whether releases may include this release unit.
    #[serde(default, skip_serializing_if = "is_managed")]
    pub disposition: ReleaseUnitDisposition,
    /// Version-bearing ecosystem and format projections. Empty means tag-only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub projections: Vec<Projection>,
    /// Named release-unit tag streams. Exactly one is primary.
    pub tags: BTreeMap<String, TagConfig>,
    /// Authored internal dependency edges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
}

fn is_managed(disposition: &ReleaseUnitDisposition) -> bool {
    *disposition == ReleaseUnitDisposition::Managed
}

/// One named release-unit tag.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct TagConfig {
    /// Whether this tag supplies version authority or projects it.
    pub role: TagRole,
    /// Tag name template containing `{version}` and optionally `{id}`.
    pub template: String,
    /// Optional executor phase declaration required for creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_phase: Option<TagPhase>,
    /// Observable tag prerequisites expressed as canonical tag ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tag_after: Vec<String>,
}

/// One named workspace-level tag.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WorkspaceTagConfig {
    /// Tag name template containing `{version}`.
    pub template: String,
    /// Optional executor phase declaration required for creation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_phase: Option<TagPhase>,
    /// Observable tag prerequisites expressed as canonical tag ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tag_after: Vec<String>,
}

/// A version projection into a manifest or arbitrary file.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Projection {
    /// Adapter specialization.
    pub adapter: Adapter,
    /// File relative to the release-unit path.
    pub file: ProjectionFile,
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

    /// Return the configured primary tag for a release unit.
    pub fn primary_tag<'a>(&'a self, release_unit_id: &str) -> Result<(&'a str, &'a TagConfig)> {
        let release_unit = self
            .release_units
            .get(release_unit_id)
            .ok_or_else(|| Error::Validation(format!("unknown release unit {release_unit_id}")))?;
        release_unit
            .tags
            .iter()
            .find(|(_, tag)| tag.role == TagRole::Primary)
            .map(|(id, tag)| (id.as_str(), tag))
            .ok_or_else(|| {
                Error::Validation(format!("release unit {release_unit_id} has no primary tag"))
            })
    }

    /// Canonical id for a named release-unit tag.
    pub fn release_unit_tag_id(release_unit_id: &str, tag_id: &str) -> String {
        format!("release-unit/{release_unit_id}/{tag_id}")
    }

    /// Canonical id for a named workspace tag.
    pub fn workspace_tag_id(tag_id: &str) -> String {
        format!("workspace/{tag_id}")
    }

    /// Validate release-unit ids, projections, release groups, tags, and dependency graphs.
    pub fn validate(&self) -> Result<()> {
        if self.contract != CURRENT_CONTRACT {
            return Err(Error::Validation(format!(
                "unsupported interpretation contract {:?}; expected {CURRENT_CONTRACT}",
                self.contract
            )));
        }
        if self.release_units.is_empty() {
            return Err(Error::Validation(
                "config must declare at least one release unit".to_owned(),
            ));
        }
        if self.settings.internal_dependency_bump == Bump::None {
            return Err(Error::Validation(
                "internal-dependency-bump must be major, minor, or patch".to_owned(),
            ));
        }

        let mut canonical_tags = BTreeSet::new();
        let mut resolved_templates = BTreeMap::new();
        for (id, release_unit) in &self.release_units {
            validate_id(id, "release unit")?;
            validate_relative_path(&release_unit.path, &format!("release unit {id} path"))?;
            let primary_count = release_unit
                .tags
                .values()
                .filter(|tag| tag.role == TagRole::Primary)
                .count();
            if primary_count != 1 {
                return Err(Error::Validation(format!(
                    "release unit {id} must declare exactly one primary tag"
                )));
            }

            for projection in &release_unit.projections {
                projection.file.validate(id, projection.adapter)?;
                if projection.adapter.requires_pointer()
                    && projection.pointer.as_deref().is_none_or(str::is_empty)
                {
                    return Err(Error::Validation(format!(
                        "release unit {id} generic {:?} projection requires a pointer",
                        projection.adapter
                    )));
                }
            }

            validate_dependencies(id, release_unit, &self.release_units)?;
            for (tag_id, tag) in &release_unit.tags {
                validate_id(tag_id, "tag")?;
                validate_tag_template(
                    &format!("release unit {id} tag {tag_id}"),
                    &tag.template,
                    true,
                )?;
                let canonical = Self::release_unit_tag_id(id, tag_id);
                canonical_tags.insert(canonical);
                let rendered = tag.template.replace("{id}", id);
                if let Some(other) =
                    resolved_templates.insert(rendered, format!("release unit {id} tag {tag_id}"))
                {
                    return Err(Error::Validation(format!(
                        "release unit {id} tag {tag_id} template collides with {other}"
                    )));
                }
            }
        }

        for (id, tag) in &self.workspace_tags {
            validate_id(id, "workspace tag")?;
            validate_tag_template(&format!("workspace tag {id}"), &tag.template, false)?;
            canonical_tags.insert(Self::workspace_tag_id(id));
            if let Some(other) =
                resolved_templates.insert(tag.template.clone(), format!("workspace tag {id}"))
            {
                return Err(Error::Validation(format!(
                    "workspace tag {id} template collides with {other}"
                )));
            }
        }

        check_ownership(&self.release_units)?;
        self.validate_discovery()?;
        self.validate_release_groups()?;
        self.validate_dependency_acyclic()?;
        self.validate_tag_graph(&canonical_tags)
    }

    fn validate_discovery(&self) -> Result<()> {
        let mut identities = BTreeSet::new();
        for receipt in &self.discovery.managed_paths {
            validate_detector_id(&receipt.detector)?;
            validate_exact_discovery_path(&receipt.path, "managed discovery path")?;
            validate_id(&receipt.release_unit, "managed discovery release unit")?;
            if !self.release_units.contains_key(&receipt.release_unit) {
                return Err(Error::Validation(format!(
                    "managed discovery path {} references unknown release unit {}",
                    receipt.path.display(),
                    receipt.release_unit
                )));
            }
            if !identities.insert((&receipt.detector, &receipt.path)) {
                return Err(Error::Validation(format!(
                    "duplicate discovery receipt for detector {} at {}",
                    receipt.detector,
                    receipt.path.display()
                )));
            }
        }
        for receipt in &self.discovery.excluded_paths {
            validate_detector_id(&receipt.detector)?;
            validate_exact_discovery_path(&receipt.path, "excluded discovery path")?;
            validate_sha256(
                &receipt.evidence_digest,
                "excluded discovery evidence digest",
            )?;
            if !identities.insert((&receipt.detector, &receipt.path)) {
                return Err(Error::Validation(format!(
                    "duplicate discovery receipt for detector {} at {}",
                    receipt.detector,
                    receipt.path.display()
                )));
            }
        }
        Ok(())
    }

    fn validate_release_groups(&self) -> Result<()> {
        let mut assigned = BTreeMap::new();
        for (kind, groups) in [("fixed", &self.fixed), ("linked", &self.linked)] {
            for (index, group) in groups.iter().enumerate() {
                if group.len() < 2 {
                    return Err(Error::Validation(format!(
                        "{kind} group {index} must contain at least two release units"
                    )));
                }
                let mut members = BTreeSet::new();
                for member in group {
                    if !self.release_units.contains_key(member) {
                        return Err(Error::Validation(format!(
                            "{kind} group {index} references unknown release unit {member}"
                        )));
                    }
                    if !members.insert(member) {
                        return Err(Error::Validation(format!(
                            "{kind} group {index} repeats release unit {member}"
                        )));
                    }
                    if let Some(previous) = assigned.insert(member, format!("{kind} group {index}"))
                    {
                        return Err(Error::Validation(format!(
                            "release unit {member} belongs to both {previous} and {kind} group {index}"
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_dependency_acyclic(&self) -> Result<()> {
        fn visit<'a>(
            id: &'a str,
            release_units: &'a BTreeMap<String, ReleaseUnitConfig>,
            visiting: &mut BTreeSet<&'a str>,
            visited: &mut BTreeSet<&'a str>,
        ) -> Result<()> {
            if visited.contains(id) {
                return Ok(());
            }
            if !visiting.insert(id) {
                return Err(Error::Validation(format!(
                    "dependency cycle includes release unit {id}"
                )));
            }
            for dependency in &release_units[id].depends_on {
                visit(dependency, release_units, visiting, visited)?;
            }
            visiting.remove(id);
            visited.insert(id);
            Ok(())
        }

        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        for id in self.release_units.keys() {
            visit(id, &self.release_units, &mut visiting, &mut visited)?;
        }
        Ok(())
    }

    fn validate_tag_graph(&self, known: &BTreeSet<String>) -> Result<()> {
        let mut edges = BTreeMap::<String, Vec<String>>::new();
        for (release_unit_id, release_unit) in &self.release_units {
            for (tag_id, tag) in &release_unit.tags {
                edges.insert(
                    Self::release_unit_tag_id(release_unit_id, tag_id),
                    tag.tag_after.clone(),
                );
            }
        }
        for (tag_id, tag) in &self.workspace_tags {
            edges.insert(Self::workspace_tag_id(tag_id), tag.tag_after.clone());
        }
        for (id, prerequisites) in &edges {
            let mut unique = BTreeSet::new();
            for prerequisite in prerequisites {
                if prerequisite == id {
                    return Err(Error::Validation(format!(
                        "tag {id} cannot depend on itself"
                    )));
                }
                if !known.contains(prerequisite) {
                    return Err(Error::Validation(format!(
                        "tag {id} depends on unknown tag {prerequisite}"
                    )));
                }
                if !unique.insert(prerequisite) {
                    return Err(Error::Validation(format!(
                        "tag {id} repeats prerequisite {prerequisite}"
                    )));
                }
            }
        }

        fn visit<'a>(
            id: &'a str,
            edges: &'a BTreeMap<String, Vec<String>>,
            visiting: &mut BTreeSet<&'a str>,
            visited: &mut BTreeSet<&'a str>,
        ) -> Result<()> {
            if visited.contains(id) {
                return Ok(());
            }
            if !visiting.insert(id) {
                return Err(Error::Validation(format!("tag-order cycle includes {id}")));
            }
            for dependency in &edges[id] {
                visit(dependency, edges, visiting, visited)?;
            }
            visiting.remove(id);
            visited.insert(id);
            Ok(())
        }
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        for id in edges.keys() {
            visit(id, &edges, &mut visiting, &mut visited)?;
        }
        Ok(())
    }
}

fn validate_dependencies(
    id: &str,
    release_unit: &ReleaseUnitConfig,
    release_units: &BTreeMap<String, ReleaseUnitConfig>,
) -> Result<()> {
    let mut dependencies = BTreeSet::new();
    for dependency in &release_unit.depends_on {
        if dependency == id {
            return Err(Error::Validation(format!(
                "release unit {id} cannot depend on itself"
            )));
        }
        if !release_units.contains_key(dependency) {
            return Err(Error::Validation(format!(
                "release unit {id} depends on unknown release unit {dependency}"
            )));
        }
        if !dependencies.insert(dependency) {
            return Err(Error::Validation(format!(
                "release unit {id} repeats dependency {dependency}"
            )));
        }
    }
    Ok(())
}

fn validate_id(id: &str, kind: &str) -> Result<()> {
    if id.is_empty()
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.@/".contains(character))
    {
        return Err(Error::Validation(format!("invalid {kind} id {id:?}")));
    }
    Ok(())
}

pub(crate) fn validate_detector_id(id: &str) -> Result<()> {
    if id.is_empty()
        || !id
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
        || !id.chars().all(|character| {
            character.is_ascii_lowercase()
                || character.is_ascii_digit()
                || "-_.".contains(character)
        })
    {
        return Err(Error::Validation(format!("invalid detector id {id:?}")));
    }
    Ok(())
}

pub(crate) fn validate_sha256(value: &str, description: &str) -> Result<()> {
    let Some(digest) = value.strip_prefix("sha256:") else {
        return Err(Error::Validation(format!(
            "{description} must be sha256 followed by 64 lowercase hexadecimal digits"
        )));
    };
    if digest.len() != 64
        || !digest
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
    {
        return Err(Error::Validation(format!(
            "{description} must be sha256 followed by 64 lowercase hexadecimal digits"
        )));
    }
    Ok(())
}

pub(crate) fn validate_exact_discovery_path(path: &Path, description: &str) -> Result<()> {
    validate_relative_path(path, description)?;
    let rendered = path.to_string_lossy();
    let normalized = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => Some(value.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    if rendered.replace('\\', "/") != normalized
        || rendered
            .chars()
            .any(|character| matches!(character, '*' | '?' | '[' | ']' | '{' | '}'))
    {
        return Err(Error::Validation(format!(
            "{description} must identify one exact workspace-relative path"
        )));
    }
    Ok(())
}

pub(crate) fn validate_tag_template(
    description: &str,
    template: &str,
    allow_id: bool,
) -> Result<()> {
    if template.matches("{version}").count() != 1
        || template.matches("{id}").count() > usize::from(allow_id)
    {
        return Err(Error::Validation(format!(
            "{description} template must contain exactly one {{version}}{}",
            if allow_id {
                " and at most one {id}"
            } else {
                " and no {id}"
            }
        )));
    }
    if template.contains("v{version}") {
        return Err(Error::Validation(format!(
            "{description} template must not prefix versions with v"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
$schema: https://intentional.foo/schemas/config.yml
contract: contract-1
settings:
  internal-dependency-bump: patch
  pre-1-0-bump-mapping: component
workspace-tags:
  release:
    template: '{version}'
release-units:
  library:
    path: packages/library
    projections:
      - { adapter: npm, file: package.json, mode: committed }
    tags:
      primary: { role: primary, template: '{id}@{version}' }
  application:
    path: packages/application
    depends-on: [library]
    projections:
      - adapter: json
        file: metadata.json
        pointer: /version
        mode: injected
    tags:
      primary: { role: primary, template: 'application@{version}' }
"#;

    #[test]
    fn parses_complete_contract() {
        let config = Config::from_yaml(VALID).expect("valid config");
        assert_eq!(config.contract, CURRENT_CONTRACT);
        assert_eq!(
            config.settings.pre_1_0_bump_mapping,
            Pre1BumpMapping::Component
        );
        assert_eq!(config.primary_tag("library").expect("primary").0, "primary");
    }

    #[test]
    fn validates_managed_and_exact_excluded_discovery_receipts() {
        let receipts = VALID.replace(
            "release-units:\n",
            "discovery:\n  managed-paths:\n    - detector: npm-package\n      path: packages/library/package.json\n      release-unit: library\n  excluded-paths:\n    - detector: npm-package\n      path: examples/package.json\n      evidence-digest: sha256:0000000000000000000000000000000000000000000000000000000000000000\nrelease-units:\n",
        );
        let config = Config::from_yaml(&receipts).expect("discovery receipts accepted");
        assert_eq!(config.discovery.managed_paths.len(), 1);
        assert_eq!(config.discovery.excluded_paths.len(), 1);

        let unknown = receipts.replace("release-unit: library", "release-unit: missing");
        assert!(Config::from_yaml(&unknown)
            .expect_err("unknown managed target rejected")
            .to_string()
            .contains("unknown release unit missing"));

        let glob = receipts.replace("examples/package.json", "examples/*.json");
        assert!(Config::from_yaml(&glob)
            .expect_err("glob exclusion rejected")
            .to_string()
            .contains("one exact workspace-relative path"));

        let non_canonical = receipts.replace("examples/package.json", "./examples/package.json");
        assert!(Config::from_yaml(&non_canonical)
            .expect_err("non-canonical exact path rejected")
            .to_string()
            .contains("one exact workspace-relative path"));

        let stale_shape = receipts.replace(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "sha256:abcd",
        );
        assert!(Config::from_yaml(&stale_shape)
            .expect_err("invalid exclusion digest rejected")
            .to_string()
            .contains("64 lowercase hexadecimal digits"));
    }

    #[test]
    fn published_config_schema_carries_discovery_receipt_contracts() {
        let schema: serde_yaml::Value =
            serde_yaml::from_str(include_str!("../../../schemas/config.yml"))
                .expect("config schema parses");
        let discovery = &schema["$defs"]["discovery"];
        assert_eq!(discovery["additionalProperties"].as_bool(), Some(false));
        assert_eq!(
            discovery["properties"]["managed-paths"]["items"]["required"]
                .as_sequence()
                .expect("managed receipt required fields")
                .len(),
            3
        );
        assert_eq!(
            discovery["properties"]["excluded-paths"]["items"]["properties"]["evidence-digest"]
                ["pattern"]
                .as_str(),
            Some("^sha256:[0-9a-f]{64}$")
        );
    }

    /// `VALID` with one more release unit `beta` at `site` whose single
    /// projection uses `adapter` and `file`.
    fn with_projection(adapter: &str, file: &str) -> String {
        VALID.replace(
            "release-units:\n",
            &format!(
                "release-units:\n  beta:\n    path: site\n    projections:\n      - {{ adapter: {adapter}, file: '{file}', pointer: /beta, mode: committed }}\n    tags:\n      primary: {{ role: primary, template: 'beta@{{version}}' }}\n"
            ),
        )
    }

    const NON_CANONICAL_ROOT_FILES: &[&str] = &[
        "/a/../b.yml",
        "/./b.yml",
        "/a/./b.yml",
        "//b.yml",
        "/a//b.yml",
        "/b/",
        "/",
        "/..",
        "/a/b\\c.yml",
        "\\b.yml",
        "\\\\server\\b.yml",
        "C:/b.yml",
        "c:b.yml",
    ];

    #[test]
    fn accepts_root_projection_for_generic_adapters() {
        for adapter in ["json", "toml", "yaml"] {
            let config = Config::from_yaml(&with_projection(adapter, "/docs/meta.yml"))
                .expect("root projection accepted");
            let file = &config.release_units["beta"].projections[0].file;
            assert!(file.is_root());
            for unit_path in ["site", ".", "a/b"] {
                let resolved = file.workspace_path(Path::new(unit_path));
                assert_eq!(resolved, Path::new("docs").join("meta.yml"));
                assert!(resolved
                    .components()
                    .all(|component| matches!(component, Component::Normal(_))));
            }
        }
    }

    #[test]
    fn rejects_non_canonical_root_projection_paths() {
        for file in NON_CANONICAL_ROOT_FILES {
            let error = Config::from_yaml(&with_projection("yaml", file))
                .expect_err("non-canonical root projection rejected")
                .to_string();
            assert!(
                error.contains(&format!(
                    "release unit beta projection file {file} must be a root path of the form /dir/file"
                )),
                "{file}: {error}"
            );
        }
    }

    #[test]
    fn rejects_root_projection_for_ecosystem_adapters() {
        for adapter in ["npm", "cargo", "pub", "python", "msbuild", "go"] {
            let error = Config::from_yaml(&with_projection(adapter, "/site/manifest"))
                .expect_err("ecosystem root projection rejected")
                .to_string();
            assert!(
                error.contains(&format!(
                    "release unit beta {adapter} projection file must be relative to the release-unit path; only json, toml, and yaml projections may use a root path"
                )),
                "{adapter}: {error}"
            );
        }
    }

    #[test]
    fn keeps_unit_relative_projection_rules() {
        Config::from_yaml(&with_projection("yaml", "./meta.yml")).expect("dot prefix accepted");
        Config::from_yaml(&with_projection("yaml", "sub/meta.yml")).expect("nested accepted");
        for file in ["../meta.yml", "sub/../../meta.yml"] {
            assert!(Config::from_yaml(&with_projection("yaml", file))
                .expect_err("parent segment rejected")
                .to_string()
                .contains("must be a non-empty relative path without .."));
        }
    }

    #[test]
    fn root_projection_round_trips_through_to_yaml() {
        let config = Config::from_yaml(&with_projection("yaml", "/docs/meta.yml"))
            .expect("root projection accepted");
        let yaml = config.to_yaml().expect("serialized");
        assert!(yaml.contains("file: /docs/meta.yml"), "{yaml}");
        assert_eq!(Config::from_yaml(&yaml).expect("reparsed"), config);
    }

    #[test]
    fn published_projection_file_pattern_agrees_with_config_validation() {
        let schema: serde_yaml::Value =
            serde_yaml::from_str(include_str!("../../../schemas/config.yml"))
                .expect("config schema parses");
        let pattern = schema["$defs"]["projection"]["properties"]["file"]["pattern"]
            .as_str()
            .expect("projection file pattern");
        let pattern = regex::Regex::new(pattern).expect("pattern compiles");
        let accepted = [
            "/b.yml",
            "/docs/meta.yml",
            "/.config/x.yml",
            "/a/..b/c.yml",
            "/a/b..",
            "meta.yml",
            "./meta.yml",
            "sub/meta.yml",
        ];
        for file in accepted {
            assert!(pattern.is_match(file), "schema refuses {file}");
            Config::from_yaml(&with_projection("yaml", file))
                .unwrap_or_else(|error| panic!("config refuses {file}: {error}"));
        }
        for file in NON_CANONICAL_ROOT_FILES
            .iter()
            .filter(|file| file.starts_with(['/', '\\']))
        {
            assert!(!pattern.is_match(file), "schema accepts {file}");
        }
    }

    /// A config of release units built from `(id, path, projections)`,
    /// where each projection is a flow mapping.
    fn units(units: &[(&str, &str, &[&str])]) -> String {
        let mut text = "contract: contract-1\nrelease-units:\n".to_owned();
        for (id, path, projections) in units {
            text.push_str(&format!("  {id}:\n    path: {path}\n    projections:\n"));
            for projection in *projections {
                text.push_str(&format!("      - {projection}\n"));
            }
            text.push_str(&format!(
                "    tags:\n      primary: {{ role: primary, template: '{id}@{{version}}' }}\n"
            ));
        }
        text
    }

    fn ownership_error(config: &str) -> String {
        Config::from_yaml(config)
            .expect_err("shared field rejected")
            .to_string()
    }

    #[test]
    fn rejects_two_release_units_projecting_one_pointer() {
        let config = units(&[
            (
                "alpha",
                "a",
                &["{ adapter: yaml, file: /site/meta.yml, pointer: /meta/version, mode: committed }"],
            ),
            (
                "beta",
                "site",
                &["{ adapter: yaml, file: meta.yml, pointer: /meta/version, mode: injected }"],
            ),
        ]);
        assert!(ownership_error(&config)
            .contains("release units alpha and beta both project /meta/version in site/meta.yml"));
    }

    #[test]
    fn rejects_pointer_spelling_aliases() {
        let dotted = units(&[
            (
                "alpha",
                ".",
                &["{ adapter: yaml, file: site/meta.yml, pointer: meta.version, mode: committed }"],
            ),
            (
                "beta",
                "b",
                &["{ adapter: yaml, file: /site/meta.yml, pointer: /meta/version, mode: committed }"],
            ),
        ]);
        assert!(ownership_error(&dotted)
            .contains("release units alpha and beta both project /meta/version in site/meta.yml"));

        let escaped = units(&[
            (
                "alpha",
                "a",
                &["{ adapter: json, file: /site/meta.json, pointer: /a~1b, mode: committed }"],
            ),
            (
                "beta",
                "b",
                &["{ adapter: json, file: /./site/../site/meta.json, pointer: /a~1b, mode: committed }"],
            ),
        ]);
        assert!(ownership_error(&escaped).contains("must be a root path"));
        let escaped = escaped.replace("/./site/../site/meta.json", "/site/meta.json");
        assert!(ownership_error(&escaped)
            .contains("release units alpha and beta both project /a~1b in site/meta.json"));

        let ecosystem = units(&[
            (
                "alpha",
                "pkg",
                &["{ adapter: npm, file: package.json, mode: committed }"],
            ),
            (
                "beta",
                "b",
                &["{ adapter: json, file: /pkg/package.json, pointer: /version, mode: committed }"],
            ),
        ]);
        assert!(ownership_error(&ecosystem)
            .contains("release units alpha and beta both project /version in pkg/package.json"));

        let same_unit = units(&[(
            "alpha",
            "a",
            &[
                "{ adapter: yaml, file: meta.yml, pointer: meta.version, mode: committed }",
                "{ adapter: yaml, file: ./meta.yml, pointer: /meta/version, mode: committed }",
            ],
        )]);
        assert!(ownership_error(&same_unit)
            .contains("release unit alpha repeats projection ./meta.yml"));
    }

    #[test]
    fn ecosystem_adapters_own_their_fixed_version_field() {
        let cases = [
            ("npm", "package.json", "json", "/version"),
            ("cargo", "Cargo.toml", "toml", "package.version"),
            ("pub", "pubspec.yaml", "yaml", "/version"),
            ("python", "pyproject.toml", "toml", "/project/version"),
        ];
        for (adapter, file, generic, pointer) in cases {
            let config = units(&[
                (
                    "alpha",
                    "pkg",
                    &[&format!("{{ adapter: {adapter}, file: {file}, mode: committed }}")],
                ),
                (
                    "beta",
                    "b",
                    &[&format!(
                        "{{ adapter: {generic}, file: /pkg/{file}, pointer: {pointer}, mode: committed }}"
                    )],
                ),
            ]);
            let error = ownership_error(&config);
            assert!(
                error.contains("release units alpha and beta both project"),
                "{adapter}: {error}"
            );
        }
        for (adapter, file, field) in [
            ("msbuild", "Sample.csproj", "<Version>"),
            ("go", "go.mod", "the module path"),
            ("npm", "package.json", "/version"),
        ] {
            let config = units(&[
                (
                    "alpha",
                    "pkg",
                    &[&format!(
                        "{{ adapter: {adapter}, file: {file}, mode: committed }}"
                    )],
                ),
                (
                    "beta",
                    ".",
                    &[&format!(
                        "{{ adapter: {adapter}, file: pkg/{file}, mode: injected }}"
                    )],
                ),
            ]);
            let error = ownership_error(&config);
            assert!(
                error.contains(&format!(
                    "release units alpha and beta both project {field} in pkg/{file}"
                )),
                "{adapter}: {error}"
            );
        }
        let distinct = units(&[
            (
                "alpha",
                "pkg",
                &["{ adapter: cargo, file: Cargo.toml, mode: committed }"],
            ),
            (
                "beta",
                "b",
                &["{ adapter: toml, file: /pkg/Cargo.toml, pointer: /dependencies/beta/version, mode: committed }"],
            ),
        ]);
        Config::from_yaml(&distinct).expect("distinct field in one manifest accepted");
    }

    #[test]
    fn allows_distinct_pointers_in_one_shared_file() {
        let config = units(&[
            (
                "alpha",
                "a",
                &["{ adapter: yaml, file: /site/meta.yml, pointer: /alpha/version, mode: committed }"],
            ),
            (
                "beta",
                "site",
                &["{ adapter: yaml, file: meta.yml, pointer: /alpha/docs-version, mode: committed }"],
            ),
        ]);
        Config::from_yaml(&config).expect("distinct pointers accepted");
    }

    #[test]
    fn ignores_none_mode_in_ownership() {
        let config = units(&[
            (
                "alpha",
                "a",
                &["{ adapter: yaml, file: /site/meta.yml, pointer: /meta/version, mode: committed }"],
            ),
            (
                "beta",
                "site",
                &["{ adapter: yaml, file: meta.yml, pointer: /meta/version, mode: none }"],
            ),
        ]);
        Config::from_yaml(&config).expect("mode none does not own a field");
    }

    #[test]
    fn validates_fixed_and_linked_membership() {
        let invalid = VALID.replace(
            "release-units:\n",
            "fixed: [[library, application]]\nlinked: [[library, application]]\nrelease-units:\n",
        );
        let error = Config::from_yaml(&invalid).expect_err("overlap rejected");
        assert!(error.to_string().contains("belongs to both"));
    }

    #[test]
    fn permits_tag_only_release_units() {
        let valid = VALID.replace(
            "    projections:\n      - { adapter: npm, file: package.json, mode: committed }\n    tags:",
            "    tags:",
        );
        Config::from_yaml(&valid).expect("tag-only release unit accepted");
    }

    #[test]
    fn rejects_obsolete_packages_key() {
        let obsolete = VALID.replace("release-units:", "packages:");
        let error = Config::from_yaml(&obsolete).expect_err("obsolete public key rejected");
        assert!(error.to_string().contains("unknown field `packages`"));
    }

    #[test]
    fn rejects_missing_or_duplicate_primary_tags() {
        let missing = VALID.replace(
            "role: primary, template: '{id}@{version}'",
            "role: projection, template: '{id}@{version}'",
        );
        assert!(Config::from_yaml(&missing)
            .expect_err("missing primary rejected")
            .to_string()
            .contains("exactly one primary"));
        let duplicate = VALID.replace(
            "primary: { role: primary, template: '{id}@{version}' }",
            "primary: { role: primary, template: '{id}@{version}' }\n      second: { role: primary, template: 'second@{version}' }",
        );
        assert!(Config::from_yaml(&duplicate)
            .expect_err("duplicate primary rejected")
            .to_string()
            .contains("exactly one primary"));
    }

    #[test]
    fn rejects_unknown_and_cyclic_tag_prerequisites() {
        let unknown = VALID.replace(
            "template: '{version}'",
            "template: '{version}'\n    tag-after: [workspace/missing]",
        );
        assert!(Config::from_yaml(&unknown)
            .expect_err("unknown tag rejected")
            .to_string()
            .contains("unknown tag"));
        let cyclic = VALID
            .replace("template: '{version}'", "template: '{version}'\n    tag-after: [release-unit/library/primary]")
            .replace(
                "primary: { role: primary, template: '{id}@{version}' }",
                "primary: { role: primary, template: '{id}@{version}', tag-after: [workspace/release] }",
            );
        assert!(Config::from_yaml(&cyclic)
            .expect_err("tag cycle rejected")
            .to_string()
            .contains("tag-order cycle"));
    }

    #[test]
    fn rejects_versions_and_legacy_global_tag() {
        let versioned = VALID.replace(
            "path: packages/library",
            "path: packages/library\n    version: 1.2.3",
        );
        assert!(Config::from_yaml(&versioned).is_err());
        let legacy = VALID.replace(
            "internal-dependency-bump: patch",
            "internal-dependency-bump: patch\n  global-tag: true",
        );
        assert!(Config::from_yaml(&legacy).is_err());
    }
}
