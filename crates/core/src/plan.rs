// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Deterministic release planning, changelog rendering, and digest sealing.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::intent::Intent;
use crate::model::{Bump, TagPhase, TagRole};
use crate::version::{
    aggregate_bumps, bump_version_with_mapping, resolve_versions, VersionRepository,
};
use semver::{Prerelease, Version};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// One release unit in a canonical release plan.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PlanReleaseUnit {
    /// Release-unit id.
    pub id: String,
    /// Latest final tag-derived version.
    pub old_version: String,
    /// Intent-derived release version, optionally with a channel suffix.
    pub new_version: String,
    /// Effective aggregate bump.
    pub bump: Bump,
    /// Directly contributing intent ids.
    pub contributing_intent_ids: Vec<String>,
    /// Canonical ids of release-unit tags to create.
    pub tag_ids: Vec<String>,
    /// Deterministically rendered changelog section.
    pub release_notes: String,
}

/// Generator identity embedded inside the sealed payload.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Generator {
    /// Tool name.
    pub tool: String,
    /// Tool version.
    pub version: String,
}

/// One annotated tag record in creation order.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PlanTag {
    /// Canonical configuration tag id.
    pub id: String,
    /// Rendered Git tag name.
    pub name: String,
    /// Version recorded by the tag.
    pub version: String,
    /// Release-unit id for release-unit tags.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_unit: Option<String>,
    /// Release-unit tag role. Workspace tags have no release-unit role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<TagRole>,
    /// Required executor declaration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_phase: Option<TagPhase>,
    /// Canonical prerequisite tag ids.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tag_after: Vec<String>,
}

/// Canonical digest-bound release plan.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ReleasePlan {
    /// SHA-256 digest of the canonical plan payload excluding this seal.
    pub digest: String,
    /// Interpretation contract used to compute the plan.
    pub contract: String,
    /// Generator identity included in the digest.
    pub generator: Generator,
    /// Optional release channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    /// Changed release units ordered by id.
    pub release_units: Vec<PlanReleaseUnit>,
    /// All release-unit and workspace tags in canonical id order.
    pub tags: Vec<PlanTag>,
    /// Observable tag creation order derived only from `tag-after` edges.
    pub tag_order: Vec<String>,
}

#[derive(Serialize)]
struct PlanPayload<'a> {
    contract: &'a str,
    generator: &'a Generator,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: &'a Option<String>,
    release_units: &'a [PlanReleaseUnit],
    tags: &'a [PlanTag],
    tag_order: &'a [String],
}

/// One release note assigned to a semantic bump group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangelogEntry {
    /// Section group.
    pub bump: Bump,
    /// Markdown changelog prose.
    pub text: String,
}

impl ReleasePlan {
    /// Build a release plan from workspace state.
    pub fn build(root: &Path, channel: Option<&str>) -> Result<Self> {
        let config = Config::load(root)?;
        let intents = Intent::load_all(root, &config)?;
        Self::from_inputs(root, &config, &intents, channel)
    }

    /// Build a plan from already-loaded configuration and intents.
    pub fn from_inputs(
        root: &Path,
        config: &Config,
        intents: &[Intent],
        channel: Option<&str>,
    ) -> Result<Self> {
        Self::from_inputs_before(root, config, intents, channel, None)
    }

    pub(crate) fn from_inputs_before(
        root: &Path,
        config: &Config,
        intents: &[Intent],
        channel: Option<&str>,
        excluded_target: Option<gix::ObjectId>,
    ) -> Result<Self> {
        if channel.is_some_and(str::is_empty) {
            return Err(Error::Validation("channel must not be empty".to_owned()));
        }
        let repository = VersionRepository::discover(root)?;
        let declared = aggregate_bumps(intents.iter().map(|intent| &intent.release_units));
        let mut current_versions = BTreeMap::new();
        for id in config.release_units.keys() {
            let (_, primary) = config.primary_tag(id)?;
            let current = match excluded_target {
                Some(target) => repository.current_version_before(id, &primary.template, target)?,
                None => repository.current_version(id, &primary.template)?,
            };
            current_versions.insert(id.clone(), current);
        }
        let resolved = resolve_versions(config, &declared, &current_versions)?;
        let changed: BTreeSet<_> = resolved
            .iter()
            .filter(|(_, versions)| versions.bump != Bump::None)
            .map(|(id, _)| id.clone())
            .collect();
        let mut versions = BTreeMap::new();
        for id in &changed {
            let (_, primary) = config.primary_tag(id)?;
            let current = resolved[id].current.clone();
            let base = resolved[id].next.clone();
            let release = match channel {
                Some(channel) => channel_version(
                    &repository,
                    id,
                    &primary.template,
                    &base,
                    channel,
                    excluded_target,
                )?,
                None => base,
            };
            versions.insert(id.clone(), (current, release));
        }

        let mut release_units = Vec::with_capacity(changed.len());
        for id in &changed {
            let (current, release) = &versions[id];
            let intent_ids = intents
                .iter()
                .filter(|intent| intent.release_units.contains_key(id))
                .map(|intent| intent.id.clone())
                .collect::<Vec<_>>();
            let effective = resolved
                .iter()
                .map(|(id, versions)| (id.clone(), versions.bump))
                .collect();
            let entries = note_entries(id, config, intents, &effective, &versions);
            let release_notes = render_changelog_section(release, &entries);
            release_units.push(PlanReleaseUnit {
                id: id.clone(),
                old_version: current.to_string(),
                new_version: release.to_string(),
                bump: resolved[id].bump,
                contributing_intent_ids: intent_ids,
                tag_ids: config.release_units[id]
                    .tags
                    .keys()
                    .map(|tag_id| Config::release_unit_tag_id(id, tag_id))
                    .collect(),
                release_notes,
            });
        }

        let channel = channel.map(str::to_owned);
        let mut tags = Vec::new();
        for release_unit in &release_units {
            let config_release_unit = &config.release_units[&release_unit.id];
            let version = Version::parse(&release_unit.new_version)?;
            for (tag_id, tag) in &config_release_unit.tags {
                tags.push(PlanTag {
                    id: Config::release_unit_tag_id(&release_unit.id, tag_id),
                    name: render_tag(&tag.template, &release_unit.id, &version),
                    version: version.to_string(),
                    release_unit: Some(release_unit.id.clone()),
                    role: Some(tag.role),
                    require_phase: tag.require_phase,
                    tag_after: tag.tag_after.clone(),
                });
            }
        }
        let highest_bump = release_units
            .iter()
            .map(|release_unit| release_unit.bump)
            .max()
            .unwrap_or(Bump::None);
        if highest_bump != Bump::None {
            for (tag_id, tag) in &config.workspace_tags {
                let current = match excluded_target {
                    Some(target) => {
                        repository.current_version_before(tag_id, &tag.template, target)?
                    }
                    None => repository.current_version(tag_id, &tag.template)?,
                };
                let base = bump_version_with_mapping(
                    &current,
                    highest_bump,
                    config.settings.pre_1_0_bump_mapping,
                );
                let version = match channel.as_deref() {
                    Some(channel) => channel_version(
                        &repository,
                        tag_id,
                        &tag.template,
                        &base,
                        channel,
                        excluded_target,
                    )?,
                    None => base,
                };
                tags.push(PlanTag {
                    id: Config::workspace_tag_id(tag_id),
                    name: render_tag(&tag.template, tag_id, &version),
                    version: version.to_string(),
                    release_unit: None,
                    role: None,
                    require_phase: tag.require_phase,
                    tag_after: tag.tag_after.clone(),
                });
            }
        }
        tags.sort_by(|left, right| left.id.cmp(&right.id));
        let tag_order = tag_order(&tags)?;
        let generator = Generator {
            tool: "intentional".to_owned(),
            version: crate::VERSION.to_owned(),
        };
        let payload = PlanPayload {
            contract: &config.contract,
            generator: &generator,
            channel: &channel,
            release_units: &release_units,
            tags: &tags,
            tag_order: &tag_order,
        };
        let payload_json = canonical_json(&payload)?;
        let digest = format!("sha256:{:x}", Sha256::digest(payload_json.as_bytes()));
        Ok(Self {
            digest,
            contract: config.contract.clone(),
            generator,
            channel,
            release_units,
            tags,
            tag_order,
        })
    }

    /// Serialize this plan as compact canonical JSON with sorted object keys.
    pub fn to_canonical_json(&self) -> Result<String> {
        canonical_json(self)
    }

    /// Verify that the embedded digest seals the complete plan payload.
    pub fn verify_digest(&self) -> Result<()> {
        let payload = PlanPayload {
            contract: &self.contract,
            generator: &self.generator,
            channel: &self.channel,
            release_units: &self.release_units,
            tags: &self.tags,
            tag_order: &self.tag_order,
        };
        let actual = format!(
            "sha256:{:x}",
            Sha256::digest(canonical_json(&payload)?.as_bytes())
        );
        if actual != self.digest {
            return Err(Error::Validation(format!(
                "release plan digest mismatch: expected {}, computed {actual}",
                self.digest
            )));
        }
        Ok(())
    }
}

/// Serialize a value as compact JSON with recursively sorted object keys.
pub fn canonical_json(value: &impl Serialize) -> Result<String> {
    let value = serde_json::to_value(value)
        .map_err(|error| Error::Validation(format!("JSON serialization failed: {error}")))?;
    let mut output = String::new();
    write_canonical(&value, &mut output)?;
    Ok(output)
}

fn write_canonical(value: &Value, output: &mut String) -> Result<()> {
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            output.push_str(&serde_json::to_string(value).map_err(|error| {
                Error::Validation(format!("JSON serialization failed: {error}"))
            })?);
        }
        Value::Array(values) => {
            output.push('[');
            for (index, value) in values.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_canonical(value, output)?;
            }
            output.push(']');
        }
        Value::Object(values) => {
            output.push('{');
            let mut keys = values.keys().collect::<Vec<_>>();
            keys.sort();
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key).map_err(|error| {
                    Error::Validation(format!("JSON key serialization failed: {error}"))
                })?);
                output.push(':');
                write_canonical(&values[key], output)?;
            }
            output.push('}');
        }
    }
    Ok(())
}

fn channel_version(
    repository: &VersionRepository,
    release_unit_id: &str,
    template: &str,
    base: &Version,
    channel: &str,
    excluded_target: Option<gix::ObjectId>,
) -> Result<Version> {
    let versions = match excluded_target {
        Some(target) => repository.all_versions_before(release_unit_id, template, target)?,
        None => repository.all_versions(release_unit_id, template)?,
    };
    let max_iteration = versions
        .into_iter()
        .filter(|version| {
            version.major == base.major
                && version.minor == base.minor
                && version.patch == base.patch
        })
        .filter_map(|version| parse_channel_iteration(&version, channel))
        .max()
        .unwrap_or(0);
    let mut version = base.clone();
    version.pre = Prerelease::new(&format!("{channel}.{}", max_iteration + 1))?;
    Ok(version)
}

fn parse_channel_iteration(version: &Version, channel: &str) -> Option<u64> {
    let (name, iteration) = version.pre.as_str().split_once('.')?;
    (name == channel).then(|| iteration.parse().ok()).flatten()
}

fn render_tag(template: &str, id: &str, version: &Version) -> String {
    template
        .replace("{id}", id)
        .replace("{version}", &version.to_string())
}

fn note_entries(
    release_unit_id: &str,
    config: &Config,
    intents: &[Intent],
    effective: &BTreeMap<String, Bump>,
    versions: &BTreeMap<String, (Version, Version)>,
) -> Vec<ChangelogEntry> {
    let mut entries = intents
        .iter()
        .filter_map(|intent| {
            intent
                .release_units
                .get(release_unit_id)
                .map(|bump| ChangelogEntry {
                    bump: *bump,
                    text: intent.message.clone(),
                })
        })
        .collect::<Vec<_>>();
    for dependency in &config.release_units[release_unit_id].depends_on {
        if effective[dependency] == Bump::None
            || !versions.contains_key(dependency)
            || !shares_ecosystem(config, release_unit_id, dependency)
        {
            continue;
        }
        entries.push(ChangelogEntry {
            bump: config.settings.internal_dependency_bump,
            text: format!(
                "Update internal dependency `{dependency}` to `{}`.",
                versions[dependency].1
            ),
        });
    }
    entries
}

fn shares_ecosystem(config: &Config, left: &str, right: &str) -> bool {
    config.release_units[left]
        .projections
        .iter()
        .any(|left_projection| {
            left_projection.adapter.ecosystem().is_some()
                && config.release_units[right]
                    .projections
                    .iter()
                    .any(|right_projection| {
                        left_projection.adapter.ecosystem() == right_projection.adapter.ecosystem()
                    })
        })
}

/// Render one deterministic CommonMark changelog section.
pub fn render_changelog_section(version: &Version, entries: &[ChangelogEntry]) -> String {
    let mut output = format!("## {version}\n");
    for (bump, heading) in [
        (Bump::Major, "Breaking"),
        (Bump::Minor, "Features"),
        (Bump::Patch, "Fixes"),
    ] {
        let matching = entries
            .iter()
            .filter(|entry| entry.bump == bump)
            .collect::<Vec<_>>();
        if matching.is_empty() {
            continue;
        }
        output.push_str(&format!("\n### {heading}\n\n"));
        for entry in matching {
            let mut lines = entry.text.trim().lines();
            if let Some(first) = lines.next() {
                output.push_str("- ");
                output.push_str(first);
                output.push('\n');
            }
            for line in lines {
                output.push_str("  ");
                output.push_str(line);
                output.push('\n');
            }
        }
    }
    output
}

fn tag_order(tags: &[PlanTag]) -> Result<Vec<String>> {
    let by_id = tags
        .iter()
        .map(|tag| (tag.id.as_str(), tag))
        .collect::<BTreeMap<_, _>>();
    let mut visited = BTreeSet::new();
    let mut order = Vec::new();
    fn visit(
        id: &str,
        by_id: &BTreeMap<&str, &PlanTag>,
        visited: &mut BTreeSet<String>,
        order: &mut Vec<String>,
    ) -> Result<()> {
        if !visited.insert(id.to_owned()) {
            return Ok(());
        }
        for prerequisite in &by_id[id].tag_after {
            if !by_id.contains_key(prerequisite.as_str()) {
                return Err(Error::Validation(format!(
                    "release tag {id} requires {prerequisite}, which is not part of this release"
                )));
            }
            visit(prerequisite, by_id, visited, order)?;
        }
        order.push(id.to_owned());
        Ok(())
    }
    for id in by_id.keys() {
        visit(id, &by_id, &mut visited, &mut order)?;
    }
    Ok(order)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_json_sorts_keys_and_is_compact() {
        let first = BTreeMap::from([("z", 1), ("a", 2)]);
        let second = BTreeMap::from([("a", 2), ("z", 1)]);
        let first = canonical_json(&first).expect("canonical JSON");
        let second = canonical_json(&second).expect("canonical JSON");
        assert_eq!(first, r#"{"a":2,"z":1}"#);
        assert_eq!(first, second);
    }

    #[test]
    fn digest_is_stable_for_same_payload() {
        let release_units = vec![PlanReleaseUnit {
            id: "sample".to_owned(),
            old_version: "1.0.0".to_owned(),
            new_version: "1.1.0".to_owned(),
            bump: Bump::Minor,
            contributing_intent_ids: vec!["clear-river-1234".to_owned()],
            tag_ids: vec!["release-unit/sample/primary".to_owned()],
            release_notes: "## 1.1.0\n".to_owned(),
        }];
        let generator = Generator {
            tool: "intentional".to_owned(),
            version: "1.0.0".to_owned(),
        };
        let tags = Vec::new();
        let order = Vec::new();
        let payload = PlanPayload {
            contract: "contract-1",
            generator: &generator,
            channel: &None,
            release_units: &release_units,
            tags: &tags,
            tag_order: &order,
        };
        let json = canonical_json(&payload).expect("canonical payload");
        let first = format!("sha256:{:x}", Sha256::digest(json.as_bytes()));
        let second = format!("sha256:{:x}", Sha256::digest(json.as_bytes()));
        assert_eq!(first, second);
        assert_eq!(first.len(), "sha256:".len() + 64);
    }

    #[test]
    fn changelog_groups_breaking_features_and_fixes() {
        let entries = vec![
            ChangelogEntry {
                bump: Bump::Patch,
                text: "Correct a defect.".to_owned(),
            },
            ChangelogEntry {
                bump: Bump::Major,
                text: "Change a contract.".to_owned(),
            },
            ChangelogEntry {
                bump: Bump::Minor,
                text: "Add a capability.".to_owned(),
            },
        ];
        let rendered = render_changelog_section(&Version::new(2, 0, 0), &entries);
        assert_eq!(
            rendered,
            "## 2.0.0\n\n### Breaking\n\n- Change a contract.\n\n### Features\n\n- Add a capability.\n\n### Fixes\n\n- Correct a defect.\n"
        );
    }
}
