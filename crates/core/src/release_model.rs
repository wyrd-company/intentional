// ---
// relationships:
//   implements: release-model
// ---

//! Schema-validated, version-independent projection of a Release Workspace.

use crate::{
    native_identity, Config, Error, ProjectionMode, ReleaseRecordPolicy, ReleaseUnitDisposition,
    Result, TagPhase, TagRole, CONFIG_PATH, INTENTS_PATH,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Identity of the current Release Model contract.
pub const RELEASE_MODEL_SCHEMA_ID: &str = "https://intentional.foo/schemas/release-model/1";
/// Workspace-relative derived document location.
pub const RELEASE_MODEL_PATH: &str = ".intentional/release-model.json";

/// Release Workspace facts for process templates.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReleaseModel {
    /// Exact schema identity.
    #[serde(rename = "$schema")]
    pub schema: String,
    /// Fixed Intentional locations.
    pub paths: ModelPaths,
    /// Release Units keyed by configured id.
    pub release_units: BTreeMap<String, ReleaseUnitEntry>,
    /// Workspace tag streams keyed by configured id.
    pub workspace_tags: BTreeMap<String, WorkspaceTagEntry>,
    /// Fixed and linked groups.
    pub release_groups: ReleaseGroups,
}

/// Fixed workspace-relative Intentional paths.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModelPaths {
    /// Configuration location.
    pub config: String,
    /// Pending intent directory.
    pub intents: String,
    /// Derived model location.
    pub model: String,
}

/// Process-facing facts about one Release Unit.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReleaseUnitEntry {
    /// Workspace-relative unit directory.
    pub path: PathBuf,
    /// Workspace-relative changelog.
    pub changelog: PathBuf,
    /// Release participation.
    pub disposition: ReleaseUnitDisposition,
    /// Surrounding process's release-record policy.
    pub release_record: ReleaseRecordPolicy,
    /// Internal dependency ids.
    pub depends_on: Vec<String>,
    /// Group membership when configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub release_group: Option<ReleaseGroupMembership>,
    /// Distinct ecosystem adapter names.
    pub ecosystems: Vec<String>,
    /// Projections in configuration order.
    pub projections: Vec<ProjectionEntry>,
    /// Non-excluded discovery receipts.
    pub sources: Vec<SourceEntry>,
    /// Configured tag streams.
    pub tags: BTreeMap<String, ReleaseUnitTagEntry>,
}

/// One manifest or generic format projection.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct ProjectionEntry {
    /// Adapter name, an open schema value.
    pub adapter: String,
    /// File relative to its unit.
    pub file: PathBuf,
    /// File relative to the workspace.
    pub workspace_file: PathBuf,
    /// Materialization mode.
    pub mode: ProjectionMode,
    /// Whether this is a generic format adapter.
    pub generic: bool,
    /// Generic adapter's version pointer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
    /// Native package identity, when present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_identity: Option<String>,
}

/// One discovery receipt naming the unit.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub struct SourceEntry {
    /// Workspace-relative discovered path.
    pub path: PathBuf,
    /// Detector id.
    pub detector: String,
}

/// Release Unit tag stream facts.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReleaseUnitTagEntry {
    /// Primary authority or projection.
    pub role: TagRole,
    /// Unrendered configured template.
    pub template: String,
    /// Required executor phase, if configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_phase: Option<TagPhase>,
    /// Canonical prerequisite tag ids.
    pub tag_after: Vec<String>,
}

/// Workspace tag stream facts.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct WorkspaceTagEntry {
    /// Unrendered configured template.
    pub template: String,
    /// Required executor phase, if configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_phase: Option<TagPhase>,
    /// Canonical prerequisite tag ids.
    pub tag_after: Vec<String>,
}

/// Configured release groups.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReleaseGroups {
    /// Fixed groups.
    pub fixed: Vec<Vec<String>>,
    /// Linked groups.
    pub linked: Vec<Vec<String>>,
}

/// A unit's complete group membership.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReleaseGroupMembership {
    /// Fixed or linked.
    pub kind: String,
    /// Complete member ids.
    pub members: Vec<String>,
}

impl ReleaseModel {
    /// Load the workspace configuration and project its facts. No tags, intents,
    /// remote, branch, clock, or existing model participate in the result.
    pub fn build(root: &Path) -> Result<Self> {
        Self::from_config(root, &Config::load(root)?)
    }

    /// Project validated configuration, including initialization's proposed config.
    pub fn from_config(root: &Path, config: &Config) -> Result<Self> {
        config.validate()?;
        gix::discover(root).map_err(|error| Error::Git(error.to_string()))?;
        let mut release_units = BTreeMap::new();
        for (id, unit) in &config.release_units {
            let mut projections = Vec::new();
            let mut ecosystems = BTreeSet::new();
            for projection in &unit.projections {
                let workspace_file = relative_path(&unit.path.join(&projection.file));
                let path = root.join(&workspace_file);
                let text =
                    std::fs::read_to_string(&path).map_err(|error| Error::io(&path, error))?;
                // Exercise the same adapter reader as status, including inherited
                // Cargo versions. Go has no version field to read.
                if projection.mode != ProjectionMode::None
                    && projection.adapter != crate::Adapter::Go
                {
                    crate::status::read_projection_version(
                        root,
                        &workspace_file,
                        projection,
                        &text,
                    )?;
                }
                if projection.mode == ProjectionMode::None && projection.adapter.requires_pointer()
                {
                    match projection.adapter {
                        crate::Adapter::Json => {
                            serde_json::from_str::<serde_json::Value>(&text).map_err(json_error)?;
                        }
                        crate::Adapter::Toml => {
                            text.parse::<toml_edit::DocumentMut>().map_err(|error| {
                                Error::Validation(format!("invalid {}: {error}", path.display()))
                            })?;
                        }
                        crate::Adapter::Yaml => {
                            serde_yaml::from_str::<serde_yaml::Value>(&text)?;
                        }
                        _ => unreachable!("generic adapters"),
                    }
                }
                let identity = native_identity(projection.adapter, &workspace_file, &text)?;
                if let Some(ecosystem) = projection.adapter.ecosystem() {
                    ecosystems.insert(ecosystem.to_owned());
                }
                let adapter = serde_json::to_value(projection.adapter)
                    .map_err(json_error)?
                    .as_str()
                    .expect("adapter serializes as string")
                    .to_owned();
                projections.push(ProjectionEntry {
                    adapter,
                    file: projection.file.clone(),
                    workspace_file,
                    mode: projection.mode,
                    generic: projection.adapter.requires_pointer(),
                    pointer: projection.pointer.clone(),
                    native_identity: identity,
                });
            }
            let sources = config
                .discovery
                .managed_paths
                .iter()
                .filter(|receipt| receipt.release_unit == *id)
                .map(|receipt| SourceEntry {
                    detector: receipt.detector.clone(),
                    path: receipt.path.clone(),
                })
                .collect();
            let release_group = [("fixed", &config.fixed), ("linked", &config.linked)]
                .into_iter()
                .find_map(|(kind, groups)| {
                    groups
                        .iter()
                        .find(|members| members.contains(id))
                        .map(|members| ReleaseGroupMembership {
                            kind: kind.to_owned(),
                            members: members.clone(),
                        })
                });
            release_units.insert(
                id.clone(),
                ReleaseUnitEntry {
                    path: unit.path.clone(),
                    changelog: relative_path(&unit.path.join("CHANGELOG.md")),
                    disposition: unit.disposition,
                    release_record: unit.release_record,
                    depends_on: unit.depends_on.clone(),
                    release_group,
                    ecosystems: ecosystems.into_iter().collect(),
                    projections,
                    sources,
                    tags: unit
                        .tags
                        .iter()
                        .map(|(id, tag)| {
                            (
                                id.clone(),
                                ReleaseUnitTagEntry {
                                    role: tag.role,
                                    template: tag.template.clone(),
                                    require_phase: tag.require_phase,
                                    tag_after: tag.tag_after.clone(),
                                },
                            )
                        })
                        .collect(),
                },
            );
        }
        let model = Self {
            schema: RELEASE_MODEL_SCHEMA_ID.to_owned(),
            paths: ModelPaths {
                config: CONFIG_PATH.to_owned(),
                intents: INTENTS_PATH.to_owned(),
                model: RELEASE_MODEL_PATH.to_owned(),
            },
            release_units,
            workspace_tags: config
                .workspace_tags
                .iter()
                .map(|(id, tag)| {
                    (
                        id.clone(),
                        WorkspaceTagEntry {
                            template: tag.template.clone(),
                            require_phase: tag.require_phase,
                            tag_after: tag.tag_after.clone(),
                        },
                    )
                })
                .collect(),
            release_groups: ReleaseGroups {
                fixed: config.fixed.clone(),
                linked: config.linked.clone(),
            },
        };
        model.encode()?;
        Ok(model)
    }

    /// Validate and encode canonical pretty JSON with sorted members and lists.
    pub fn encode(&self) -> Result<String> {
        let mut model = self.clone();
        for unit in model.release_units.values_mut() {
            unit.depends_on.sort();
            unit.ecosystems.sort();
            unit.sources.sort();
            if let Some(group) = &mut unit.release_group {
                group.members.sort();
            }
            for tag in unit.tags.values_mut() {
                tag.tag_after.sort();
            }
        }
        for tag in model.workspace_tags.values_mut() {
            tag.tag_after.sort();
        }
        for groups in [
            &mut model.release_groups.fixed,
            &mut model.release_groups.linked,
        ] {
            for members in groups.iter_mut() {
                members.sort();
            }
            groups.sort();
        }
        let value = serde_json::to_value(&model).map_err(json_error)?;
        crate::schema::validate(crate::schema::SchemaRecord::ReleaseModel, &value)?;
        Ok(format!(
            "{}\n",
            serde_json::to_string_pretty(&value).map_err(json_error)?
        ))
    }

    /// Decode a document only after validating its current schema contract.
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value: serde_json::Value = serde_json::from_slice(bytes).map_err(json_error)?;
        crate::schema::validate(crate::schema::SchemaRecord::ReleaseModel, &value)?;
        serde_json::from_value(value).map_err(json_error)
    }
}

// Join paths in the model's portable, workspace-relative spelling.
fn relative_path(path: &Path) -> PathBuf {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::CurDir => None,
            other => Some(other.as_os_str().to_string_lossy().into_owned()),
        })
        .collect::<Vec<_>>()
        .join("/")
        .into()
}

fn json_error(error: serde_json::Error) -> Error {
    Error::Validation(format!("invalid Release Model JSON: {error}"))
}

// Keep unrelated bytes, including non-UTF-8 comments and original line endings.
// Put the dedicated rule last so later wildcard rules cannot override it.
pub(crate) fn model_attributes(root: &Path) -> Result<Option<Vec<u8>>> {
    let path = root.join(".gitattributes");
    let current = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(Error::io(&path, error)),
    };
    let mut updated = Vec::new();
    let mut attributes = Vec::new();
    let lines = current
        .split_inclusive(|byte| *byte == b'\n')
        .collect::<Vec<_>>();
    let model_line = lines.iter().rposition(|line| {
        matches!(
            line.split(|byte| byte.is_ascii_whitespace()).next(),
            Some(b".intentional/release-model.json" | b"/.intentional/release-model.json")
        )
    });
    for (index, line) in lines.iter().enumerate() {
        if Some(index) == model_line {
            for attribute in line.split(|byte| byte.is_ascii_whitespace()).skip(1) {
                if attribute.is_empty() {
                    continue;
                }
                let name = attribute
                    .strip_prefix(b"-")
                    .or_else(|| attribute.strip_prefix(b"!"))
                    .unwrap_or(attribute);
                let name = name.split(|byte| *byte == b'=').next().unwrap();
                if name != b"text" && name != b"eol" && name != b"crlf" {
                    attributes.push(attribute.to_vec());
                }
            }
        } else {
            updated.extend_from_slice(line);
        }
    }
    if !updated.is_empty() && !updated.ends_with(b"\n") {
        updated.push(b'\n');
    }
    updated.extend_from_slice(b".intentional/release-model.json");
    for attribute in attributes {
        updated.push(b' ');
        updated.extend(attribute);
    }
    updated.extend_from_slice(b" text eol=lf\n");
    Ok((updated != current).then_some(updated))
}

/// Write a complete validated model and ensure its LF checkout attribute.
/// Returns true when either file changes. A dry run only reports that result.
pub fn write_release_model(root: &Path, dry_run: bool) -> Result<bool> {
    let bytes = ReleaseModel::build(root)?.encode()?;
    let attributes = model_attributes(root)?;
    let path = root.join(RELEASE_MODEL_PATH);
    let model_changed = match std::fs::read(&path) {
        Ok(current) => current != bytes.as_bytes(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(error) => return Err(Error::io(&path, error)),
    };
    if !dry_run {
        if let Some(attributes) = &attributes {
            let path = root.join(".gitattributes");
            write_model_file(&path, attributes)?;
        }
        if model_changed {
            write_model_file(&path, bytes.as_bytes())?;
        }
    }
    Ok(model_changed || attributes.is_some())
}

// Stage beside the destination so readers see either the old or complete file.
pub(crate) fn write_model_file(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::create_dir_all(path.parent().expect("output parent"))
        .map_err(|error| Error::io(path, error))?;
    let temporary = path.with_file_name(format!(
        ".{}-{:016x}.tmp",
        path.file_name().expect("output filename").to_string_lossy(),
        rand::random::<u64>()
    ));
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|error| Error::io(&temporary, error))?;
    let result = (|| {
        file.write_all(bytes)
            .map_err(|error| Error::io(&temporary, error))?;
        drop(file);
        std::fs::rename(&temporary, path).map_err(|error| Error::io(path, error))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}
