// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Annotated release records and baseline establishment.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::evidence::assemble::{IntendedDestination, PublisherEvidence};
use crate::evidence::phase::{self, PhaseBindings, PHASE_EVIDENCE_FIELD};
use crate::executor::recipe::{select_publications, SelectedPublication};
use crate::intent::Intent;
use crate::model::{Adapter, ProjectionMode, TagPhase};
use crate::plan::{canonical_json, Generator, ReleasePlan};
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
    /// Recover an applied release and plan its unphased annotated tags.
    pub fn build(root: &Path, channel: Option<&str>, phase: Option<TagPhase>) -> Result<Self> {
        Self::build_with_plan(root, channel, phase, None, None)
    }

    /// Verify a supplied sealed plan or recover one locally, then plan annotated tags.
    ///
    /// `evidence_input` is the directory the publication workflow staged for the
    /// requested phase: built subjects before publication, completed publisher
    /// fragments after it. A phase tag without that directory could only claim
    /// what its own invocation asserted, so the two are required together.
    pub fn build_with_plan(
        root: &Path,
        channel: Option<&str>,
        phase: Option<TagPhase>,
        plan_path: Option<&Path>,
        evidence_input: Option<&Path>,
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
        Self::from_versions(
            root,
            &config,
            &versions,
            phase,
            false,
            Some(&plan.digest),
            evidence_input,
        )
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
        Self::from_versions(
            root,
            &config,
            &versions,
            None,
            true,
            digest.as_deref(),
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn from_versions(
        root: &Path,
        config: &Config,
        versions: &BTreeMap<String, String>,
        phase: Option<TagPhase>,
        baseline: bool,
        release_digest: Option<&str>,
        evidence_input: Option<&Path>,
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
        let evidence = match (phase, evidence_input) {
            (Some(phase), input) => {
                sealed_phase_evidence(root, config, &repository, &candidates, phase, digest, input)?
            }
            (None, Some(input)) => {
                return Err(Error::Validation(format!(
                    "phase evidence directory {} applies only to a --phase invocation",
                    input.display()
                )))
            }
            (None, None) => None,
        };
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
                    evidence.as_deref(),
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
                message: tag_message(
                    &config.contract,
                    digest,
                    &id,
                    &candidate.version,
                    baseline,
                    evidence.as_deref(),
                ),
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
        let head_commit = repository
            .find_object(head)
            .map_err(|error| Error::Git(format!("failed to read HEAD commit: {error}")))?
            .try_into_commit()
            .map_err(|error| Error::Git(format!("HEAD is not a commit: {error}")))?;
        let tagger = tagger_signature(&head_commit)?;
        for tag in &self.tags {
            repository
                .tag(
                    &tag.name,
                    head,
                    gix::object::Kind::Commit,
                    Some(tagger),
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
    evidence: Option<&str>,
) -> Result<()> {
    if record.target != head {
        return Err(Error::Validation(format!(
            "existing tag {name} targets a different commit"
        )));
    }
    // A phase tag seals a historical observation, so a retry that would seal a
    // different one is a conflict rather than a repeat of completed work.
    if record.fields.get(PHASE_EVIDENCE_FIELD).map(String::as_str) != evidence {
        return Err(Error::Validation(format!(
            "existing tag {name} has unexpected {PHASE_EVIDENCE_FIELD}"
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

fn verify_plan_generator(generator: &Generator) -> Result<()> {
    if generator.tool != "intentional" {
        return Err(Error::Validation(format!(
            "release plan generator tool {} is not intentional",
            generator.tool
        )));
    }
    let version = Version::parse(&generator.version).map_err(|error| {
        Error::Validation(format!(
            "release plan generator version {} is not valid SemVer: {error}",
            generator.version
        ))
    })?;
    let current = Version::parse(crate::VERSION)?;
    if version > current {
        return Err(Error::Validation(format!(
            "release plan generator {} is newer than intentional {}",
            generator.version,
            crate::VERSION
        )));
    }
    Ok(())
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
    verify_plan_generator(&plan.generator)?;
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
            let generator_version = Some(plan.generator.version.as_str());
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
                    generator_version,
                )?,
                None => ReleasePlan::from_inputs_before(
                    root,
                    config,
                    &intents,
                    channel,
                    None,
                    generator_version,
                )?,
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
            None,
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

fn tagger_signature<'a>(commit: &'a gix::Commit<'_>) -> Result<gix::actor::SignatureRef<'a>> {
    let committer = commit
        .committer()
        .map_err(|error| Error::Git(format!("failed to read committer signature: {error}")))?;
    Ok(gix::actor::SignatureRef {
        name: b"Intentional".into(),
        email: b"intentional@wyrd.company".into(),
        time: committer.time,
    })
}

/// Canonical annotated-tag record message for one release tag.
pub fn release_tag_message(contract: &str, digest: &str, id: &str, version: &str) -> String {
    tag_message(contract, digest, id, version, false, None)
}

fn tag_message(
    contract: &str,
    digest: &str,
    id: &str,
    version: &str,
    baseline: bool,
    evidence: Option<&str>,
) -> String {
    let phase_evidence = match evidence {
        Some(evidence) => format!("{PHASE_EVIDENCE_FIELD}: {evidence}\n"),
        None => String::new(),
    };
    format!(
        "intentional release record\n\ncontract: {contract}\ngenerator: intentional {}\nplan-digest: {digest}\ntag-id: {id}\nversion: {version}\nbaseline: {baseline}\n{phase_evidence}",
        crate::VERSION
    )
}

/// Seal the evidence every phase tag of one invocation carries.
///
/// The bindings are read from the repository rather than from the caller: R is
/// the commit being tagged, S is its sole parent, and the global tag is the one
/// configured tag that declares no phase. A phase tag that trusted supplied
/// identities could bind a release to a history it never had.
fn sealed_phase_evidence(
    root: &Path,
    config: &Config,
    repository: &gix::Repository,
    candidates: &BTreeMap<String, TagCandidate>,
    phase: TagPhase,
    digest: &str,
    input: Option<&Path>,
) -> Result<Option<String>> {
    let release_commit = repository
        .head_id()
        .map_err(|error| Error::Git(format!("failed to resolve HEAD: {error}")))?
        .detach();
    let commit = repository
        .find_object(release_commit)
        .map_err(|error| Error::Git(format!("failed to read HEAD commit: {error}")))?
        .try_into_commit()
        .map_err(|error| Error::Git(format!("HEAD is not a commit: {error}")))?;
    let parents = commit.parent_ids().collect::<Vec<_>>();
    let [source_commit] = parents.as_slice() else {
        return Err(Error::Validation(format!(
            "the release commit has {} parents; a phase tag requires the single source commit it \
             was derived from",
            parents.len()
        )));
    };
    // Phase evidence binds the global release tag, which only an executor
    // configuration declares. Executor conformance reports a configuration
    // without exactly one, so a workspace that phases its tags without the
    // executor still records plain phased tags rather than failing here. Staged
    // evidence is the one thing that cannot be reconciled with that: accepting
    // a directory and then sealing nothing from it would discard the claim the
    // caller asked to record.
    let unphased = config.unphased_tags();
    let [global] = unphased.as_slice() else {
        if input.is_some() {
            return Err(Error::Validation(format!(
                "sealing staged phase evidence requires exactly one configured tag without require-phase to bind it to; configuration declares {}",
                unphased.len()
            )));
        }
        return Ok(None);
    };
    // Past this point the tag will seal evidence, and every claim it seals is
    // staged rather than derivable. A phase that reaches here without its
    // directory could only record what its own invocation asserted.
    let Some(input) = input else {
        return Err(Error::Validation(format!(
            "--phase {phase} seals evidence against global release tag {} and requires the staged evidence directory",
            global.id
        )));
    };
    let global_tag = candidates.get(&global.id).ok_or_else(|| {
        Error::Validation(format!(
            "global release tag {} has no version in this release",
            global.id
        ))
    })?;
    let source = source_commit.detach().to_string();
    let release = release_commit.to_string();
    let bindings = PhaseBindings {
        source_commit: &source,
        release_commit: &release,
        global_tag: &global_tag.name,
        plan_digest: digest,
    };
    let evidence = match phase {
        TagPhase::BeforePublication => {
            let destinations = select_publications(root, config)?
                .into_iter()
                .map(|publication| IntendedDestination {
                    release_unit: publication.release_unit,
                    publisher: publication.publisher,
                    target: publication.target,
                })
                .collect::<Vec<_>>();
            let subjects = phase::load_built_subjects(config, input)?;
            // A before-publication tag records the subjects this release built,
            // and that sealed set is what later binds a publisher fragment to
            // this release rather than to some other one. Sealing none would
            // record the tag while silently disabling the check it exists for.
            if subjects.is_empty() && !destinations.is_empty() {
                return Err(Error::Validation(
                    "a before-publication tag records the subjects the release built, but no built-subject document was staged".to_owned(),
                ));
            }
            phase::build_before_publication(bindings, &subjects, &destinations)?
        }
        TagPhase::AfterPublication => {
            let fragments = phase::load_publisher_evidence(input)?;
            // An after-publication tag claims that every configured publication
            // completed, so sealing fewer fragments than the configuration
            // selects would record a claim the release cannot support.
            let sealed = fragments
                .iter()
                .map(PublisherEvidence::identity)
                .collect::<BTreeSet<_>>();
            let expected = select_publications(root, config)?
                .iter()
                .map(SelectedPublication::identity)
                .collect::<BTreeSet<_>>();
            let missing = expected.difference(&sealed).cloned().collect::<Vec<_>>();
            if !missing.is_empty() {
                return Err(Error::Validation(format!(
                    "an after-publication tag seals every configured publication, but no evidence was staged for {}",
                    missing.join(", ")
                )));
            }
            phase::build_after_publication(bindings, &fragments)?
        }
    };
    phase::encode(&evidence).map(Some)
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
    use crate::evidence::assemble::PHASE_TAG_EVIDENCE_SCHEMA;
    use crate::evidence::assemble::{PUBLISHER_EVIDENCE_CONTRACT, PUBLISHER_EVIDENCE_SCHEMA};
    use crate::evidence::phase::{BUILT_SUBJECT_CONTRACT, BUILT_SUBJECT_SCHEMA};
    use crate::executor::fixture::Workspace;
    use std::path::PathBuf;

    const PLAN_DIGEST: &str =
        "sha256:4444444444444444444444444444444444444444444444444444444444444444";
    const SUBJECT_DIGEST: &str =
        "sha256:5555555555555555555555555555555555555555555555555555555555555555";

    const PHASE_CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-1
github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml }
release-units:
  component:
    path: component
    npm: {}
    tags:
      primary: { role: primary, template: 'release/{version}' }
      staged:
        role: projection
        template: '{id}/staged@{version}'
        require-phase: before-publication
      published:
        role: projection
        template: '{id}/published@{version}'
        require-phase: after-publication
"#;

    fn git(root: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A workspace whose HEAD is a release commit with exactly one parent.
    fn phase_workspace(label: &str) -> Workspace {
        let workspace = Workspace::new(label);
        workspace
            .write(".intentional/config.yml", PHASE_CONFIG)
            .write(
                "component/package.json",
                r#"{"name":"sample-library","version":"1.0.0"}"#,
            );
        let root = workspace.root();
        git(root, &["init", "-q"]);
        git(root, &["config", "user.name", "Fixture Author"]);
        git(root, &["config", "user.email", "fixture@example.invalid"]);
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "source"]);
        workspace.write("component/CHANGELOG.md", "# Changelog\n\n## 1.0.0\n");
        git(root, &["add", "-A"]);
        git(root, &["commit", "-q", "-m", "release"]);
        workspace
    }

    /// Stage one built-subject document exactly as the build job writes it.
    fn stage_built_subject(workspace: &Workspace, identity: &str) -> PathBuf {
        workspace.write(
            "evidence/built-subject.yml",
            &format!(
                "$schema: {BUILT_SUBJECT_SCHEMA}\ncontract: {BUILT_SUBJECT_CONTRACT}\nrelease-unit: component\nidentity: {identity:?}\nversion: 1.0.0\ndigest: {SUBJECT_DIGEST}\n"
            ),
        );
        workspace.root().join("evidence")
    }

    /// Stage one completed publisher fragment exactly as verification writes it.
    fn stage_publisher_evidence(workspace: &Workspace, tag_object: &str) -> PathBuf {
        workspace.write(
            "evidence/publisher-evidence.yml",
            &format!(
                r#"$schema: {PUBLISHER_EVIDENCE_SCHEMA}
contract: {PUBLISHER_EVIDENCE_CONTRACT}
release-unit: component
publisher: npm
target: primary
source-commit: "{tag_object}"
release-commit: "{tag_object}"
global-tag:
  name: release/1.0.0
  object: "{tag_object}"
  target: "{tag_object}"
plan-digest: {PLAN_DIGEST}
subject:
  kind: npm-package
  identity: sample-library
  version: 1.0.0
  digest: {SUBJECT_DIGEST}
packager:
  id: npm
  version: 10.8.2
build-provenance: []
attached-metadata: []
destination:
  identity: registry.example.test/sample-library
  version: 1.0.0
  digest: sha512-example
clean-client:
  mode: public
  client: npm
  version: 10.8.2
  digest: sha512-example
destination-aliases: []
phase-tags: []
"#
            ),
        );
        workspace.root().join("evidence")
    }

    fn plan_phase_tags(root: &Path, phase: TagPhase, input: &Path) -> Result<TagResult> {
        let config = Config::load(root)?;
        let versions = BTreeMap::from([("component".to_owned(), "1.0.0".to_owned())]);
        TagResult::from_versions(
            root,
            &config,
            &versions,
            Some(phase),
            false,
            Some(PLAN_DIGEST),
            Some(input),
        )
    }

    /// The phase evidence one planned tag message carries.
    fn sealed(message: &str) -> crate::evidence::assemble::PhaseTagEvidence {
        let line = message
            .lines()
            .find_map(|line| line.strip_prefix(&format!("{PHASE_EVIDENCE_FIELD}: ")))
            .expect("a phase evidence record line");
        phase::decode(line).expect("decodable phase evidence")
    }

    #[test]
    fn a_before_publication_tag_names_every_configured_destination() {
        let workspace = phase_workspace("tag-before-publication");
        let input = stage_built_subject(&workspace, "sample-library");
        let result = plan_phase_tags(workspace.root(), TagPhase::BeforePublication, &input)
            .expect("before-publication tags");
        assert_eq!(
            result
                .tags
                .iter()
                .map(|tag| tag.name.clone())
                .collect::<Vec<_>>(),
            vec!["component/staged@1.0.0"]
        );
        let evidence = sealed(&result.tags[0].message);
        assert_eq!(evidence.schema, PHASE_TAG_EVIDENCE_SCHEMA);
        assert_eq!(evidence.global_tag, "release/1.0.0");
        assert_eq!(evidence.plan_digest, PLAN_DIGEST);
        assert_eq!(
            evidence
                .intended_destinations
                .expect("intended destinations")
                .iter()
                .map(IntendedDestination::identity)
                .collect::<Vec<_>>(),
            vec!["component/npm/primary"]
        );
        assert_eq!(evidence.subjects[0].digest, SUBJECT_DIGEST);
    }

    #[test]
    fn an_after_publication_tag_seals_the_accepted_fragments() {
        let workspace = phase_workspace("tag-after-publication");
        let object = "6666666666666666666666666666666666666666";
        let input = stage_publisher_evidence(&workspace, object);
        let result = plan_phase_tags(workspace.root(), TagPhase::AfterPublication, &input)
            .expect("after-publication tags");
        let evidence = sealed(&result.tags[0].message);
        let fragments = evidence.publisher_evidence.expect("sealed fragments");
        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].identity(), "component/npm/primary");
        assert_eq!(fragments[0].source_commit, object);
        assert!(
            evidence.intended_destinations.is_none(),
            "an after-publication tag intends nothing"
        );
        assert_eq!(evidence.subjects.len(), 1);
    }

    #[test]
    fn an_identical_phase_tag_is_accepted_and_a_conflicting_one_is_rejected() {
        let workspace = phase_workspace("tag-phase-retry");
        let input = stage_built_subject(&workspace, "sample-library");
        let planned = plan_phase_tags(workspace.root(), TagPhase::BeforePublication, &input)
            .expect("before-publication tags");
        planned
            .apply(workspace.root(), false)
            .expect("tags created");

        let repeat = plan_phase_tags(workspace.root(), TagPhase::BeforePublication, &input)
            .expect("an identical phase tag is completed work");
        assert!(repeat.tags.is_empty(), "a completed tag is not recreated");

        stage_built_subject(&workspace, "sample-tool");
        let error = plan_phase_tags(workspace.root(), TagPhase::BeforePublication, &input)
            .expect_err("a conflicting phase tag is rejected");
        assert!(
            error
                .to_string()
                .contains(&format!("unexpected {PHASE_EVIDENCE_FIELD}")),
            "{error}"
        );
    }

    #[test]
    fn a_phase_evidence_record_line_survives_the_record_separator_in_its_values() {
        let workspace = phase_workspace("tag-phase-separator");
        let input = stage_built_subject(&workspace, "sample-library: staged");
        let planned = plan_phase_tags(workspace.root(), TagPhase::BeforePublication, &input)
            .expect("before-publication tags");
        planned
            .apply(workspace.root(), false)
            .expect("tags created");

        let repository = gix::discover(workspace.root()).expect("repository");
        let record = read_tag_record(&repository, &planned.tags[0].name)
            .expect("a readable record")
            .expect("an annotated record");
        assert_eq!(record.fields["version"], "1.0.0");
        let evidence = phase::decode(&record.fields[PHASE_EVIDENCE_FIELD])
            .expect("phase evidence decodes from the record");
        assert_eq!(evidence.subjects[0].identity, "sample-library: staged");
    }

    #[test]
    fn refuses_to_discard_staged_evidence_a_workspace_cannot_bind() {
        let workspace = phase_workspace("tag-phase-unbindable");
        // Every configured tag declares a phase, so no global release tag
        // exists for the evidence to bind to. Sealing nothing while accepting
        // the directory would discard the claim the caller asked to record.
        workspace.write(
            ".intentional/config.yml",
            &PHASE_CONFIG.replace(
                "      primary: { role: primary, template: 'release/{version}' }\n",
                "      primary: { role: primary, template: 'release/{version}', require-phase: before-publication }\n",
            ),
        );
        let input = stage_built_subject(&workspace, "sample-library");
        let config = Config::load(workspace.root()).expect("configuration");
        let versions = BTreeMap::from([("component".to_owned(), "1.0.0".to_owned())]);
        let error = TagResult::from_versions(
            workspace.root(),
            &config,
            &versions,
            Some(TagPhase::BeforePublication),
            false,
            Some(PLAN_DIGEST),
            Some(&input),
        )
        .expect_err("evidence with nothing to bind it is rejected");
        assert!(
            error
                .to_string()
                .contains("exactly one configured tag without require-phase"),
            "{error}"
        );
    }

    #[test]
    fn a_phase_without_its_staged_evidence_and_an_unphased_run_with_it_are_both_refused() {
        let workspace = phase_workspace("tag-phase-pairing");
        let input = stage_built_subject(&workspace, "sample-library");
        let config = Config::load(workspace.root()).expect("configuration");
        let versions = BTreeMap::from([("component".to_owned(), "1.0.0".to_owned())]);
        // Every claim a phase seals is staged rather than derivable, so a
        // phase that reaches sealing without its directory could only record
        // what its own invocation asserted.
        let error = TagResult::from_versions(
            workspace.root(),
            &config,
            &versions,
            Some(TagPhase::BeforePublication),
            false,
            Some(PLAN_DIGEST),
            None,
        )
        .expect_err("a phase without its staged evidence is rejected");
        assert!(
            error
                .to_string()
                .contains("requires the staged evidence directory"),
            "{error}"
        );
        // The sealed subject set is what later binds a publisher fragment to
        // this release, so a staged directory carrying no subject would record
        // the tag while disabling the check it exists for.
        let empty = workspace.root().join("empty-evidence");
        std::fs::create_dir_all(&empty).expect("an empty evidence directory");
        let error = TagResult::from_versions(
            workspace.root(),
            &config,
            &versions,
            Some(TagPhase::BeforePublication),
            false,
            Some(PLAN_DIGEST),
            Some(&empty),
        )
        .expect_err("a phase staging no subject is rejected");
        assert!(
            error
                .to_string()
                .contains("no built-subject document was staged"),
            "{error}"
        );
        let error = TagResult::from_versions(
            workspace.root(),
            &config,
            &versions,
            None,
            false,
            Some(PLAN_DIGEST),
            Some(&input),
        )
        .expect_err("staged evidence without a phase is rejected");
        assert!(
            error
                .to_string()
                .contains("applies only to a --phase invocation"),
            "{error}"
        );
    }

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
            None,
        );
        assert!(message.contains("contract: contract-1"));
        assert!(message.contains("plan-digest: sha256:abc"));
        assert!(message.contains("version: 1.2.0"));
        assert!(message.contains(&format!("generator: intentional {}", crate::VERSION)));
        assert!(
            !message.contains(PHASE_EVIDENCE_FIELD),
            "a record without phase semantics carries no phase evidence"
        );
    }

    #[test]
    fn a_phase_record_appends_the_evidence_as_its_final_line() {
        let unphased = tag_message(
            "contract-1",
            "sha256:abc",
            "release-unit/sample/staged",
            "1.2.0",
            false,
            None,
        );
        let phased = tag_message(
            "contract-1",
            "sha256:abc",
            "release-unit/sample/staged",
            "1.2.0",
            false,
            Some(r#"{"phase":"before-publication"}"#),
        );
        assert_eq!(
            phased,
            format!("{unphased}{PHASE_EVIDENCE_FIELD}: {{\"phase\":\"before-publication\"}}\n")
        );
    }

    #[test]
    fn accepts_compatible_prior_plan_generator() {
        let generator = Generator {
            tool: "intentional".to_owned(),
            version: "0.1.0".to_owned(),
        };
        verify_plan_generator(&generator).expect("older generator");
    }

    #[test]
    fn accepts_current_plan_generator() {
        let generator = Generator {
            tool: "intentional".to_owned(),
            version: crate::VERSION.to_owned(),
        };
        verify_plan_generator(&generator).expect("current generator");
    }

    #[test]
    fn rejects_non_intentional_plan_generator() {
        let generator = Generator {
            tool: "other-tool".to_owned(),
            version: "1.0.0".to_owned(),
        };
        let error = verify_plan_generator(&generator).expect_err("foreign tool");
        assert!(error.to_string().contains("is not intentional"));
    }

    #[test]
    fn rejects_malformed_plan_generator_version() {
        let generator = Generator {
            tool: "intentional".to_owned(),
            version: "not-semver".to_owned(),
        };
        let error = verify_plan_generator(&generator).expect_err("malformed version");
        assert!(error.to_string().contains("not valid SemVer"));
    }

    #[test]
    fn rejects_future_plan_generator_version() {
        let generator = Generator {
            tool: "intentional".to_owned(),
            version: "99.0.0".to_owned(),
        };
        let error = verify_plan_generator(&generator).expect_err("future version");
        assert!(error.to_string().contains("newer than intentional"));
    }
}
