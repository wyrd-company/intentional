// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Deterministic release planning, changelog rendering, and digest sealing.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::intent::Intent;
use crate::model::Bump;
use crate::version::{aggregate_bumps, bump_version, effective_bumps, VersionRepository};
use semver::{Prerelease, Version};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// One package in a canonical release plan.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PlanPackage {
    /// Logical package id.
    pub id: String,
    /// Latest final tag-derived version.
    pub old_version: String,
    /// Intent-derived release version, optionally with a channel suffix.
    pub new_version: String,
    /// Effective aggregate bump.
    pub bump: Bump,
    /// Directly contributing intent ids.
    pub contributing_intent_ids: Vec<String>,
    /// Package tags to create.
    pub tags: Vec<String>,
    /// Deterministically rendered changelog section.
    pub release_notes: String,
}

/// Canonical digest-bound release plan.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ReleasePlan {
    /// SHA-256 digest of the canonical plan payload excluding this seal.
    pub digest: String,
    /// Changed packages ordered by id.
    pub packages: Vec<PlanPackage>,
    /// Dependency-ordered package ids.
    pub publication_order: Vec<String>,
    /// Optional global plain SemVer tag.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_tag: Option<String>,
}

#[derive(Serialize)]
struct PlanPayload<'a> {
    packages: &'a [PlanPackage],
    publication_order: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    global_tag: &'a Option<String>,
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
        if channel.is_some_and(str::is_empty) {
            return Err(Error::Validation("channel must not be empty".to_owned()));
        }
        let repository = VersionRepository::discover(root)?;
        let declared = aggregate_bumps(intents.iter().map(|intent| &intent.packages));
        let effective = effective_bumps(config, &declared);
        let changed: BTreeSet<_> = effective
            .iter()
            .filter(|(_, bump)| **bump != Bump::None)
            .map(|(id, _)| id.clone())
            .collect();
        let mut versions = BTreeMap::new();
        for id in &changed {
            let package = &config.packages[id];
            let current = repository.current_version(id, &package.tag)?;
            let base = bump_version(&current, effective[id]);
            let release = match channel {
                Some(channel) => channel_version(&repository, id, &package.tag, &base, channel)?,
                None => base,
            };
            versions.insert(id.clone(), (current, release));
        }

        let mut packages = Vec::with_capacity(changed.len());
        for id in &changed {
            let package = &config.packages[id];
            let (current, release) = &versions[id];
            let intent_ids = intents
                .iter()
                .filter(|intent| intent.packages.contains_key(id))
                .map(|intent| intent.id.clone())
                .collect::<Vec<_>>();
            let entries = note_entries(id, config, intents, &effective, &versions);
            let release_notes = render_changelog_section(release, &entries);
            packages.push(PlanPackage {
                id: id.clone(),
                old_version: current.to_string(),
                new_version: release.to_string(),
                bump: effective[id],
                contributing_intent_ids: intent_ids,
                tags: vec![render_tag(&package.tag, id, release)],
                release_notes,
            });
        }

        let publication_order = publication_order(config, &changed);
        let global_tag = if config.settings.global_tag {
            packages
                .iter()
                .filter_map(|package| Version::parse(&package.new_version).ok())
                .max()
                .map(|version| version.to_string())
        } else {
            None
        };
        let payload = PlanPayload {
            packages: &packages,
            publication_order: &publication_order,
            global_tag: &global_tag,
        };
        let payload_json = canonical_json(&payload)?;
        let digest = format!("sha256:{:x}", Sha256::digest(payload_json.as_bytes()));
        Ok(Self {
            digest,
            packages,
            publication_order,
            global_tag,
        })
    }

    /// Serialize this plan as compact canonical JSON with sorted object keys.
    pub fn to_canonical_json(&self) -> Result<String> {
        canonical_json(self)
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
    package_id: &str,
    template: &str,
    base: &Version,
    channel: &str,
) -> Result<Version> {
    let max_iteration = repository
        .all_versions(package_id, template)?
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
    package_id: &str,
    config: &Config,
    intents: &[Intent],
    effective: &BTreeMap<String, Bump>,
    versions: &BTreeMap<String, (Version, Version)>,
) -> Vec<ChangelogEntry> {
    let mut entries = intents
        .iter()
        .filter_map(|intent| {
            intent.packages.get(package_id).map(|bump| ChangelogEntry {
                bump: *bump,
                text: intent.message.clone(),
            })
        })
        .collect::<Vec<_>>();
    for dependency in &config.packages[package_id].depends_on {
        if effective[dependency] == Bump::None
            || !versions.contains_key(dependency)
            || !shares_ecosystem(config, package_id, dependency)
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
    config.packages[left]
        .projections
        .iter()
        .any(|left_projection| {
            left_projection.adapter.ecosystem().is_some()
                && config.packages[right]
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

fn publication_order(config: &Config, changed: &BTreeSet<String>) -> Vec<String> {
    fn visit(
        id: &str,
        config: &Config,
        changed: &BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        order: &mut Vec<String>,
    ) {
        if !visited.insert(id.to_owned()) {
            return;
        }
        for dependency in &config.packages[id].depends_on {
            if changed.contains(dependency) {
                visit(dependency, config, changed, visited, order);
            }
        }
        order.push(id.to_owned());
    }

    let mut visited = BTreeSet::new();
    let mut order = Vec::new();
    for id in changed {
        visit(id, config, changed, &mut visited, &mut order);
    }
    order
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
        let packages = vec![PlanPackage {
            id: "sample".to_owned(),
            old_version: "1.0.0".to_owned(),
            new_version: "1.1.0".to_owned(),
            bump: Bump::Minor,
            contributing_intent_ids: vec!["clear-river-1234".to_owned()],
            tags: vec!["sample@1.1.0".to_owned()],
            release_notes: "## 1.1.0\n".to_owned(),
        }];
        let order = vec!["sample".to_owned()];
        let global = None;
        let payload = PlanPayload {
            packages: &packages,
            publication_order: &order,
            global_tag: &global,
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
