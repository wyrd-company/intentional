// ---
// relationships:
//   implements: github-release-executor
// ---

//! Executor initialization plan creation, resumption, and application.

use crate::config::{
    Config, GithubConfig, GithubWorkflow, GithubWorkflows, NpmAdditionalTargets, NpmGithubTarget,
    NpmPublisher, OciPublisher, ReleaseUnitConfig, CONFIG_PATH, DEFAULT_PUBLISH_WORKFLOW,
    DEFAULT_RELEASE_WORKFLOW,
};
use crate::error::{Error, Result};
use crate::executor::recipe::{
    capability_set, derive_capabilities, recipes_for, select_publications, Capability,
    CapabilityEvidence, Packager, PRIMARY_TARGET,
};
use crate::init::SourceEvidence;
use crate::model::{PublisherKind, ReleaseUnitDisposition};
use crate::plan::canonical_json;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Durable executor initialization plan location.
pub const EXECUTOR_INIT_PLAN_PATH: &str = ".intentional/executor-init-plan.yml";

/// Published executor initialization-plan schema identifier.
pub const EXECUTOR_INIT_PLAN_SCHEMA: &str =
    "https://intentional.foo/schemas/executor-init-plan.yml";

/// Choice id that establishes the candidate's publication intent or packager baseline.
pub const ACCEPT_CHOICE: &str = "accept";

/// Choice id that declines the candidate.
pub const DECLINE_CHOICE: &str = "decline";

/// Resolution state of an executor initialization plan.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutorInitState {
    /// At least one candidate is unresolved.
    NeedsInput,
    /// Every candidate carries an explicit resolution.
    Ready,
}

impl ExecutorInitState {
    /// Stable name matching the state's serialized spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NeedsInput => "needs-input",
            Self::Ready => "ready",
        }
    }
}

impl std::fmt::Display for ExecutorInitState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// What one candidate decides.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CandidateKind {
    /// Whether a derived capability publishes through one publisher target.
    PublicationIntent,
    /// Whether Intentional creates a native packager baseline.
    Packager,
}

/// One finite choice presented by a candidate.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct Choice {
    /// Stable choice id named by a resolution.
    pub id: String,
    /// Human-readable description of the choice.
    pub label: String,
    /// Publisher this choice configures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher: Option<PublisherKind>,
    /// Publisher target this choice configures.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Native packager this choice establishes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packager: Option<Packager>,
}

/// One evidence-backed executor decision awaiting or carrying a resolution.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExecutorCandidate {
    /// Stable hash of the candidate's decision identity.
    pub id: String,
    /// Decision this candidate carries.
    pub kind: CandidateKind,
    /// Configured release unit the decision applies to.
    pub release_unit: String,
    /// Derived capability supporting the decision.
    pub capability: String,
    /// Native evidence the decision rests on.
    pub evidence: Vec<SourceEvidence>,
    /// Finite choices; exactly one may be named by the resolution.
    pub choices: Vec<Choice>,
    /// Choice Intentional recommends, when native evidence supports one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended: Option<String>,
    /// Editable choice id, or null while unresolved.
    pub resolution: Option<String>,
}

impl ExecutorCandidate {
    /// Derive the stable candidate id from its decision identity.
    pub fn stable_id(
        kind: CandidateKind,
        release_unit: &str,
        capability: &str,
        scope: &str,
    ) -> String {
        let mut identity = Sha256::new();
        identity.update(match kind {
            CandidateKind::PublicationIntent => b"publication-intent".as_slice(),
            CandidateKind::Packager => b"packager".as_slice(),
        });
        for part in [release_unit, capability, scope] {
            identity.update([0]);
            identity.update(part.as_bytes());
        }
        format!("candidate:{:x}", identity.finalize())
    }

    fn selected(&self) -> Option<&Choice> {
        let resolution = self.resolution.as_deref()?;
        self.choices.iter().find(|choice| choice.id == resolution)
    }

    fn validate(&self) -> Result<()> {
        if self.evidence.is_empty() {
            return Err(Error::Validation(format!(
                "executor candidate {} must carry evidence",
                self.id
            )));
        }
        if self.choices.is_empty() {
            return Err(Error::Validation(format!(
                "executor candidate {} must offer at least one choice",
                self.id
            )));
        }
        if let Some(resolution) = &self.resolution {
            if self.selected().is_none() {
                return Err(Error::Validation(format!(
                    "executor candidate {} resolution {resolution} names no declared choice",
                    self.id
                )));
            }
        }
        Ok(())
    }
}

/// Durable, editable executor initialization handoff.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExecutorInitPlan {
    /// Published schema location.
    #[serde(rename = "$schema")]
    pub schema: String,
    /// Computed state. Editing this field never bypasses validation.
    pub state: ExecutorInitState,
    /// Digest binding the plan to the exact evidence it was derived from.
    pub source_fingerprint: String,
    /// GitHub executor configuration the plan establishes.
    pub inferred_github: GithubConfig,
    /// Evidence-backed decisions awaiting or carrying resolutions.
    pub candidates: Vec<ExecutorCandidate>,
    /// Exact operations applying this plan performs, including reported prerequisites.
    pub planned_operations: Vec<String>,
}

impl ExecutorInitPlan {
    /// Validate candidate identity, evidence, and resolution completeness.
    pub fn validate(&self) -> Result<()> {
        if self.schema != EXECUTOR_INIT_PLAN_SCHEMA {
            return Err(Error::Validation(format!(
                "executor initialization plan schema must be {EXECUTOR_INIT_PLAN_SCHEMA}"
            )));
        }
        let mut ids = BTreeSet::new();
        for candidate in &self.candidates {
            candidate.validate()?;
            if !ids.insert(candidate.id.as_str()) {
                return Err(Error::Validation(format!(
                    "duplicate executor candidate {}",
                    candidate.id
                )));
            }
        }
        let unresolved = self
            .candidates
            .iter()
            .any(|candidate| candidate.resolution.is_none());
        if unresolved && self.state == ExecutorInitState::Ready {
            return Err(Error::Validation(
                "a ready executor initialization plan requires every candidate to be resolved"
                    .to_owned(),
            ));
        }
        Ok(())
    }

    /// Serialize deterministically.
    pub fn to_yaml(&self) -> Result<String> {
        self.validate()?;
        Ok(serde_yaml::to_string(self)?)
    }

    /// Equivalent structured JSON for agent consumers.
    pub fn to_json(&self) -> Result<String> {
        self.validate()?;
        canonical_json(self)
    }
}

/// Planned executor initialization output.
#[derive(Debug, Clone)]
pub struct ExecutorInitResult {
    /// Computed resolution state.
    pub state: ExecutorInitState,
    /// Plan location for an agent or human to edit.
    pub path: PathBuf,
    /// Exact operations, including reported repository prerequisites.
    pub operations: Vec<String>,
    /// Derived plan.
    pub plan: ExecutorInitPlan,
    writes: Vec<(PathBuf, String)>,
}

impl ExecutorInitResult {
    /// Files this result writes, as workspace-relative paths.
    pub fn planned_writes(&self) -> Vec<&Path> {
        self.writes
            .iter()
            .map(|(relative, _)| relative.as_path())
            .collect()
    }
}

impl ExecutorInitResult {
    /// Materialize the plan or its authorized configuration changes.
    pub fn apply(&self, root: &Path, dry_run: bool) -> Result<()> {
        if dry_run {
            return Ok(());
        }
        for (relative, contents) in &self.writes {
            let path = root.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
            }
            std::fs::write(&path, contents).map_err(|error| Error::io(&path, error))?;
        }
        Ok(())
    }
}

/// Create or resume the executor initialization plan and apply it when it is ready.
pub fn initialize_executor(root: &Path) -> Result<ExecutorInitResult> {
    let mut config = Config::load(root)?;
    let recorded = read_plan(root)?;
    let existing = recorded.as_ref().map(|text| parse_plan(text)).transpose()?;
    let inferred_github = match (&config.github, existing.as_ref()) {
        (Some(github), _) => github.clone(),
        (None, Some(plan)) => plan.inferred_github.clone(),
        (None, None) => default_github(),
    };
    let resolutions = existing
        .as_ref()
        .map(|plan| {
            plan.candidates
                .iter()
                .filter_map(|candidate| {
                    candidate
                        .resolution
                        .clone()
                        .map(|resolution| (candidate.id.clone(), resolution))
                })
                .collect::<BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    let candidates = derive_candidates(root, &config, &resolutions)?;
    let state = if candidates
        .iter()
        .any(|candidate| candidate.resolution.is_none())
    {
        ExecutorInitState::NeedsInput
    } else {
        ExecutorInitState::Ready
    };

    let original = config.clone();
    let mut writes = Vec::new();
    // `operations` reports what this run does; `outstanding` is persisted into
    // the plan and describes what applying that plan still performs.
    let mut operations = Vec::new();
    let mut outstanding = Vec::new();
    if state == ExecutorInitState::Ready {
        let github_added = config.github.is_none();
        config.github = Some(inferred_github.clone());
        if github_added {
            operations.push(format!(
                "configure the GitHub executor in {CONFIG_PATH} with release workflow {} and publish workflow {}",
                inferred_github.workflows.release.path.display(),
                inferred_github.workflows.publish.path.display()
            ));
        }
        for candidate in &candidates {
            apply_candidate(root, &mut config, candidate, &mut writes, &mut operations)?;
        }
        config.validate()?;
        select_publications(root, &config)?;
        if config != original {
            // Configuration is re-serialized from the parsed model, so the
            // rewrite normalizes the whole document. Say so before applying it
            // rather than letting the user discover it in the diff.
            operations.push(format!(
                "rewrite {CONFIG_PATH} in canonical form; comments, key order, and formatting in that file are not preserved"
            ));
            writes.push((PathBuf::from(CONFIG_PATH), config.to_yaml()?));
        }
    } else {
        let unresolved = format!(
            "resolve {} executor candidate(s) in {EXECUTOR_INIT_PLAN_PATH} and rerun intentional executor init",
            candidates
                .iter()
                .filter(|candidate| candidate.resolution.is_none())
                .count()
        );
        operations.push(unresolved.clone());
        outstanding.push(unresolved);
    }

    let source_fingerprint = fingerprint(&candidates)?;
    if let Some(previous) = &existing {
        for report in drift_reports(previous, &candidates, &source_fingerprint) {
            operations.push(report.clone());
            outstanding.push(report);
        }
    }
    for prerequisite in prerequisites(&inferred_github)? {
        operations.push(prerequisite.clone());
        outstanding.push(prerequisite);
    }

    let plan = ExecutorInitPlan {
        schema: EXECUTOR_INIT_PLAN_SCHEMA.to_owned(),
        state,
        source_fingerprint,
        inferred_github,
        candidates,
        planned_operations: outstanding,
    };
    plan.validate()?;
    let rendered = plan.to_yaml()?;
    if recorded.as_deref() != Some(rendered.as_str()) {
        writes.push((PathBuf::from(EXECUTOR_INIT_PLAN_PATH), rendered));
    }
    Ok(ExecutorInitResult {
        state,
        path: PathBuf::from(EXECUTOR_INIT_PLAN_PATH),
        operations,
        plan,
        writes,
    })
}

/// Repository settings Intentional reports without mutating.
fn prerequisites(github: &GithubConfig) -> Result<Vec<String>> {
    let namespaces = github.namespaces()?;
    Ok(vec![
        "report: the repository GitHub App must be a ruleset bypass actor for the default branch and every Intentional-managed release tag namespace; Intentional does not mutate repository settings".to_owned(),
        format!(
            "report: the repository must define GitHub App credentials {}GITHUB_APP_ID and {}GITHUB_APP_PRIVATE_KEY",
            namespaces.envvar, namespaces.envvar
        ),
        format!(
            "report: the protected environment {} must guard the release authority transition",
            namespaces.environment
        ),
    ])
}

fn default_github() -> GithubConfig {
    GithubConfig {
        workflows: GithubWorkflows {
            release: GithubWorkflow {
                path: PathBuf::from(DEFAULT_RELEASE_WORKFLOW),
                gates: Vec::new(),
            },
            publish: GithubWorkflow {
                path: PathBuf::from(DEFAULT_PUBLISH_WORKFLOW),
                gates: Vec::new(),
            },
        },
        prefix: None,
    }
}

type Resolutions = BTreeMap<String, String>;

fn derive_candidates(
    root: &Path,
    config: &Config,
    resolutions: &Resolutions,
) -> Result<Vec<ExecutorCandidate>> {
    let mut candidates = Vec::new();
    for (id, release_unit) in &config.release_units {
        // A suspended release unit does not release, so it is never offered
        // publication intent.
        if release_unit.disposition != ReleaseUnitDisposition::Managed {
            continue;
        }
        let derived = derive_capabilities(root, release_unit)?;
        let capabilities = capability_set(&derived);
        for evidence in &derived {
            for (publisher, target) in offered_targets(evidence.capability, &capabilities) {
                if configured(release_unit, publisher, &target)
                    || !prerequisite_met(release_unit, &candidates, publisher, &target, id)
                {
                    continue;
                }
                candidates.push(publication_candidate(
                    id,
                    evidence,
                    publisher,
                    &target,
                    resolutions,
                ));
            }
        }
    }
    candidates.extend(packager_candidates(root, config, &candidates, resolutions)?);
    Ok(candidates)
}

/// Publisher targets a capability can offer, ordered by publisher then target.
///
/// Initialization offers only decisions it can apply on its own. A target that
/// needs repository data no evidence supplies, or that more than one derived
/// capability could publish, is configured directly instead of guessed here.
fn offered_targets(
    capability: Capability,
    capabilities: &BTreeSet<Capability>,
) -> Vec<(PublisherKind, String)> {
    let mut offered = Vec::new();
    for publisher in [
        PublisherKind::Npm,
        PublisherKind::Cargo,
        PublisherKind::Homebrew,
        PublisherKind::Rpm,
        PublisherKind::Apt,
        PublisherKind::Aur,
        PublisherKind::Oci,
    ] {
        let matching = recipes_for(capabilities, publisher);
        for recipe in matching
            .iter()
            .filter(|recipe| recipe.capability == capability)
        {
            if required_configuration(publisher, recipe.target).is_some() {
                continue;
            }
            let unambiguous = matching
                .iter()
                .filter(|peer| peer.target == recipe.target)
                .count()
                == 1;
            if unambiguous {
                offered.push((publisher, recipe.target.to_owned()));
            }
        }
    }
    offered
}

/// Configuration a publisher target requires that no native evidence supplies.
fn required_configuration(publisher: PublisherKind, target: &str) -> Option<&'static str> {
    match (publisher, target) {
        (PublisherKind::Homebrew, _) => Some("a tap repository"),
        (PublisherKind::Oci, "dockerhub") => Some("a Docker Hub repository"),
        _ => None,
    }
}

/// npm's additional GitHub target is offered only after its primary is accepted.
fn prerequisite_met(
    release_unit_config: &ReleaseUnitConfig,
    candidates: &[ExecutorCandidate],
    publisher: PublisherKind,
    target: &str,
    release_unit: &str,
) -> bool {
    if publisher != PublisherKind::Npm || target != "github" {
        return true;
    }
    if configured(release_unit_config, PublisherKind::Npm, PRIMARY_TARGET) {
        return true;
    }
    candidates.iter().any(|candidate| {
        candidate.release_unit == release_unit
            && candidate.resolution.as_deref() == Some(ACCEPT_CHOICE)
            && candidate
                .selected()
                .is_some_and(|choice| choice.target.as_deref() == Some(PRIMARY_TARGET))
    })
}

fn configured(release_unit: &ReleaseUnitConfig, publisher: PublisherKind, target: &str) -> bool {
    match (publisher, target) {
        (PublisherKind::Npm, "github") => release_unit
            .npm
            .as_ref()
            .and_then(|npm| npm.additional_targets.as_ref())
            .is_some_and(|targets| targets.github.is_some()),
        (PublisherKind::Npm, _) => release_unit.npm.is_some(),
        (PublisherKind::Cargo, _) => release_unit.cargo.is_some(),
        (PublisherKind::Homebrew, _) => release_unit.homebrew.is_some(),
        (PublisherKind::Rpm, _) => release_unit.rpm.is_some(),
        (PublisherKind::Apt, _) => release_unit.apt.is_some(),
        (PublisherKind::Aur, _) => release_unit.aur.is_some(),
        (PublisherKind::Oci, "dockerhub") => release_unit
            .oci
            .as_ref()
            .is_some_and(|oci| oci.dockerhub.is_some()),
        (PublisherKind::Oci, _) => release_unit
            .oci
            .as_ref()
            .is_some_and(|oci| oci.ghcr.is_some()),
    }
}

fn publication_candidate(
    release_unit: &str,
    evidence: &CapabilityEvidence,
    publisher: PublisherKind,
    target: &str,
    resolutions: &Resolutions,
) -> ExecutorCandidate {
    let capability = evidence.capability.as_str();
    let scope = format!("{publisher}/{target}");
    let id = ExecutorCandidate::stable_id(
        CandidateKind::PublicationIntent,
        release_unit,
        capability,
        &scope,
    );
    let evidence = vec![evidence.evidence.clone()];
    ExecutorCandidate {
        resolution: carried_resolution(&id, resolutions),
        id,
        kind: CandidateKind::PublicationIntent,
        release_unit: release_unit.to_owned(),
        capability: capability.to_owned(),
        evidence,
        choices: vec![
            Choice {
                id: ACCEPT_CHOICE.to_owned(),
                label: format!(
                    "Publish {release_unit} to {publisher} {target} using its maintained recipe"
                ),
                publisher: Some(publisher),
                target: Some(target.to_owned()),
                packager: None,
            },
            Choice {
                id: DECLINE_CHOICE.to_owned(),
                label: format!("Do not publish {release_unit} to {publisher} {target}"),
                publisher: None,
                target: None,
                packager: None,
            },
        ],
        recommended: None,
    }
}

fn packager_candidates(
    root: &Path,
    config: &Config,
    candidates: &[ExecutorCandidate],
    resolutions: &Resolutions,
) -> Result<Vec<ExecutorCandidate>> {
    let mut required = BTreeMap::new();
    for publication in select_publications(root, config)? {
        required.insert(
            (publication.release_unit.clone(), publication.packager),
            publication.capability,
        );
    }
    for candidate in candidates {
        let Some(choice) = candidate.selected() else {
            continue;
        };
        let (Some(publisher), Some(target)) = (choice.publisher, choice.target.as_deref()) else {
            continue;
        };
        let capabilities = BTreeSet::from([parse_capability(&candidate.capability)?]);
        for recipe in recipes_for(&capabilities, publisher) {
            if recipe.target == target {
                required.insert(
                    (candidate.release_unit.clone(), recipe.packager),
                    recipe.capability,
                );
            }
        }
    }

    let mut derived = Vec::new();
    for ((release_unit, packager), capability) in required {
        if !packager.baseline_is_authorable()
            || packager_configured(root, &config.release_units[&release_unit], packager)
        {
            continue;
        }
        let evidence = derive_capabilities(root, &config.release_units[&release_unit])?
            .into_iter()
            .filter(|item| item.capability == capability)
            .map(|item| item.evidence)
            .collect::<Vec<_>>();
        if evidence.is_empty() {
            continue;
        }
        let id = ExecutorCandidate::stable_id(
            CandidateKind::Packager,
            &release_unit,
            capability.as_str(),
            packager.as_str(),
        );
        derived.push(ExecutorCandidate {
            resolution: carried_resolution(&id, resolutions),
            id,
            kind: CandidateKind::Packager,
            release_unit: release_unit.clone(),
            capability: capability.as_str().to_owned(),
            evidence,
            choices: vec![
                Choice {
                    id: ACCEPT_CHOICE.to_owned(),
                    label: format!(
                        "Create the baseline {packager} configuration for {release_unit}"
                    ),
                    publisher: None,
                    target: None,
                    packager: Some(packager),
                },
                Choice {
                    id: DECLINE_CHOICE.to_owned(),
                    label: format!(
                        "Author the {packager} configuration for {release_unit} outside Intentional"
                    ),
                    publisher: None,
                    target: None,
                    packager: None,
                },
            ],
            recommended: Some(ACCEPT_CHOICE.to_owned()),
        });
    }
    Ok(derived)
}

fn packager_configured(root: &Path, release_unit: &ReleaseUnitConfig, packager: Packager) -> bool {
    packager
        .configuration_paths()
        .iter()
        .any(|relative| root.join(&release_unit.path).join(relative).is_file())
}

fn parse_capability(value: &str) -> Result<Capability> {
    serde_yaml::from_str(value)
        .map_err(|_| Error::Validation(format!("unknown release-unit capability {value}")))
}

fn carried_resolution(id: &str, resolutions: &Resolutions) -> Option<String> {
    resolutions.get(id).cloned()
}

fn apply_candidate(
    root: &Path,
    config: &mut Config,
    candidate: &ExecutorCandidate,
    writes: &mut Vec<(PathBuf, String)>,
    operations: &mut Vec<String>,
) -> Result<()> {
    let Some(choice) = candidate.selected() else {
        return Ok(());
    };
    if choice.id == DECLINE_CHOICE {
        return Ok(());
    }
    let release_unit = config
        .release_units
        .get_mut(&candidate.release_unit)
        .ok_or_else(|| {
            Error::Validation(format!(
                "executor candidate {} names unknown release unit {}",
                candidate.id, candidate.release_unit
            ))
        })?;
    match candidate.kind {
        CandidateKind::PublicationIntent => {
            let (Some(publisher), Some(target)) = (choice.publisher, choice.target.as_deref())
            else {
                return Err(Error::Validation(format!(
                    "executor candidate {} accepts publication without a publisher target",
                    candidate.id
                )));
            };
            enable_publisher(release_unit, publisher, target)?;
            operations.push(format!(
                "configure the {publisher} {target} publisher for release unit {}",
                candidate.release_unit
            ));
        }
        CandidateKind::Packager => {
            let Some(packager) = choice.packager else {
                return Err(Error::Validation(format!(
                    "executor candidate {} accepts a packager baseline without a packager",
                    candidate.id
                )));
            };
            let relative = release_unit.path.join(
                packager
                    .configuration_paths()
                    .first()
                    .expect("packager declares configuration paths"),
            );
            if root.join(&relative).is_file() {
                return Ok(());
            }
            writes.push((
                relative.clone(),
                baseline(packager, &candidate.release_unit),
            ));
            operations.push(format!(
                "create the baseline {packager} configuration {}",
                relative.display()
            ));
        }
    }
    Ok(())
}

fn enable_publisher(
    release_unit: &mut ReleaseUnitConfig,
    publisher: PublisherKind,
    target: &str,
) -> Result<()> {
    match (publisher, target) {
        (PublisherKind::Npm, "github") => {
            let npm = release_unit.npm.get_or_insert_with(NpmPublisher::default);
            npm.additional_targets
                .get_or_insert_with(NpmAdditionalTargets::default)
                .github
                .get_or_insert_with(NpmGithubTarget::default);
        }
        (PublisherKind::Npm, _) => {
            release_unit.npm.get_or_insert_with(NpmPublisher::default);
        }
        (PublisherKind::Cargo, _) => {
            release_unit.cargo.get_or_insert_with(Default::default);
        }
        (PublisherKind::Rpm, _) => {
            release_unit.rpm.get_or_insert_with(Default::default);
        }
        (PublisherKind::Apt, _) => {
            release_unit.apt.get_or_insert_with(Default::default);
        }
        (PublisherKind::Aur, _) => {
            release_unit.aur.get_or_insert_with(Default::default);
        }
        (PublisherKind::Oci, "dockerhub") => {
            return Err(explicit_configuration_error(publisher, target))
        }
        (PublisherKind::Oci, _) => {
            release_unit
                .oci
                .get_or_insert_with(OciPublisher::default)
                .ghcr
                .get_or_insert_with(Default::default);
        }
        (PublisherKind::Homebrew, _) => {
            return Err(explicit_configuration_error(publisher, target))
        }
    }
    Ok(())
}

fn explicit_configuration_error(publisher: PublisherKind, target: &str) -> Error {
    Error::Validation(format!(
        "the {publisher} {target} publisher requires {}; add it to {CONFIG_PATH} directly",
        required_configuration(publisher, target).unwrap_or("explicit configuration")
    ))
}

fn baseline(packager: Packager, release_unit: &str) -> String {
    match packager {
        Packager::GoReleaser => format!(
            "version: 2\nproject_name: {release_unit}\nbuilds:\n  - main: .\n    binary: {release_unit}\n    env:\n      - CGO_ENABLED=0\n    goos: [ linux, darwin, windows ]\n    goarch: [ amd64, arm64 ]\n"
        ),
        Packager::Npm | Packager::Cargo | Packager::Buildx | Packager::DevContainerCli => {
            String::new()
        }
    }
}

fn fingerprint(candidates: &[ExecutorCandidate]) -> Result<String> {
    let mut source = BTreeMap::new();
    for candidate in candidates {
        for evidence in &candidate.evidence {
            source.insert(
                evidence.path.to_string_lossy().to_string(),
                &evidence.digest,
            );
        }
    }
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(canonical_json(&source)?.as_bytes())
    ))
}

fn read_plan(root: &Path) -> Result<Option<String>> {
    let path = root.join(EXECUTOR_INIT_PLAN_PATH);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::io(path, error)),
    }
}

fn parse_plan(text: &str) -> Result<ExecutorInitPlan> {
    let plan: ExecutorInitPlan = serde_yaml::from_str(text)?;
    plan.validate()?;
    Ok(plan)
}

/// Report evidence that changed since the recorded plan was written.
///
/// Resolutions survive the change because they answer a question about the
/// capability, not about the file's exact bytes. The drift is still reported so
/// a decision taken against different evidence is visible rather than silent.
fn drift_reports(
    previous: &ExecutorInitPlan,
    candidates: &[ExecutorCandidate],
    source_fingerprint: &str,
) -> Vec<String> {
    if previous.source_fingerprint == source_fingerprint {
        return Vec::new();
    }
    let recorded = evidence_digests(&previous.candidates);
    let current = evidence_digests(candidates);
    let mut changed = Vec::new();
    for (path, digest) in &current {
        match recorded.get(path) {
            Some(previous) if previous == digest => {}
            Some(_) => changed.push(format!("{path} changed")),
            None => changed.push(format!("{path} is new evidence")),
        }
    }
    for path in recorded.keys() {
        if !current.contains_key(path) {
            changed.push(format!("{path} is no longer evidence"));
        }
    }
    if changed.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "report: source evidence changed since the recorded plan ({}); recorded resolutions are retained because they decide publication intent, not file contents",
        changed.join(", ")
    )]
}

fn evidence_digests(candidates: &[ExecutorCandidate]) -> BTreeMap<String, String> {
    let mut digests = BTreeMap::new();
    for candidate in candidates {
        for evidence in &candidate.evidence {
            digests.insert(
                evidence.path.to_string_lossy().to_string(),
                evidence.digest.clone(),
            );
        }
    }
    digests
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::fixture::Workspace;

    const CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-1
release-units:
  component:
    path: component
    tags:
      primary: { role: primary, template: '{id}@{version}' }
"#;

    fn resolve(root: &Path, capability: &str, scope: &str, choice: &str) {
        let path = root.join(EXECUTOR_INIT_PLAN_PATH);
        let text = std::fs::read_to_string(&path).expect("plan exists");
        let mut plan: ExecutorInitPlan = serde_yaml::from_str(&text).expect("plan parses");
        let id = ExecutorCandidate::stable_id(
            CandidateKind::PublicationIntent,
            "component",
            capability,
            scope,
        );
        let packager_id =
            ExecutorCandidate::stable_id(CandidateKind::Packager, "component", capability, scope);
        let mut matched = false;
        for candidate in &mut plan.candidates {
            if candidate.id == id || candidate.id == packager_id {
                candidate.resolution = Some(choice.to_owned());
                matched = true;
            }
        }
        assert!(matched, "plan offers a {capability} {scope} candidate");
        std::fs::write(
            &path,
            serde_yaml::to_string(&plan).expect("plan serializes"),
        )
        .expect("write plan");
    }

    fn run(workspace: &Workspace) -> ExecutorInitResult {
        let result = initialize_executor(workspace.root()).expect("executor init runs");
        result.apply(workspace.root(), false).expect("plan applies");
        result
    }

    #[test]
    fn requires_explicit_publication_intent_and_reports_repository_prerequisites() {
        let workspace = Workspace::new("init-intent");
        workspace.write(".intentional/config.yml", CONFIG).write(
            "component/package.json",
            r#"{"name":"example-component","version":"1.0.0"}"#,
        );
        let result = run(&workspace);
        assert_eq!(result.state, ExecutorInitState::NeedsInput);
        assert_eq!(result.plan.candidates.len(), 1);
        assert_eq!(result.plan.candidates[0].resolution, None);
        assert_eq!(result.plan.candidates[0].capability, "node-package");
        assert!(result
            .operations
            .iter()
            .any(|operation| operation.contains("ruleset bypass actor")));
        assert!(result
            .operations
            .iter()
            .any(|operation| operation.contains("INTENTIONAL_GITHUB_APP_ID")));
        assert!(workspace.root().join(EXECUTOR_INIT_PLAN_PATH).is_file());
        assert_eq!(
            Config::load(workspace.root()).expect("config loads").github,
            None,
            "an unresolved plan never configures the executor"
        );
    }

    #[test]
    fn applies_resolved_intent_and_resumes_without_reasking() {
        let workspace = Workspace::new("init-apply");
        workspace.write(".intentional/config.yml", CONFIG).write(
            "component/package.json",
            r#"{"name":"example-component","version":"1.0.0"}"#,
        );
        run(&workspace);
        resolve(
            workspace.root(),
            "node-package",
            "npm/primary",
            ACCEPT_CHOICE,
        );

        let result = run(&workspace);
        assert_eq!(result.state, ExecutorInitState::NeedsInput);
        assert_eq!(
            result.plan.candidates.len(),
            2,
            "the additional npm target is offered once its primary is accepted"
        );
        resolve(
            workspace.root(),
            "node-package",
            "npm/github",
            DECLINE_CHOICE,
        );

        let result = run(&workspace);
        assert_eq!(result.state, ExecutorInitState::Ready);
        let config = Config::load(workspace.root()).expect("config loads");
        let github = config.github.expect("executor configured");
        assert_eq!(
            github.workflows.release.path,
            PathBuf::from(DEFAULT_RELEASE_WORKFLOW)
        );
        let component = &config.release_units["component"];
        assert!(component.npm.is_some());
        assert!(component
            .npm
            .as_ref()
            .expect("npm publisher")
            .additional_targets
            .is_none());

        let repeated = run(&workspace);
        assert_eq!(
            repeated.state,
            ExecutorInitState::Ready,
            "a converged workspace stays ready without new questions"
        );
        assert_eq!(
            repeated
                .plan
                .candidates
                .iter()
                .map(|candidate| candidate.resolution.clone())
                .collect::<Vec<_>>(),
            vec![Some(DECLINE_CHOICE.to_owned())],
            "the durable plan keeps the declined additional target resolved"
        );
    }

    #[test]
    fn proposes_the_native_packager_baseline_a_selected_recipe_requires() {
        let workspace = Workspace::new("init-packager");
        workspace
            .write(".intentional/config.yml", CONFIG)
            .write("component/go.mod", "module example.test/component\n")
            .write("component/main.go", "package main\n\nfunc main() {}\n");
        run(&workspace);
        resolve(
            workspace.root(),
            "go-application",
            "rpm/primary",
            ACCEPT_CHOICE,
        );
        run(&workspace);
        resolve(
            workspace.root(),
            "go-application",
            "apt/primary",
            DECLINE_CHOICE,
        );
        resolve(
            workspace.root(),
            "go-application",
            "aur/primary",
            DECLINE_CHOICE,
        );
        resolve(
            workspace.root(),
            "go-application",
            "goreleaser",
            ACCEPT_CHOICE,
        );

        let result = run(&workspace);
        assert_eq!(result.state, ExecutorInitState::Ready);
        assert!(
            !result.plan.candidates.iter().any(|candidate| candidate
                .choices
                .iter()
                .any(|choice| choice.publisher == Some(PublisherKind::Homebrew))),
            "a publisher needing a tap repository is configured directly"
        );
        let baseline = workspace.root().join("component/.goreleaser.yaml");
        assert!(baseline.is_file(), "the packager baseline is created");
        assert!(std::fs::read_to_string(&baseline)
            .expect("baseline readable")
            .contains("project_name: component"));
        assert!(Config::load(workspace.root())
            .expect("config loads")
            .release_units["component"]
            .rpm
            .is_some());
    }

    #[test]
    fn reports_evidence_drift_on_resume_without_dropping_resolutions() {
        let workspace = Workspace::new("init-drift");
        workspace.write(".intentional/config.yml", CONFIG).write(
            "component/package.json",
            r#"{"name":"example-component","version":"1.0.0"}"#,
        );
        let first = run(&workspace);
        resolve(
            workspace.root(),
            "node-package",
            "npm/primary",
            DECLINE_CHOICE,
        );
        workspace.write(
            "component/package.json",
            r#"{"name":"example-component","version":"2.0.0"}"#,
        );

        let result = initialize_executor(workspace.root()).expect("executor init runs");
        assert_ne!(
            result.plan.source_fingerprint, first.plan.source_fingerprint,
            "the fingerprint follows the evidence"
        );
        assert!(
            result
                .operations
                .iter()
                .any(|operation| operation.contains("component/package.json changed")),
            "drift is reported: {:?}",
            result.operations
        );
        assert_eq!(
            result.plan.candidates[0].resolution.as_deref(),
            Some(DECLINE_CHOICE),
            "a decision about publication intent survives a content change"
        );
    }

    #[test]
    fn persists_only_outstanding_operations_in_the_plan() {
        let workspace = Workspace::new("init-operations");
        workspace.write(".intentional/config.yml", CONFIG).write(
            "component/package.json",
            r#"{"name":"example-component","version":"1.0.0"}"#,
        );
        run(&workspace);
        resolve(
            workspace.root(),
            "node-package",
            "npm/primary",
            ACCEPT_CHOICE,
        );
        run(&workspace);
        resolve(
            workspace.root(),
            "node-package",
            "npm/github",
            DECLINE_CHOICE,
        );

        let result = run(&workspace);
        assert_eq!(result.state, ExecutorInitState::Ready);
        assert!(
            result
                .operations
                .iter()
                .any(|operation| operation.contains("configure the npm primary publisher")),
            "the command reports what it configured"
        );
        assert!(
            result
                .plan
                .planned_operations
                .iter()
                .all(|operation| operation.starts_with("report: ")),
            "the persisted plan carries only outstanding work and standing prerequisites: {:?}",
            result.plan.planned_operations
        );

        // The applying run drops the now-configured candidate from the plan, so
        // the run after it is the first that can be a true no-op.
        run(&workspace);
        let repeated = run(&workspace);
        assert!(
            repeated.planned_writes().is_empty(),
            "a converged rerun writes nothing"
        );
    }

    #[test]
    fn reports_the_canonical_configuration_rewrite_before_applying_it() {
        let workspace = Workspace::new("init-rewrite");
        workspace
            .write(
                ".intentional/config.yml",
                &format!("# repository comment\n{CONFIG}"),
            )
            .write(
                "component/package.json",
                r#"{"name":"example-component","version":"1.0.0"}"#,
            );
        run(&workspace);
        resolve(
            workspace.root(),
            "node-package",
            "npm/primary",
            DECLINE_CHOICE,
        );

        let result = initialize_executor(workspace.root()).expect("executor init runs");
        assert!(
            result
                .operations
                .iter()
                .any(|operation| operation.contains("comments, key order, and formatting")),
            "the canonical rewrite is reported before it happens: {:?}",
            result.operations
        );
        assert!(
            std::fs::read_to_string(workspace.root().join(".intentional/config.yml"))
                .expect("config readable")
                .starts_with("# repository comment"),
            "the report precedes any write"
        );
    }

    #[test]
    fn withholds_decisions_from_suspended_release_units() {
        let workspace = Workspace::new("init-suspended");
        workspace
            .write(
                ".intentional/config.yml",
                &CONFIG.replace(
                    "    path: component\n",
                    "    path: component\n    disposition: suspended\n",
                ),
            )
            .write(
                "component/package.json",
                r#"{"name":"example-component","version":"1.0.0"}"#,
            );
        let result = run(&workspace);
        assert_eq!(result.state, ExecutorInitState::Ready);
        assert!(
            result.plan.candidates.is_empty(),
            "a suspended release unit is never offered publication intent"
        );
    }

    #[test]
    fn withholds_decisions_it_cannot_apply_on_its_own() {
        let workspace = Workspace::new("init-withheld");
        workspace
            .write(".intentional/config.yml", CONFIG)
            .write("component/Dockerfile", "FROM scratch\n")
            .write(
                "component/devcontainer-feature.json",
                r#"{"id":"example","version":"1.0.0"}"#,
            );
        let result = run(&workspace);
        assert_eq!(
            result.state,
            ExecutorInitState::Ready,
            "no candidate is offered when every target is ambiguous or needs explicit data"
        );
        assert!(
            result.plan.candidates.is_empty(),
            "two capabilities publishing one OCI target are resolved in configuration, not guessed"
        );
    }

    #[test]
    fn leaves_an_unchanged_configuration_file_untouched() {
        let workspace = Workspace::new("init-unchanged");
        workspace.write(".intentional/config.yml", CONFIG).write(
            "component/package.json",
            r#"{"name":"example-component","version":"1.0.0"}"#,
        );
        run(&workspace);
        resolve(
            workspace.root(),
            "node-package",
            "npm/primary",
            DECLINE_CHOICE,
        );
        assert_eq!(run(&workspace).state, ExecutorInitState::Ready);
        let path = workspace.root().join(".intentional/config.yml");
        let converged = std::fs::read_to_string(&path).expect("config readable");
        std::fs::write(&path, format!("# repository comment\n{converged}"))
            .expect("annotate config");

        assert_eq!(run(&workspace).state, ExecutorInitState::Ready);
        assert!(
            std::fs::read_to_string(&path)
                .expect("config readable")
                .starts_with("# repository comment"),
            "a converged plan never rewrites an unchanged configuration file"
        );
    }

    #[test]
    fn plan_matches_the_published_executor_plan_schema() {
        let workspace = Workspace::new("init-schema");
        workspace.write(".intentional/config.yml", CONFIG).write(
            "component/package.json",
            r#"{"name":"example-component","version":"1.0.0"}"#,
        );
        let plan = run(&workspace).plan;
        let document: serde_yaml::Value =
            serde_yaml::from_str(&plan.to_yaml().expect("plan serializes")).expect("plan parses");
        let schema: serde_yaml::Value =
            serde_yaml::from_str(include_str!("../../../../schemas/executor-init-plan.yml"))
                .expect("schema parses");
        assert_eq!(
            schema["$id"].as_str(),
            Some(EXECUTOR_INIT_PLAN_SCHEMA),
            "the runtime schema identifier matches the published schema"
        );
        for required in schema["required"]
            .as_sequence()
            .expect("required properties")
        {
            let key = required.as_str().expect("property name");
            assert!(document.get(key).is_some(), "plan carries {key}");
        }
        for required in schema["$defs"]["candidate"]["required"]
            .as_sequence()
            .expect("required candidate properties")
        {
            let key = required.as_str().expect("property name");
            assert!(
                document["candidates"][0].get(key).is_some(),
                "candidate carries {key}"
            );
        }
        assert!(document["candidates"][0]["resolution"].is_null());
    }
}
