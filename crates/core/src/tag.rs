// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Annotated release records and baseline establishment.

use crate::config::{Config, WorkspaceTagConfig};
use crate::error::{Error, Result};
use crate::model::{Adapter, Bump, ProjectionMode, TagPhase};
use crate::plan::canonical_json;
use crate::status::read_projection_version;
use crate::version::{bump_version_with_mapping, VersionRepository};
use semver::{Prerelease, Version};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// One annotated Git tag to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedTag {
    /// Canonical configured tag id.
    pub id: String,
    /// Rendered Git tag name.
    pub name: String,
    /// Canonical annotated tag message.
    pub message: String,
}

/// Planned annotated tags for an applied release or baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagResult {
    /// Tags in prerequisite-safe order.
    pub tags: Vec<PlannedTag>,
}

#[derive(Serialize)]
struct TagDigestPayload<'a> {
    contract: &'a str,
    generator: &'a str,
    versions: &'a BTreeMap<String, String>,
    baseline: bool,
}

#[derive(Debug, Clone)]
struct TagCandidate {
    name: String,
    version: String,
    required_phase: Option<TagPhase>,
    prerequisites: Vec<String>,
}

impl TagResult {
    /// Recover an applied release and plan its annotated tags.
    pub fn build(root: &Path, channel: Option<&str>, phase: Option<TagPhase>) -> Result<Self> {
        let config = Config::load(root)?;
        let repository = VersionRepository::discover(root)?;
        let git = gix::discover(root)
            .map_err(|error| Error::Git(format!("failed to discover repository: {error}")))?;
        let mut versions_by_package = BTreeMap::new();
        let mut max_bump = Bump::None;
        for (id, package) in &config.packages {
            let changelog_path = root.join(&package.path).join("CHANGELOG.md");
            let Ok(changelog) = std::fs::read_to_string(&changelog_path) else {
                continue;
            };
            let Some(version) = leading_changelog_version(&changelog)? else {
                continue;
            };
            let matches_channel = match channel {
                Some(channel) => channel_iteration(&version, channel).is_some(),
                None => version.pre.is_empty(),
            };
            if !matches_channel {
                continue;
            }
            let (_, primary) = config.primary_tag(id)?;
            let current = repository.current_version(id, &primary.template)?;
            if version < current {
                continue;
            }
            if version == current {
                let all_exist = package.tags.values().all(|tag| {
                    let name = render_tag(&tag.template, id, &version.to_string());
                    git.try_find_reference(format!("refs/tags/{name}").as_str())
                        .ok()
                        .flatten()
                        .is_some()
                });
                if all_exist {
                    continue;
                }
            } else {
                max_bump = max_bump.max(infer_bump(&current, &version));
            }
            versions_by_package.insert(id.clone(), version);
        }
        if versions_by_package.is_empty() {
            return Err(Error::Validation(
                "no applied release is available to tag".to_owned(),
            ));
        }

        let mut versions = versions_by_package
            .iter()
            .map(|(id, version)| (id.clone(), version.to_string()))
            .collect::<BTreeMap<_, _>>();
        for (id, tag) in &config.workspace_tags {
            let current = repository.current_version(id, &tag.template)?;
            let base =
                bump_version_with_mapping(&current, max_bump, config.settings.pre_1_0_bump_mapping);
            let version = match channel {
                Some(channel) => workspace_channel_version(&repository, id, tag, &base, channel)?,
                None => base,
            };
            versions.insert(Config::workspace_tag_id(id), version.to_string());
        }
        Self::from_versions(root, &config, &versions, phase, false)
    }

    /// Infer and plan initial annotated baseline tags.
    pub fn build_baseline(root: &Path, explicit: &BTreeMap<String, Version>) -> Result<Self> {
        let config = Config::load(root)?;
        let repository = VersionRepository::discover(root)?;
        let mut versions = BTreeMap::new();
        for (id, package) in &config.packages {
            let (_, primary) = config.primary_tag(id)?;
            if repository.has_matching_tag(id, &primary.template)? {
                continue;
            }
            let mut evidence = Vec::new();
            for projection in &package.projections {
                if projection.mode == ProjectionMode::None || projection.adapter == Adapter::Go {
                    continue;
                }
                let relative = package.path.join(&projection.file);
                let text = std::fs::read_to_string(root.join(&relative))
                    .map_err(|error| Error::io(root.join(&relative), error))?;
                evidence.push((
                    relative.clone(),
                    read_projection_version(root, &relative, projection, &text)?,
                ));
            }
            let version = match (evidence.first(), explicit.get(id)) {
                (None, None) => {
                    return Err(Error::Validation(format!(
                        "tag-only package {id} requires --version {id}=X.Y.Z"
                    )))
                }
                (None, Some(version)) => version.clone(),
                (Some((_, first)), explicit_version) => {
                    if evidence.iter().any(|(_, version)| version != first) {
                        let detail = evidence
                            .iter()
                            .map(|(path, version)| format!("{}={version}", path.display()))
                            .collect::<Vec<_>>()
                            .join(", ");
                        return Err(Error::Validation(format!(
                            "baseline projections disagree for {id}: {detail}"
                        )));
                    }
                    let inferred = Version::parse(first)?;
                    if explicit_version.is_some_and(|version| version != &inferred) {
                        return Err(Error::Validation(format!(
                            "explicit baseline for {id} disagrees with projection evidence {inferred}"
                        )));
                    }
                    inferred
                }
            };
            versions.insert(id.clone(), version.to_string());
        }
        for (id, tag) in &config.workspace_tags {
            if repository.has_matching_tag(id, &tag.template)? {
                continue;
            }
            let canonical = Config::workspace_tag_id(id);
            let version = explicit.get(&canonical).ok_or_else(|| {
                Error::Validation(format!(
                    "workspace tag {id} requires --version {canonical}=X.Y.Z"
                ))
            })?;
            versions.insert(canonical, version.to_string());
        }
        if versions.is_empty() {
            return Err(Error::Validation(
                "all configured baseline tags already exist".to_owned(),
            ));
        }
        Self::from_versions(root, &config, &versions, None, true)
    }

    fn from_versions(
        root: &Path,
        config: &Config,
        versions: &BTreeMap<String, String>,
        phase: Option<TagPhase>,
        baseline: bool,
    ) -> Result<Self> {
        let payload = TagDigestPayload {
            contract: &config.contract,
            generator: crate::VERSION,
            versions,
            baseline,
        };
        let digest = format!(
            "sha256:{:x}",
            Sha256::digest(canonical_json(&payload)?.as_bytes())
        );
        let mut candidates = BTreeMap::<String, TagCandidate>::new();
        for (package_id, version) in versions {
            let Some(package) = config.packages.get(package_id) else {
                continue;
            };
            for (tag_id, tag) in &package.tags {
                let canonical = Config::package_tag_id(package_id, tag_id);
                candidates.insert(
                    canonical,
                    TagCandidate {
                        name: render_tag(&tag.template, package_id, version),
                        version: version.clone(),
                        required_phase: tag.require_phase,
                        prerequisites: tag.tag_after.clone(),
                    },
                );
            }
        }
        for (tag_id, tag) in &config.workspace_tags {
            let canonical = Config::workspace_tag_id(tag_id);
            let Some(version) = versions.get(&canonical) else {
                continue;
            };
            candidates.insert(
                canonical,
                TagCandidate {
                    name: render_tag(&tag.template, tag_id, version),
                    version: version.clone(),
                    required_phase: tag.require_phase,
                    prerequisites: tag.tag_after.clone(),
                },
            );
        }
        let selected = if baseline {
            candidates.clone()
        } else {
            candidates
                .iter()
                .filter(|(_, candidate)| candidate.required_phase == phase)
                .map(|(id, candidate)| (id.clone(), candidate.clone()))
                .collect::<BTreeMap<_, _>>()
        };
        if selected.is_empty() {
            return Err(Error::Validation(match phase {
                Some(phase) => format!("no release tags require --phase {phase}"),
                None => "no unphased release tags are available".to_owned(),
            }));
        }
        let order = order_tags(&selected)?;
        let repository = gix::discover(root)
            .map_err(|error| Error::Git(format!("failed to discover repository: {error}")))?;
        let head = repository
            .head_id()
            .map_err(|error| Error::Git(format!("failed to resolve HEAD: {error}")))?
            .detach();
        let mut tags = Vec::new();
        for id in order {
            let candidate = &selected[&id];
            if repository
                .try_find_reference(format!("refs/tags/{}", candidate.name).as_str())
                .map_err(|error| {
                    Error::Git(format!("failed to inspect tag {}: {error}", candidate.name))
                })?
                .is_some()
            {
                return Err(Error::Validation(format!(
                    "tag {} already exists",
                    candidate.name
                )));
            }
            for prerequisite in &candidate.prerequisites {
                if selected.contains_key(prerequisite) {
                    continue;
                }
                let prerequisite_name = candidates
                    .get(prerequisite)
                    .map(|entry| entry.name.as_str())
                    .ok_or_else(|| {
                        Error::Validation(format!(
                            "release tag {id} requires unavailable tag {prerequisite}"
                        ))
                    })?;
                verify_existing_prerequisite(
                    &repository,
                    prerequisite,
                    prerequisite_name,
                    head,
                    &config.contract,
                    &digest,
                )?;
            }
            tags.push(PlannedTag {
                id: id.clone(),
                name: candidate.name.clone(),
                message: tag_message(&config.contract, &digest, &id, &candidate.version, baseline),
            });
        }
        Ok(Self { tags })
    }

    /// Human-readable operations printed identically for dry and real runs.
    pub fn operations(&self) -> Vec<String> {
        self.tags
            .iter()
            .map(|tag| format!("create annotated tag {}", tag.name))
            .collect()
    }

    /// Create annotated tags at HEAD unless `dry_run` is enabled.
    pub fn apply(&self, root: &Path, dry_run: bool) -> Result<()> {
        if dry_run {
            return Ok(());
        }
        let repository = gix::discover(root)
            .map_err(|error| Error::Git(format!("failed to discover repository: {error}")))?;
        let head = repository
            .head_id()
            .map_err(|error| Error::Git(format!("failed to resolve HEAD: {error}")))?
            .detach();
        for tag in &self.tags {
            repository
                .tag(
                    &tag.name,
                    head,
                    gix::object::Kind::Commit,
                    None,
                    &tag.message,
                    gix::refs::transaction::PreviousValue::MustNotExist,
                )
                .map_err(|error| {
                    Error::Git(format!(
                        "failed to create annotated tag {}: {error}",
                        tag.name
                    ))
                })?;
        }
        Ok(())
    }
}

/// Validate annotated package tag sets without rewriting recoverable omissions.
pub fn tag_record_issues(root: &Path, config: &Config) -> Result<Vec<String>> {
    let versions = VersionRepository::discover(root)?;
    let repository = gix::discover(root)
        .map_err(|error| Error::Git(format!("failed to discover repository: {error}")))?;
    let mut issues = Vec::new();
    for (package_id, package) in &config.packages {
        let (_, primary) = config.primary_tag(package_id)?;
        if !versions.has_matching_tag(package_id, &primary.template)? {
            continue;
        }
        let version = versions.current_version(package_id, &primary.template)?;
        let primary_name = render_tag(&primary.template, package_id, &version.to_string());
        let Some(primary_record) = read_tag_record(&repository, &primary_name)? else {
            // Lightweight history predating contract-aware records remains readable.
            continue;
        };
        for tag in package.tags.values() {
            let name = render_tag(&tag.template, package_id, &version.to_string());
            let Some(record) = read_tag_record(&repository, &name)? else {
                issues.push(format!(
                    "package {package_id} is missing annotated projection tag {name} for {version}"
                ));
                continue;
            };
            for field in ["contract", "generator", "plan-digest", "version"] {
                if record.fields.get(field) != primary_record.fields.get(field) {
                    issues.push(format!(
                        "package {package_id} tag {name} disagrees with primary tag {primary_name} on {field}"
                    ));
                }
            }
            if record.target != primary_record.target {
                issues.push(format!(
                    "package {package_id} tag {name} targets a different commit than {primary_name}"
                ));
            }
        }
    }
    Ok(issues)
}

struct ParsedTagRecord {
    target: gix::ObjectId,
    fields: BTreeMap<String, String>,
}

fn read_tag_record(repository: &gix::Repository, name: &str) -> Result<Option<ParsedTagRecord>> {
    let Some(mut reference) = repository
        .try_find_reference(format!("refs/tags/{name}").as_str())
        .map_err(|error| Error::Git(format!("failed to inspect tag {name}: {error}")))?
    else {
        return Ok(None);
    };
    let object_id = reference
        .try_id()
        .ok_or_else(|| Error::Validation(format!("tag {name} is symbolic")))?
        .detach();
    let target = reference
        .peel_to_id()
        .map_err(|error| Error::Git(format!("failed to peel tag {name}: {error}")))?
        .detach();
    let object = repository
        .find_object(object_id)
        .map_err(|error| Error::Git(format!("failed to read tag {name}: {error}")))?;
    let Ok(tag) = object.try_into_tag() else {
        return Ok(None);
    };
    let decoded = tag
        .decode()
        .map_err(|error| Error::Git(format!("failed to decode tag {name}: {error}")))?;
    let fields = decoded
        .message
        .to_string()
        .lines()
        .filter_map(|line| line.split_once(": "))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect::<BTreeMap<_, _>>();
    for required in ["contract", "generator", "plan-digest", "tag-id", "version"] {
        if !fields.contains_key(required) {
            return Err(Error::Validation(format!(
                "annotated tag {name} is missing Intentional record field {required}"
            )));
        }
    }
    Ok(Some(ParsedTagRecord { target, fields }))
}

fn verify_existing_prerequisite(
    repository: &gix::Repository,
    prerequisite: &str,
    tag_name: &str,
    head: gix::ObjectId,
    contract: &str,
    digest: &str,
) -> Result<()> {
    let mut reference = repository
        .try_find_reference(format!("refs/tags/{tag_name}").as_str())
        .map_err(|error| {
            Error::Git(format!(
                "failed to inspect prerequisite {prerequisite}: {error}"
            ))
        })?
        .ok_or_else(|| Error::Validation(format!("missing prerequisite tag {prerequisite}")))?;
    let object_id = reference
        .try_id()
        .ok_or_else(|| Error::Validation(format!("prerequisite tag {prerequisite} is symbolic")))?
        .detach();
    let target = reference
        .peel_to_id()
        .map_err(|error| {
            Error::Git(format!(
                "failed to peel prerequisite {prerequisite}: {error}"
            ))
        })?
        .detach();
    if target != head {
        return Err(Error::Validation(format!(
            "prerequisite tag {prerequisite} targets a different commit"
        )));
    }
    let object = repository.find_object(object_id).map_err(|error| {
        Error::Git(format!(
            "failed to read prerequisite {prerequisite}: {error}"
        ))
    })?;
    let tag = object.try_into_tag().map_err(|_| {
        Error::Validation(format!("prerequisite tag {prerequisite} is not annotated"))
    })?;
    let decoded = tag.decode().map_err(|error| {
        Error::Git(format!(
            "failed to decode prerequisite {prerequisite}: {error}"
        ))
    })?;
    let message = decoded.message.to_string();
    if !message.contains(&format!("contract: {contract}\n"))
        || !message.contains(&format!("plan-digest: {digest}\n"))
    {
        return Err(Error::Validation(format!(
            "prerequisite tag {prerequisite} does not match this release contract and plan"
        )));
    }
    Ok(())
}

fn order_tags(candidates: &BTreeMap<String, TagCandidate>) -> Result<Vec<String>> {
    fn visit(
        id: &str,
        candidates: &BTreeMap<String, TagCandidate>,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        order: &mut Vec<String>,
    ) -> Result<()> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id.to_owned()) {
            return Err(Error::Validation(format!("tag-order cycle includes {id}")));
        }
        for prerequisite in &candidates[id].prerequisites {
            if candidates.contains_key(prerequisite) {
                visit(prerequisite, candidates, visiting, visited, order)?;
            }
        }
        visiting.remove(id);
        visited.insert(id.to_owned());
        order.push(id.to_owned());
        Ok(())
    }
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut order = Vec::new();
    for id in candidates.keys() {
        visit(id, candidates, &mut visiting, &mut visited, &mut order)?;
    }
    Ok(order)
}

fn tag_message(contract: &str, digest: &str, id: &str, version: &str, baseline: bool) -> String {
    format!(
        "intentional release record\n\ncontract: {contract}\ngenerator: intentional {}\nplan-digest: {digest}\ntag-id: {id}\nversion: {version}\nbaseline: {baseline}\n",
        crate::VERSION
    )
}

fn render_tag(template: &str, id: &str, version: &str) -> String {
    template.replace("{id}", id).replace("{version}", version)
}

fn leading_changelog_version(changelog: &str) -> Result<Option<Version>> {
    let Some(heading) = changelog.lines().find(|line| line.starts_with("## ")) else {
        return Ok(None);
    };
    Version::parse(heading.trim_start_matches("## ").trim())
        .map(Some)
        .map_err(Error::from)
}

fn channel_iteration(version: &Version, channel: &str) -> Option<u64> {
    let (name, iteration) = version.pre.as_str().split_once('.')?;
    (name == channel).then(|| iteration.parse().ok()).flatten()
}

fn infer_bump(current: &Version, applied: &Version) -> Bump {
    if applied.major > current.major {
        Bump::Major
    } else if applied.minor > current.minor {
        Bump::Minor
    } else if applied.patch > current.patch {
        Bump::Patch
    } else {
        Bump::None
    }
}

fn workspace_channel_version(
    repository: &VersionRepository,
    id: &str,
    tag: &WorkspaceTagConfig,
    base: &Version,
    channel: &str,
) -> Result<Version> {
    let iteration = repository
        .all_versions(id, &tag.template)?
        .into_iter()
        .filter(|version| {
            version.major == base.major
                && version.minor == base.minor
                && version.patch == base.patch
        })
        .filter_map(|version| channel_iteration(&version, channel))
        .max()
        .unwrap_or(0)
        + 1;
    let mut version = base.clone();
    version.pre = Prerelease::new(&format!("{channel}.{iteration}"))?;
    Ok(version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_leading_release_section() {
        let changelog = "# Changelog\n\n## 2.1.0\n\nNotes.\n\n## 2.0.0\n";
        assert_eq!(
            leading_changelog_version(changelog).expect("valid changelog"),
            Some(Version::new(2, 1, 0))
        );
    }

    #[test]
    fn canonical_message_contains_release_evidence() {
        let message = tag_message(
            "contract-1",
            "sha256:abc",
            "package/sample/primary",
            "1.2.0",
            false,
        );
        assert!(message.contains("contract: contract-1"));
        assert!(message.contains("plan-digest: sha256:abc"));
        assert!(message.contains("version: 1.2.0"));
    }
}
