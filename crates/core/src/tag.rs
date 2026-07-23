// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Annotated release records and baseline establishment.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::intent::Intent;
use crate::model::{Adapter, ProjectionMode, TagPhase};
use crate::plan::{canonical_json, ReleasePlan};
use crate::status::read_projection_version;
use crate::version::VersionRepository;
use semver::Version;
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
        let mut versions_by_release_unit = BTreeMap::new();
        for (id, release_unit) in &config.release_units {
            let changelog_path = root.join(&release_unit.path).join("CHANGELOG.md");
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
            versions_by_release_unit.insert(id.clone(), version);
        }
        if versions_by_release_unit.is_empty() {
            return Err(Error::Validation(
                "no applied release is available to tag".to_owned(),
            ));
        }

        let release_unit_versions = versions_by_release_unit
            .iter()
            .map(|(id, version)| (id.clone(), version.to_string()))
            .collect::<BTreeMap<_, _>>();
        verify_version_projections(root, &config, &release_unit_versions)?;
        let plan = match plan_path {
            Some(path) => {
                supplied_release_plan(root, &config, &release_unit_versions, channel, path)?
            }
            None => recovered_release_plan(root, &config, &release_unit_versions, channel)?,
        };
        let versions = plan_versions(&plan)?;
        match existing_tag_set_digest(&git, &config, &versions, false)? {
            Some(existing) if existing != plan.digest => {
                return Err(Error::Validation(
                    "release plan disagrees with existing release records".to_owned(),
                ));
            }
            _ => {}
        }
        Self::from_versions(root, &config, &versions, phase, false, Some(&plan.digest))
    }

    /// Infer and plan initial annotated baseline tags.
    pub fn build_baseline(root: &Path, explicit: &BTreeMap<String, Version>) -> Result<Self> {
        let config = Config::load(root)?;
        let repository = VersionRepository::discover(root)?;
        let git = gix::discover(root)
            .map_err(|error| Error::Git(format!("failed to discover repository: {error}")))?;
        let mut versions = BTreeMap::new();
        for (id, release_unit) in &config.release_units {
            let (_, primary) = config.primary_tag(id)?;
            if repository.has_matching_tag(id, &primary.template)? {
                let version = repository.current_version(id, &primary.template)?;
                versions.insert(id.clone(), version.to_string());
                continue;
            }
            let mut evidence = Vec::new();
            for projection in &release_unit.projections {
                if projection.mode == ProjectionMode::None || projection.adapter == Adapter::Go {
                    continue;
                }
                let relative = release_unit.path.join(&projection.file);
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
                        "tag-only release unit {id} requires --version {id}=X.Y.Z"
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
                let version = repository.current_version(id, &tag.template)?;
                versions.insert(canonical, version.to_string());
                continue;
            }
            let version = explicit.get(&canonical).ok_or_else(|| {
                Error::Validation(format!(
                    "workspace tag {id} requires --version {canonical}=X.Y.Z"
                ))
            })?;
            versions.insert(canonical, version.to_string());
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
        for (release_unit_id, version) in versions {
            let Some(release_unit) = config.release_units.get(release_unit_id) else {
                continue;
            };
            for (tag_id, tag) in &release_unit.tags {
                let canonical = Config::release_unit_tag_id(release_unit_id, tag_id);
                candidates.insert(
                    canonical,
                    TagCandidate {
                        name: render_tag(&tag.template, release_unit_id, version),
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
    for (id, release_unit) in &config.release_units {
        let Some(expected) = versions.get(id) else {
            continue;
        };
        for projection in &release_unit.projections {
            if projection.mode == ProjectionMode::None || projection.adapter == Adapter::Go {
                continue;
            }
            let relative = release_unit.path.join(&projection.file);
            let text = std::fs::read_to_string(root.join(&relative))
                .map_err(|error| Error::io(root.join(&relative), error))?;
            let actual = read_projection_version(root, &relative, projection, &text)?;
            if &actual != expected {
                return Err(Error::Validation(format!(
                    "release unit {id} projection {} is {actual}, applied release is {expected}",
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
    for (release_unit_id, release_unit) in &config.release_units {
        let Some(version) = versions.get(release_unit_id) else {
            continue;
        };
        for (tag_id, tag) in &release_unit.tags {
            let id = Config::release_unit_tag_id(release_unit_id, tag_id);
            collect_existing_digest(
                repository,
                &render_tag(&tag.template, release_unit_id, version),
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

fn existing_head_release_digest(
    repository: &gix::Repository,
    config: &Config,
) -> Result<Option<String>> {
    let head = repository
        .head_id()
        .map_err(|error| Error::Git(format!("failed to resolve HEAD: {error}")))?
        .detach();
    let mut canonical_ids = config
        .release_units
        .iter()
        .flat_map(|(release_unit_id, release_unit)| {
            release_unit
                .tags
                .keys()
                .map(|tag_id| Config::release_unit_tag_id(release_unit_id, tag_id))
        })
        .collect::<BTreeSet<_>>();
    canonical_ids.extend(
        config
            .workspace_tags
            .keys()
            .map(|tag_id| Config::workspace_tag_id(tag_id)),
    );
    let references = repository
        .references()
        .map_err(|error| Error::Git(format!("failed to read references: {error}")))?;
    let tags = references
        .tags()
        .map_err(|error| Error::Git(format!("failed to read tags: {error}")))?;
    let mut digest = None;
    for reference in tags.flatten() {
        let name = reference.name().shorten().to_string();
        let Some(record) = read_tag_record(repository, &name)? else {
            continue;
        };
        let relevant = record.target == head
            && record.fields.get("contract") == Some(&config.contract)
            && record.fields.get("baseline").map(String::as_str) == Some("false")
            && record
                .fields
                .get("tag-id")
                .is_some_and(|id| canonical_ids.contains(id));
        if !relevant {
            continue;
        }
        let record_digest = record.fields.get("plan-digest").cloned().ok_or_else(|| {
            Error::Validation(format!("existing release tag {name} has no plan-digest"))
        })?;
        if digest
            .as_ref()
            .is_some_and(|existing| existing != &record_digest)
        {
            return Err(Error::Validation(
                "existing release tags at HEAD disagree on plan-digest".to_owned(),
            ));
        }
        digest = Some(record_digest);
    }
    Ok(digest)
}

fn supplied_release_plan(
    root: &Path,
    config: &Config,
    versions: &BTreeMap<String, String>,
    channel: Option<&str>,
    path: &Path,
) -> Result<ReleasePlan> {
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
    verify_plan_release_unit_versions(versions, &plan)?;
    verify_materialized_release(root, config, &plan)?;
    let repository = gix::discover(root)
        .map_err(|error| Error::Git(format!("failed to discover repository: {error}")))?;
    let partial_digest = existing_head_release_digest(&repository, config)?;
    let plan_versions = plan_versions(&plan)?;
    match existing_tag_set_digest(&repository, config, &plan_versions, false)? {
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
            let expected = match partial_digest {
                Some(_) => ReleasePlan::from_inputs_before(
                    root,
                    config,
                    &intents,
                    channel,
                    Some(
                        repository
                            .head_id()
                            .map_err(|error| {
                                Error::Git(format!("failed to resolve HEAD: {error}"))
                            })?
                            .detach(),
                    ),
                )?,
                None => ReleasePlan::from_inputs(root, config, &intents, channel)?,
            };
            if expected != plan {
                return Err(Error::Validation(
                    "supplied release plan does not match the release recovered from intents"
                        .to_owned(),
                ));
            }
        }
    }
    Ok(plan)
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

fn recovered_release_plan(
    root: &Path,
    config: &Config,
    versions: &BTreeMap<String, String>,
    channel: Option<&str>,
) -> Result<ReleasePlan> {
    let intents = match channel {
        Some(_) => Intent::load_all(root, config)?,
        None => recover_deleted_intents(root, config)?,
    };
    let repository = gix::discover(root)
        .map_err(|error| Error::Git(format!("failed to discover repository: {error}")))?;
    let partial_digest = existing_head_release_digest(&repository, config)?;
    let plan = match &partial_digest {
        Some(_) => ReleasePlan::from_inputs_before(
            root,
            config,
            &intents,
            channel,
            Some(
                repository
                    .head_id()
                    .map_err(|error| Error::Git(format!("failed to resolve HEAD: {error}")))?
                    .detach(),
            ),
        )?,
        None => ReleasePlan::from_inputs(root, config, &intents, channel)?,
    };
    if partial_digest
        .as_ref()
        .is_some_and(|digest| digest != &plan.digest)
    {
        return Err(Error::Validation(
            "existing release records disagree with the recovered release plan".to_owned(),
        ));
    }
    verify_plan_release_unit_versions(versions, &plan)?;
    verify_materialized_release(root, config, &plan)?;
    Ok(plan)
}

fn plan_versions(plan: &ReleasePlan) -> Result<BTreeMap<String, String>> {
    let mut planned_versions = BTreeMap::new();
    for tag in &plan.tags {
        let key = tag.release_unit.as_ref().unwrap_or(&tag.id);
        if let Some(existing) = planned_versions.insert(key.clone(), tag.version.clone()) {
            if existing != tag.version {
                return Err(Error::Validation(format!(
                    "release plan has conflicting versions for {key}"
                )));
            }
        }
    }
    Ok(planned_versions)
}

fn verify_plan_release_unit_versions(
    versions: &BTreeMap<String, String>,
    plan: &ReleasePlan,
) -> Result<()> {
    let planned_versions = plan_versions(plan)?;
    let planned_release_units = planned_versions
        .into_iter()
        .filter(|(id, _)| !id.starts_with("workspace/"))
        .collect::<BTreeMap<_, _>>();
    if &planned_release_units != versions {
        return Err(Error::Validation(format!(
            "applied release does not match recovered release plan: expected {planned_release_units:?}, found {versions:?}"
        )));
    }
    Ok(())
}

fn verify_materialized_release(root: &Path, config: &Config, plan: &ReleasePlan) -> Result<()> {
    for planned in &plan.release_units {
        let release_unit = &config.release_units[&planned.id];
        let changelog_path = root.join(&release_unit.path).join("CHANGELOG.md");
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
        for projection in &release_unit.projections {
            if projection.mode == ProjectionMode::None || projection.adapter == Adapter::Go {
                continue;
            }
            let relative = release_unit.path.join(&projection.file);
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
    let mut boundaries = release_tag_targets(root, config, &repository)?;
    let mut release = repository
        .head_commit()
        .map_err(|error| Error::Git(format!("failed to resolve HEAD commit: {error}")))?;
    boundaries.remove(&release.id);
    let deleted = loop {
        if boundaries.contains(&release.id) {
            return Err(Error::Validation(
                "no commit since the latest release tags deletes Intentional intents; supply the sealed release plan"
                    .to_owned(),
            ));
        }
        let Some(parent_id) = release.parent_ids().next() else {
            return Err(Error::Validation(
                "no first-parent commit deletes Intentional intents; supply the sealed release plan"
                    .to_owned(),
            ));
        };
        let parent = parent_id
            .object()
            .map_err(|error| Error::Git(format!("failed to read release parent: {error}")))?
            .try_into_commit()
            .map_err(|error| Error::Git(format!("release parent is not a commit: {error}")))?;
        let deleted = deleted_intents_between(&parent, &release)?;
        if !deleted.is_empty() {
            break deleted;
        }
        release = parent;
    };
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

fn release_tag_targets(
    root: &Path,
    config: &Config,
    repository: &gix::Repository,
) -> Result<BTreeSet<gix::ObjectId>> {
    let versions = VersionRepository::discover(root)?;
    let mut tags = Vec::new();
    for id in config.release_units.keys() {
        let (_, primary) = config.primary_tag(id)?;
        if versions.has_matching_tag(id, &primary.template)? {
            let version = versions.current_version(id, &primary.template)?;
            tags.push(render_tag(&primary.template, id, &version.to_string()));
        }
    }
    for (id, tag) in &config.workspace_tags {
        if versions.has_matching_tag(id, &tag.template)? {
            let version = versions.current_version(id, &tag.template)?;
            tags.push(render_tag(&tag.template, id, &version.to_string()));
        }
    }
    let mut targets = BTreeSet::new();
    for name in tags {
        let Some(mut reference) = repository
            .try_find_reference(format!("refs/tags/{name}").as_str())
            .map_err(|error| {
                Error::Git(format!("failed to inspect release tag {name}: {error}"))
            })?
        else {
            continue;
        };
        targets.insert(
            reference
                .peel_to_id()
                .map_err(|error| Error::Git(format!("failed to peel release tag {name}: {error}")))?
                .detach(),
        );
    }
    Ok(targets)
}

fn deleted_intents_between(
    parent: &gix::Commit<'_>,
    release: &gix::Commit<'_>,
) -> Result<Vec<(std::path::PathBuf, Vec<u8>)>> {
    let old_tree = parent
        .tree()
        .map_err(|error| Error::Git(format!("failed to read release parent tree: {error}")))?;
    let new_tree = release
        .tree()
        .map_err(|error| Error::Git(format!("failed to read release tree: {error}")))?;
    let mut deleted = Vec::new();
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
    Ok(deleted)
}

/// Validate annotated release-unit tag sets without rewriting recoverable omissions.
pub fn tag_record_issues(root: &Path, config: &Config) -> Result<Vec<String>> {
    let versions = VersionRepository::discover(root)?;
    let repository = gix::discover(root)
        .map_err(|error| Error::Git(format!("failed to discover repository: {error}")))?;
    let mut issues = Vec::new();
    for (release_unit_id, release_unit) in &config.release_units {
        let (primary_id, primary) = config.primary_tag(release_unit_id)?;
        if !versions.has_matching_tag(release_unit_id, &primary.template)? {
            continue;
        }
        let version = versions.current_version(release_unit_id, &primary.template)?;
        let primary_name = render_tag(&primary.template, release_unit_id, &version.to_string());
        let Some(primary_record) = read_tag_record(&repository, &primary_name)? else {
            // Lightweight history predating contract-aware records remains readable.
            continue;
        };
        for (field, expected) in [
            (
                "tag-id",
                Config::release_unit_tag_id(release_unit_id, primary_id),
            ),
            ("version", version.to_string()),
            ("contract", config.contract.clone()),
        ] {
            if primary_record.fields.get(field) != Some(&expected) {
                issues.push(format!(
                    "release unit {release_unit_id} primary tag {primary_name} has unexpected {field}"
                ));
            }
        }
        for (tag_id, tag) in &release_unit.tags {
            let name = render_tag(&tag.template, release_unit_id, &version.to_string());
            let Some(record) = read_tag_record(&repository, &name)? else {
                issues.push(format!(
                    "release unit {release_unit_id} is missing annotated projection tag {name} for {version}"
                ));
                continue;
            };
            let expected_tag_id = Config::release_unit_tag_id(release_unit_id, tag_id);
            if record.fields.get("tag-id") != Some(&expected_tag_id) {
                issues.push(format!(
                    "release unit {release_unit_id} tag {name} has unexpected tag-id"
                ));
            }
            for field in ["contract", "generator", "plan-digest", "version"] {
                if record.fields.get(field) != primary_record.fields.get(field) {
                    issues.push(format!(
                        "release unit {release_unit_id} tag {name} disagrees with primary tag {primary_name} on {field}"
                    ));
                }
            }
            if record.target != primary_record.target {
                issues.push(format!(
                    "release unit {release_unit_id} tag {name} targets a different commit than {primary_name}"
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
            "release-unit/sample/primary",
            "1.2.0",
            false,
        );
        assert!(message.contains("contract: contract-1"));
        assert!(message.contains("plan-digest: sha256:abc"));
        assert!(message.contains("version: 1.2.0"));
    }
}
