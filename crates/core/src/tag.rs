// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Annotated release records and baseline establishment.

use crate::config::{Config, WorkspaceTagConfig};
use crate::error::{Error, Result};
use crate::intent::Intent;
use crate::model::{Adapter, Bump, ProjectionMode, TagPhase};
use crate::plan::{canonical_json, ReleasePlan};
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
        Self::build_with_plan(root, channel, phase, None)
    }

    /// Verify a supplied sealed plan or recover one locally, then plan annotated tags.
    pub fn build_with_plan(
        root: &Path,
        channel: Option<&str>,
        phase: Option<TagPhase>,
        plan_path: Option<&Path>,
    ) -> Result<Self> {
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
        verify_version_projections(root, &config, &versions)?;
        let digest = match plan_path {
            Some(path) => supplied_release_digest(root, &config, &versions, channel, path)?,
            None => match existing_tag_set_digest(&git, &config, &versions, false)? {
                Some(digest) => digest,
                None => recovered_release_digest(root, &config, &versions, channel)?,
            },
        };
        Self::from_versions(root, &config, &versions, phase, false, Some(&digest))
    }

    /// Infer and plan initial annotated baseline tags.
    pub fn build_baseline(root: &Path, explicit: &BTreeMap<String, Version>) -> Result<Self> {
        let config = Config::load(root)?;
        let repository = VersionRepository::discover(root)?;
        let git = gix::discover(root)
            .map_err(|error| Error::Git(format!("failed to discover repository: {error}")))?;
        let mut versions = BTreeMap::new();
        let mut incomplete = false;
        for (id, package) in &config.packages {
            let (_, primary) = config.primary_tag(id)?;
            if repository.has_matching_tag(id, &primary.template)? {
                let version = repository.current_version(id, &primary.template)?;
                let complete = package.tags.values().all(|tag| {
                    let name = render_tag(&tag.template, id, &version.to_string());
                    git.try_find_reference(format!("refs/tags/{name}").as_str())
                        .ok()
                        .flatten()
                        .is_some()
                });
                if !complete {
                    incomplete = true;
                    versions.insert(id.clone(), version.to_string());
                }
                continue;
            }
            incomplete = true;
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
            let canonical = Config::workspace_tag_id(id);
            if repository.has_matching_tag(id, &tag.template)? {
                continue;
            }
            incomplete = true;
            let version = explicit.get(&canonical).ok_or_else(|| {
                Error::Validation(format!(
                    "workspace tag {id} requires --version {canonical}=X.Y.Z"
                ))
            })?;
            versions.insert(canonical, version.to_string());
        }
        if !incomplete {
            return Err(Error::Validation(
                "all configured baseline tags already exist".to_owned(),
            ));
        }
        let digest = existing_tag_set_digest(&git, &config, &versions, true)?;
        Self::from_versions(root, &config, &versions, None, true, digest.as_deref())
    }

    fn from_versions(
        root: &Path,
        config: &Config,
        versions: &BTreeMap<String, String>,
        phase: Option<TagPhase>,
        baseline: bool,
        release_digest: Option<&str>,
    ) -> Result<Self> {
        let payload = TagDigestPayload {
            contract: &config.contract,
            generator: crate::VERSION,
            versions,
            baseline,
        };
        let baseline_digest = format!(
            "sha256:{:x}",
            Sha256::digest(canonical_json(&payload)?.as_bytes())
        );
        let digest = release_digest.unwrap_or(&baseline_digest);
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
        let mut existing = Vec::new();
        for id in order {
            let candidate = &selected[&id];
            if let Some(record) = read_tag_record(&repository, &candidate.name)? {
                validate_existing_candidate(
                    &candidate.name,
                    &record,
                    &id,
                    &candidate.version,
                    &config.contract,
                    digest,
                    baseline,
                    head,
                )?;
                existing.push(candidate.name.clone());
                continue;
            }
            if repository
                .try_find_reference(format!("refs/tags/{}", candidate.name).as_str())
                .map_err(|error| {
                    Error::Git(format!("failed to inspect tag {}: {error}", candidate.name))
                })?
                .is_some()
            {
                return Err(Error::Validation(format!(
                    "existing tag {} is not an annotated Intentional record",
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
                    digest,
                    &candidates[prerequisite].version,
                )?;
            }
            tags.push(PlannedTag {
                id: id.clone(),
                name: candidate.name.clone(),
                message: tag_message(&config.contract, digest, &id, &candidate.version, baseline),
            });
        }
        if tags.is_empty() {
            return Err(Error::Validation(format!(
                "tag {} already exists",
                existing.first().expect("selected tag set was non-empty")
            )));
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

#[allow(clippy::too_many_arguments)]
fn validate_existing_candidate(
    name: &str,
    record: &ParsedTagRecord,
    id: &str,
    version: &str,
    contract: &str,
    digest: &str,
    baseline: bool,
    head: gix::ObjectId,
) -> Result<()> {
    if record.target != head {
        return Err(Error::Validation(format!(
            "existing tag {name} targets a different commit"
        )));
    }
    let baseline = baseline.to_string();
    for (field, expected) in [
        ("tag-id", id),
        ("version", version),
        ("contract", contract),
        ("plan-digest", digest),
        ("baseline", baseline.as_str()),
    ] {
        if record.fields.get(field).map(String::as_str) != Some(expected) {
            return Err(Error::Validation(format!(
                "existing tag {name} has unexpected {field}"
            )));
        }
    }
    Ok(())
}

fn verify_version_projections(
    root: &Path,
    config: &Config,
    versions: &BTreeMap<String, String>,
) -> Result<()> {
    for (id, package) in &config.packages {
        let Some(expected) = versions.get(id) else {
            continue;
        };
        for projection in &package.projections {
            if projection.mode == ProjectionMode::None || projection.adapter == Adapter::Go {
                continue;
            }
            let relative = package.path.join(&projection.file);
            let text = std::fs::read_to_string(root.join(&relative))
                .map_err(|error| Error::io(root.join(&relative), error))?;
            let actual = read_projection_version(root, &relative, projection, &text)?;
            if &actual != expected {
                return Err(Error::Validation(format!(
                    "package {id} projection {} is {actual}, applied release is {expected}",
                    relative.display()
                )));
            }
        }
    }
    Ok(())
}

fn existing_tag_set_digest(
    repository: &gix::Repository,
    config: &Config,
    versions: &BTreeMap<String, String>,
    baseline: bool,
) -> Result<Option<String>> {
    let head = repository
        .head_id()
        .map_err(|error| Error::Git(format!("failed to resolve HEAD: {error}")))?
        .detach();
    let mut digest = None;
    for (package_id, package) in &config.packages {
        let Some(version) = versions.get(package_id) else {
            continue;
        };
        for (tag_id, tag) in &package.tags {
            let id = Config::package_tag_id(package_id, tag_id);
            collect_existing_digest(
                repository,
                &render_tag(&tag.template, package_id, version),
                &id,
                version,
                &config.contract,
                head,
                baseline,
                &mut digest,
            )?;
        }
    }
    for (tag_id, tag) in &config.workspace_tags {
        let id = Config::workspace_tag_id(tag_id);
        let Some(version) = versions.get(&id) else {
            continue;
        };
        collect_existing_digest(
            repository,
            &render_tag(&tag.template, tag_id, version),
            &id,
            version,
            &config.contract,
            head,
            baseline,
            &mut digest,
        )?;
    }
    Ok(digest)
}

fn supplied_release_digest(
    root: &Path,
    config: &Config,
    versions: &BTreeMap<String, String>,
    channel: Option<&str>,
    path: &Path,
) -> Result<String> {
    let text = std::fs::read_to_string(path).map_err(|error| Error::io(path, error))?;
    let plan: ReleasePlan = serde_json::from_str(&text)
        .map_err(|error| Error::Validation(format!("invalid release plan: {error}")))?;
    plan.verify_digest()?;
    if plan.contract != config.contract {
        return Err(Error::Validation(format!(
            "release plan contract {} does not match workspace {}",
            plan.contract, config.contract
        )));
    }
    if plan.generator.tool != "intentional" || plan.generator.version != crate::VERSION {
        return Err(Error::Validation(format!(
            "release plan generator {} {} does not match intentional {}",
            plan.generator.tool,
            plan.generator.version,
            crate::VERSION
        )));
    }
    if plan.channel.as_deref() != channel {
        return Err(Error::Validation(
            "release plan channel does not match tag invocation".to_owned(),
        ));
    }
    verify_plan_versions(versions, &plan)?;
    verify_materialized_release(root, config, &plan)?;
    let repository = gix::discover(root)
        .map_err(|error| Error::Git(format!("failed to discover repository: {error}")))?;
    match existing_tag_set_digest(&repository, config, versions, false)? {
        Some(existing) if existing != plan.digest => {
            return Err(Error::Validation(
                "supplied release plan disagrees with existing release records".to_owned(),
            ));
        }
        Some(_) => {}
        None => {
            let intents = match channel {
                Some(_) => Intent::load_all(root, config)?,
                None => recover_deleted_intents(root, config)?,
            };
            let expected = ReleasePlan::from_inputs(root, config, &intents, channel)?;
            if expected != plan {
                return Err(Error::Validation(
                    "supplied release plan does not match the release recovered from intents"
                        .to_owned(),
                ));
            }
        }
    }
    Ok(plan.digest)
}

#[allow(clippy::too_many_arguments)]
fn collect_existing_digest(
    repository: &gix::Repository,
    name: &str,
    expected_id: &str,
    expected_version: &str,
    expected_contract: &str,
    expected_target: gix::ObjectId,
    expected_baseline: bool,
    digest: &mut Option<String>,
) -> Result<()> {
    let Some(record) = read_tag_record(repository, name)? else {
        return Ok(());
    };
    let expected_baseline = expected_baseline.to_string();
    for (field, expected) in [
        ("tag-id", expected_id),
        ("version", expected_version),
        ("contract", expected_contract),
        ("baseline", expected_baseline.as_str()),
    ] {
        if record.fields.get(field).map(String::as_str) != Some(expected) {
            return Err(Error::Validation(format!(
                "release tag {name} has unexpected {field}"
            )));
        }
    }
    if record.target != expected_target {
        return Err(Error::Validation(format!(
            "release tag {name} targets a different commit"
        )));
    }
    let record_digest = record.fields["plan-digest"].clone();
    if digest
        .as_ref()
        .is_some_and(|digest| digest != &record_digest)
    {
        return Err(Error::Validation(
            "existing release tags disagree on plan-digest".to_owned(),
        ));
    }
    *digest = Some(record_digest);
    Ok(())
}

fn recovered_release_digest(
    root: &Path,
    config: &Config,
    versions: &BTreeMap<String, String>,
    channel: Option<&str>,
) -> Result<String> {
    let intents = match channel {
        Some(_) => Intent::load_all(root, config)?,
        None => recover_deleted_intents(root, config)?,
    };
    let plan = ReleasePlan::from_inputs(root, config, &intents, channel)?;
    verify_plan_versions(versions, &plan)?;
    verify_materialized_release(root, config, &plan)?;
    Ok(plan.digest)
}

fn verify_plan_versions(versions: &BTreeMap<String, String>, plan: &ReleasePlan) -> Result<()> {
    let mut planned_versions = BTreeMap::new();
    for tag in &plan.tags {
        let key = tag.package.as_ref().unwrap_or(&tag.id);
        if let Some(existing) = planned_versions.insert(key.clone(), tag.version.clone()) {
            if existing != tag.version {
                return Err(Error::Validation(format!(
                    "release plan has conflicting versions for {key}"
                )));
            }
        }
    }
    if &planned_versions != versions {
        return Err(Error::Validation(format!(
            "applied release does not match recovered release plan: expected {planned_versions:?}, found {versions:?}"
        )));
    }
    Ok(())
}

fn verify_materialized_release(root: &Path, config: &Config, plan: &ReleasePlan) -> Result<()> {
    for planned in &plan.packages {
        let package = &config.packages[&planned.id];
        let changelog_path = root.join(&package.path).join("CHANGELOG.md");
        let changelog = std::fs::read_to_string(&changelog_path)
            .map_err(|error| Error::io(&changelog_path, error))?;
        if leading_changelog_version(&changelog)?
            .as_ref()
            .map(Version::to_string)
            != Some(planned.new_version.clone())
        {
            return Err(Error::Validation(format!(
                "{} leading changelog version does not match recovered plan {}",
                planned.id, planned.new_version
            )));
        }
        for projection in &package.projections {
            if projection.mode == ProjectionMode::None || projection.adapter == Adapter::Go {
                continue;
            }
            let relative = package.path.join(&projection.file);
            let text = std::fs::read_to_string(root.join(&relative))
                .map_err(|error| Error::io(root.join(&relative), error))?;
            let actual = read_projection_version(root, &relative, projection, &text)?;
            if actual != planned.new_version {
                return Err(Error::Validation(format!(
                    "{} projection {} is {actual}, recovered plan requires {}",
                    planned.id,
                    relative.display(),
                    planned.new_version
                )));
            }
        }
    }
    Ok(())
}

fn recover_deleted_intents(root: &Path, config: &Config) -> Result<Vec<Intent>> {
    let repository = gix::discover(root)
        .map_err(|error| Error::Git(format!("failed to discover repository: {error}")))?;
    let head = repository
        .head_commit()
        .map_err(|error| Error::Git(format!("failed to resolve HEAD commit: {error}")))?;
    let parent = head
        .parent_ids()
        .next()
        .ok_or_else(|| Error::Validation("release commit has no parent".to_owned()))?
        .object()
        .map_err(|error| Error::Git(format!("failed to read release parent: {error}")))?
        .try_into_commit()
        .map_err(|error| Error::Git(format!("release parent is not a commit: {error}")))?;
    let old_tree = parent
        .tree()
        .map_err(|error| Error::Git(format!("failed to read release parent tree: {error}")))?;
    let new_tree = head
        .tree()
        .map_err(|error| Error::Git(format!("failed to read release tree: {error}")))?;
    let mut deleted = Vec::<(std::path::PathBuf, Vec<u8>)>::new();
    let mut changes = old_tree
        .changes()
        .map_err(|error| Error::Git(format!("failed to configure release diff: {error}")))?;
    changes.options(|options| {
        options.track_rewrites(None);
    });
    changes
        .for_each_to_obtain_tree(&new_tree, |change| {
            if let gix::object::tree::diff::Change::Deletion { location, id, .. } = change {
                let path = std::path::PathBuf::from(String::from_utf8_lossy(location).into_owned());
                if path.starts_with(crate::intent::INTENTS_PATH)
                    && path.extension().is_some_and(|extension| extension == "md")
                {
                    let object = id
                        .object()
                        .map_err(|error| std::io::Error::other(error.to_string()))?;
                    deleted.push((path, object.data.clone()));
                }
            }
            Ok::<_, std::io::Error>(gix::object::tree::diff::Action::Continue)
        })
        .map_err(|error| Error::Git(format!("failed to inspect release diff: {error}")))?;
    deleted.sort_by(|left, right| left.0.cmp(&right.0));
    if deleted.is_empty() {
        return Err(Error::Validation(
            "release commit does not delete any Intentional intents; supply an applied release commit"
                .to_owned(),
        ));
    }
    deleted
        .into_iter()
        .map(|(path, bytes)| {
            let text = std::str::from_utf8(&bytes).map_err(|error| {
                Error::Validation(format!(
                    "deleted intent {} is not UTF-8: {error}",
                    path.display()
                ))
            })?;
            Intent::parse(&path, text, config)
        })
        .collect()
}

/// Validate annotated package tag sets without rewriting recoverable omissions.
pub fn tag_record_issues(root: &Path, config: &Config) -> Result<Vec<String>> {
    let versions = VersionRepository::discover(root)?;
    let repository = gix::discover(root)
        .map_err(|error| Error::Git(format!("failed to discover repository: {error}")))?;
    let mut issues = Vec::new();
    for (package_id, package) in &config.packages {
        let (primary_id, primary) = config.primary_tag(package_id)?;
        if !versions.has_matching_tag(package_id, &primary.template)? {
            continue;
        }
        let version = versions.current_version(package_id, &primary.template)?;
        let primary_name = render_tag(&primary.template, package_id, &version.to_string());
        let Some(primary_record) = read_tag_record(&repository, &primary_name)? else {
            // Lightweight history predating contract-aware records remains readable.
            continue;
        };
        for (field, expected) in [
            ("tag-id", Config::package_tag_id(package_id, primary_id)),
            ("version", version.to_string()),
            ("contract", config.contract.clone()),
        ] {
            if primary_record.fields.get(field) != Some(&expected) {
                issues.push(format!(
                    "package {package_id} primary tag {primary_name} has unexpected {field}"
                ));
            }
        }
        for (tag_id, tag) in &package.tags {
            let name = render_tag(&tag.template, package_id, &version.to_string());
            let Some(record) = read_tag_record(&repository, &name)? else {
                issues.push(format!(
                    "package {package_id} is missing annotated projection tag {name} for {version}"
                ));
                continue;
            };
            let expected_tag_id = Config::package_tag_id(package_id, tag_id);
            if record.fields.get("tag-id") != Some(&expected_tag_id) {
                issues.push(format!(
                    "package {package_id} tag {name} has unexpected tag-id"
                ));
            }
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
    for required in [
        "contract",
        "generator",
        "plan-digest",
        "tag-id",
        "version",
        "baseline",
    ] {
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
    version: &str,
) -> Result<()> {
    let record = read_tag_record(repository, tag_name)?.ok_or_else(|| {
        Error::Validation(format!(
            "missing prerequisite tag {prerequisite} or it is not annotated"
        ))
    })?;
    if record.target != head {
        return Err(Error::Validation(format!(
            "prerequisite tag {prerequisite} targets a different commit"
        )));
    }
    for (field, expected) in [
        ("contract", contract),
        ("plan-digest", digest),
        ("tag-id", prerequisite),
        ("version", version),
    ] {
        if record.fields.get(field).map(String::as_str) != Some(expected) {
            return Err(Error::Validation(format!(
                "prerequisite tag {prerequisite} has unexpected {field}"
            )));
        }
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
