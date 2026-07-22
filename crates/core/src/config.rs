// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Workspace configuration and validation.

use crate::error::{Error, Result};
use crate::model::{
    Adapter, Bump, Pre1BumpMapping, ProjectionMode, ReleaseUnitDisposition, TagPhase, TagRole,
};
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

            let mut projection_keys = BTreeSet::new();
            for projection in &release_unit.projections {
                validate_relative_path(
                    &projection.file,
                    &format!("release unit {id} projection file"),
                )?;
                if projection.adapter.requires_pointer()
                    && projection.pointer.as_deref().is_none_or(str::is_empty)
                {
                    return Err(Error::Validation(format!(
                        "release unit {id} generic {:?} projection requires a pointer",
                        projection.adapter
                    )));
                }
                let key = (&projection.file, projection.pointer.as_deref());
                if !projection_keys.insert(key) {
                    return Err(Error::Validation(format!(
                        "release unit {id} repeats projection {}",
                        projection.file.display()
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

fn validate_tag_template(description: &str, template: &str, allow_id: bool) -> Result<()> {
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
