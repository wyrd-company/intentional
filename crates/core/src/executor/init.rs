// ---
// relationships:
//   implements: github-release-executor
// ---

//! Executor initialization plan creation, resumption, and application.

use crate::config::{
    Config, ExcludedPathReceipt, GithubConfig, GithubWorkflow, GithubWorkflows, ManagedPathReceipt,
    NpmAdditionalTargets, NpmGithubTarget, NpmPublisher, OciPublisher, PackageConfig,
    ReleaseUnitConfig, CONFIG_PATH, DEFAULT_PUBLISH_WORKFLOW, DEFAULT_RELEASE_WORKFLOW,
};
use crate::error::{Error, Result};
use crate::executor::recipe::{
    capability_set, derive_capabilities, derive_package_candidates, recipes_for,
    select_publications, Capability, CapabilityEvidence, PackageCandidateEvidence, Packager,
    PRIMARY_TARGET,
};
use crate::init::SourceEvidence;
use crate::model::{PublisherKind, ReleaseUnitDisposition};
use crate::plan::canonical_json;
use crate::yaml_edit::Document;
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
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
    /// Whether one discovered native artifact becomes a configured package.
    Package,
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
    /// Proposed package identifier, for a package candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    /// Release-unit-relative package directory, for a package candidate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Detector whose exact artifact is accepted or declined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detector: Option<String>,
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
            CandidateKind::Package => b"package".as_slice(),
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
        if self.package.is_none()
            || (self.kind == CandidateKind::Package
                && (self.path.is_none() || self.detector.is_none()))
        {
            return Err(Error::Validation(format!(
                "executor candidate {} must name its package; package candidates must also name their path and detector",
                self.id
            )));
        }
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
    let config_path = root.join(CONFIG_PATH);
    let config_text =
        std::fs::read_to_string(&config_path).map_err(|error| Error::io(&config_path, error))?;
    let mut config = Config::from_yaml(&config_text)?;
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

    let mut writes = Vec::new();
    // Configuration is edited in place rather than re-serialized, so the file
    // keeps its comments and never gains defaults the user did not write.
    let mut edits: Vec<(Vec<String>, Value)> = Vec::new();
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
            edits.push((
                vec!["github".to_owned()],
                serde_yaml::to_value(&inferred_github)?,
            ));
        }
        for candidate in &candidates {
            apply_candidate(
                root,
                &mut config,
                candidate,
                &mut writes,
                &mut operations,
                &mut edits,
            )?;
        }
        config.validate()?;
        select_publications(root, &config)?;
        if let Some(updated) = edited_config(&config_text, &edits, &config)? {
            operations.push(format!(
                "update {CONFIG_PATH} in place; comments, key order, and formatting outside the edited keys are preserved"
            ));
            writes.push((PathBuf::from(CONFIG_PATH), updated));
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

/// Apply the planned configuration edits to the exact file the user wrote.
///
/// The edited document must mean exactly what the validated model means; a
/// disagreement is a defect in the edit, never something to write out.
fn edited_config(
    text: &str,
    edits: &[(Vec<String>, Value)],
    expected: &Config,
) -> Result<Option<String>> {
    if edits.is_empty() {
        return Ok(None);
    }
    let mut document = Document::parse(text)?;
    for (path, value) in edits {
        let path = path.iter().map(String::as_str).collect::<Vec<_>>();
        document.set(&path, value)?;
    }
    let updated = document.into_text();
    if &Config::from_yaml(&updated)? != expected {
        return Err(Error::Validation(format!(
            "editing {CONFIG_PATH} in place did not reproduce the planned configuration"
        )));
    }
    Ok((updated != text).then_some(updated))
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
        declined_publications: BTreeSet::new(),
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
        let packages = package_candidates(root, config, id, release_unit, resolutions)?;
        candidates.extend(packages);
        if release_unit.packages.is_empty() {
            continue;
        }
        let derived = derive_capabilities(root, config, id)?;
        let capabilities = capability_set(&derived);
        for evidence in &derived {
            let package = &release_unit.packages[&evidence.package];
            for (publisher, target) in offered_targets(evidence.capability, &capabilities) {
                let identity = format!("{}/{}/{publisher}/{target}", id, evidence.package);
                if config
                    .github
                    .as_ref()
                    .is_some_and(|github| github.declined_publications.contains(&identity))
                    || configured(package, publisher, &target)
                    || !prerequisite_met(
                        package,
                        &candidates,
                        publisher,
                        &target,
                        id,
                        &evidence.package,
                    )
                {
                    continue;
                }
                candidates.push(publication_candidate(
                    id,
                    &evidence.package,
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

fn package_candidates(
    root: &Path,
    config: &Config,
    release_unit: &str,
    release_unit_config: &ReleaseUnitConfig,
    resolutions: &Resolutions,
) -> Result<Vec<ExecutorCandidate>> {
    let mut proposed = Vec::new();
    let mut identifiers = release_unit_config
        .packages
        .iter()
        .map(|(id, package)| {
            (
                id.clone(),
                crate::config::join_relative_paths(&release_unit_config.path, &package.path),
            )
        })
        .collect::<BTreeMap<_, _>>();
    for artifact in derive_package_candidates(root, config, release_unit)? {
        let path = artifact
            .directory
            .strip_prefix(&release_unit_config.path)
            .unwrap_or(&artifact.directory);
        let path = if path.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            path.to_owned()
        };
        let package = proposed_package_id(&artifact, &path)?;
        if let Some(first) = identifiers.insert(package.clone(), artifact.evidence.path.clone()) {
            return Err(Error::Validation(format!(
                "release unit {release_unit} package candidate identifier {package} collides for artifacts {} and {}",
                first.display(), artifact.evidence.path.display()
            )));
        }
        let capability = artifact.capability.as_str();
        let scope = artifact.evidence.path.display().to_string();
        let id =
            ExecutorCandidate::stable_id(CandidateKind::Package, release_unit, capability, &scope);
        let capabilities = BTreeSet::from([artifact.capability]);
        let mut choices = offered_targets(artifact.capability, &capabilities)
            .into_iter()
            .filter(|(publisher, target)| !(*publisher == PublisherKind::Npm && target == "github"))
            .map(|(publisher, target)| Choice {
                id: format!("accept-{publisher}-{target}"),
                label: format!(
                    "Configure package {package} at {} for {publisher} {target}",
                    path.display()
                ),
                publisher: Some(publisher),
                target: Some(target),
                packager: None,
            })
            .collect::<Vec<_>>();
        choices.push(Choice {
            id: DECLINE_CHOICE.to_owned(),
            label: format!(
                "Do not publish the artifact at {}",
                artifact.evidence.path.display()
            ),
            publisher: None,
            target: None,
            packager: None,
        });
        proposed.push(ExecutorCandidate {
            resolution: carried_resolution(&id, resolutions),
            id,
            kind: CandidateKind::Package,
            release_unit: release_unit.to_owned(),
            package: Some(package),
            path: Some(path),
            detector: Some(artifact.detector),
            capability: capability.to_owned(),
            evidence: vec![artifact.evidence],
            recommended: (choices.len() == 2).then(|| choices[0].id.clone()),
            choices,
        });
    }
    Ok(proposed)
}

fn proposed_package_id(artifact: &PackageCandidateEvidence, path: &Path) -> Result<String> {
    let proposed = path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| *name != ".")
        .map(str::to_owned)
        .or_else(|| artifact.native_identity.clone())
        .ok_or_else(|| Error::Validation(format!(
            "artifact {} has neither a directory name nor native identity for its package candidate",
            artifact.evidence.path.display()
        )))?;
    Ok(proposed
        .rsplit('/')
        .next()
        .unwrap_or(&proposed)
        .trim_start_matches('@')
        .to_owned())
}

/// Publisher targets a capability can offer, ordered by publisher then target.
///
/// Initialization offers only decisions it can apply on its own. A target that
/// needs repository data no evidence supplies, or that more than one derived
/// capability could publish, is configured directly instead.
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
    package_config: &PackageConfig,
    candidates: &[ExecutorCandidate],
    publisher: PublisherKind,
    target: &str,
    release_unit: &str,
    package: &str,
) -> bool {
    if publisher != PublisherKind::Npm || target != "github" {
        return true;
    }
    if configured(package_config, PublisherKind::Npm, PRIMARY_TARGET) {
        return true;
    }
    candidates.iter().any(|candidate| {
        candidate.release_unit == release_unit
            && candidate.package.as_deref() == Some(package)
            && candidate.resolution.as_deref() == Some(ACCEPT_CHOICE)
            && candidate
                .selected()
                .is_some_and(|choice| choice.target.as_deref() == Some(PRIMARY_TARGET))
    })
}

fn configured(package: &PackageConfig, publisher: PublisherKind, target: &str) -> bool {
    match (publisher, target) {
        (PublisherKind::Npm, "github") => package
            .npm
            .as_ref()
            .and_then(|npm| npm.additional_targets.as_ref())
            .is_some_and(|targets| targets.github.is_some()),
        (PublisherKind::Npm, _) => package.npm.is_some(),
        (PublisherKind::Cargo, _) => package.cargo.is_some(),
        (PublisherKind::Homebrew, _) => package.homebrew.is_some(),
        (PublisherKind::Rpm, _) => package.rpm.is_some(),
        (PublisherKind::Apt, _) => package.apt.is_some(),
        (PublisherKind::Aur, _) => package.aur.is_some(),
        (PublisherKind::Oci, "dockerhub") => package
            .oci
            .as_ref()
            .is_some_and(|oci| oci.dockerhub.is_some()),
        (PublisherKind::Oci, _) => package.oci.as_ref().is_some_and(|oci| oci.ghcr.is_some()),
    }
}

fn publication_candidate(
    release_unit: &str,
    package: &str,
    evidence: &CapabilityEvidence,
    publisher: PublisherKind,
    target: &str,
    resolutions: &Resolutions,
) -> ExecutorCandidate {
    let capability = evidence.capability.as_str();
    let scope = format!("{package}/{publisher}/{target}");
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
        package: Some(package.to_owned()),
        path: None,
        detector: None,
        capability: capability.to_owned(),
        evidence,
        choices: vec![
            Choice {
                id: ACCEPT_CHOICE.to_owned(),
                label: format!(
                    "Publish {release_unit}/{package} to {publisher} {target} using its maintained recipe"
                ),
                publisher: Some(publisher),
                target: Some(target.to_owned()),
                packager: None,
            },
            Choice {
                id: DECLINE_CHOICE.to_owned(),
                label: format!("Do not publish {release_unit}/{package} to {publisher} {target}"),
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
        let package =
            &config.release_units[&publication.release_unit].packages[&publication.package];
        required.insert(
            (
                publication.release_unit.clone(),
                publication.package.clone(),
                publication.packager,
            ),
            (publication.capability, package.path.clone(), Vec::new()),
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
                    (
                        candidate.release_unit.clone(),
                        candidate
                            .package
                            .clone()
                            .expect("validated candidate package"),
                        recipe.packager,
                    ),
                    (
                        recipe.capability,
                        candidate
                            .path
                            .clone()
                            .or_else(|| {
                                config.release_units[&candidate.release_unit]
                                    .packages
                                    .get(candidate.package.as_deref()?)
                                    .map(|package| package.path.clone())
                            })
                            .expect("candidate package path is configured or proposed"),
                        candidate.evidence.clone(),
                    ),
                );
            }
        }
    }

    let mut derived = Vec::new();
    for ((release_unit, package_id, packager), (capability, package_path, candidate_evidence)) in
        required
    {
        let release_unit_config = &config.release_units[&release_unit];
        if !packager.baseline_is_authorable()
            || packager_configured(root, release_unit_config, &package_path, packager)
        {
            continue;
        }
        let evidence = if candidate_evidence.is_empty() {
            derive_capabilities(root, config, &release_unit)?
                .into_iter()
                .filter(|item| item.package == package_id && item.capability == capability)
                .map(|item| item.evidence)
                .collect::<Vec<_>>()
        } else {
            candidate_evidence
        };
        if evidence.is_empty() {
            continue;
        }
        let id = ExecutorCandidate::stable_id(
            CandidateKind::Packager,
            &release_unit,
            capability.as_str(),
            &format!("{package_id}/{packager}"),
        );
        derived.push(ExecutorCandidate {
            resolution: carried_resolution(&id, resolutions),
            id,
            kind: CandidateKind::Packager,
            release_unit: release_unit.clone(),
            package: Some(package_id.clone()),
            path: None,
            detector: None,
            capability: capability.as_str().to_owned(),
            evidence,
            choices: vec![
                Choice {
                    id: ACCEPT_CHOICE.to_owned(),
                    label: format!(
                        "Create the baseline {packager} configuration for {release_unit}/{package_id}"
                    ),
                    publisher: None,
                    target: None,
                    packager: Some(packager),
                },
                Choice {
                    id: DECLINE_CHOICE.to_owned(),
                    label: format!(
                        "Author the {packager} configuration for {release_unit}/{package_id} outside Intentional"
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

fn packager_configured(
    root: &Path,
    release_unit: &ReleaseUnitConfig,
    package_path: &Path,
    packager: Packager,
) -> bool {
    packager.configuration_paths().iter().any(|relative| {
        root.join(&release_unit.path)
            .join(package_path)
            .join(relative)
            .is_file()
    })
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
    edits: &mut Vec<(Vec<String>, Value)>,
) -> Result<()> {
    let Some(choice) = candidate.selected() else {
        return Ok(());
    };
    if choice.id == DECLINE_CHOICE {
        if candidate.kind == CandidateKind::Package {
            let detector = candidate
                .detector
                .as_ref()
                .expect("package candidate detector");
            let evidence = candidate
                .evidence
                .first()
                .expect("package candidate evidence");
            config.discovery.excluded_paths.push(ExcludedPathReceipt {
                detector: detector.clone(),
                path: evidence.path.clone(),
                evidence_digest: evidence.digest.clone(),
            });
            edits.push((
                vec!["discovery".to_owned(), "excluded-paths".to_owned()],
                serde_yaml::to_value(&config.discovery.excluded_paths)?,
            ));
            operations.push(format!(
                "record the declined artifact {} in {CONFIG_PATH}",
                evidence.path.display()
            ));
        } else if candidate.kind == CandidateKind::PublicationIntent {
            let accepted = candidate
                .choices
                .iter()
                .find(|choice| choice.id == ACCEPT_CHOICE)
                .expect("publication candidate has an accept choice");
            let publisher = accepted
                .publisher
                .expect("publication accept choice has a publisher");
            let target = accepted
                .target
                .as_deref()
                .expect("publication accept choice has a target");
            let package = candidate
                .package
                .as_deref()
                .expect("publication candidate has a package");
            let identity = format!("{}/{package}/{publisher}/{target}", candidate.release_unit);
            config
                .github
                .as_mut()
                .expect("publication candidates require github configuration")
                .declined_publications
                .insert(identity.clone());
            edits.push((
                vec!["github".to_owned(), "declined-publications".to_owned()],
                serde_yaml::to_value(
                    &config
                        .github
                        .as_ref()
                        .expect("github configuration")
                        .declined_publications,
                )?,
            ));
            operations.push(format!(
                "record the declined publication {identity} in {CONFIG_PATH}"
            ));
        }
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
        CandidateKind::Package => {
            let package_id = candidate
                .package
                .as_ref()
                .expect("package candidate identifier");
            let path = candidate.path.clone().expect("package candidate path");
            let (Some(publisher), Some(target)) = (choice.publisher, choice.target.as_deref())
            else {
                return Err(Error::Validation(format!(
                    "executor candidate {} accepts a package without a publisher target",
                    candidate.id
                )));
            };
            let mut package = PackageConfig::new(path);
            enable_package_publisher(&mut package, publisher, target)?;
            release_unit
                .packages
                .insert(package_id.clone(), package.clone());
            edits.push((
                vec![
                    "release-units".to_owned(),
                    candidate.release_unit.clone(),
                    "packages".to_owned(),
                    package_id.clone(),
                ],
                serde_yaml::to_value(&package)?,
            ));
            let detector = candidate
                .detector
                .as_ref()
                .expect("package candidate detector");
            let evidence = candidate
                .evidence
                .first()
                .expect("package candidate evidence");
            config.discovery.managed_paths.push(ManagedPathReceipt {
                detector: detector.clone(),
                path: evidence.path.clone(),
                release_unit: candidate.release_unit.clone(),
                package: package_id.clone(),
            });
            edits.push((
                vec!["discovery".to_owned(), "managed-paths".to_owned()],
                serde_yaml::to_value(&config.discovery.managed_paths)?,
            ));
            operations.push(format!(
                "configure package {package_id} for {publisher} {target}"
            ));
        }
        CandidateKind::PublicationIntent => {
            let (Some(publisher), Some(target)) = (choice.publisher, choice.target.as_deref())
            else {
                return Err(Error::Validation(format!(
                    "executor candidate {} accepts publication without a publisher target",
                    candidate.id
                )));
            };
            let package_id = candidate
                .package
                .as_ref()
                .expect("validated publication candidate package");
            let package = release_unit.packages.get_mut(package_id).ok_or_else(|| {
                Error::Validation(format!(
                    "executor candidate {} names unknown package {package_id} in release unit {}",
                    candidate.id, candidate.release_unit
                ))
            })?;
            enable_package_publisher(package, publisher, target)?;
            edits.push((
                vec![
                    "release-units".to_owned(),
                    candidate.release_unit.clone(),
                    "packages".to_owned(),
                    package_id.clone(),
                    publisher.as_str().to_owned(),
                ],
                publisher_value(package, publisher)?,
            ));
            operations.push(format!(
                "configure the {publisher} {target} publisher for package {}/{}",
                candidate.release_unit, package_id
            ));
        }
        CandidateKind::Packager => {
            let Some(packager) = choice.packager else {
                return Err(Error::Validation(format!(
                    "executor candidate {} accepts a packager baseline without a packager",
                    candidate.id
                )));
            };
            let package_id = candidate
                .package
                .as_ref()
                .expect("validated packager candidate package");
            let package = release_unit.packages.get(package_id).ok_or_else(|| {
                Error::Validation(format!(
                    "executor candidate {} names unknown package {package_id} in release unit {}",
                    candidate.id, candidate.release_unit
                ))
            })?;
            let relative = crate::config::join_relative_paths(&release_unit.path, &package.path)
                .join(
                    packager
                        .configuration_paths()
                        .first()
                        .expect("packager declares configuration paths"),
                );
            if root.join(&relative).is_file() {
                return Ok(());
            }
            writes.push((relative.clone(), baseline(packager, package_id)?));
            operations.push(format!(
                "create the baseline {packager} configuration {}",
                relative.display()
            ));
        }
    }
    Ok(())
}

/// Serialized value of one release unit's configured publisher property.
fn publisher_value(package: &PackageConfig, publisher: PublisherKind) -> Result<Value> {
    Ok(match publisher {
        PublisherKind::Npm => serde_yaml::to_value(&package.npm)?,
        PublisherKind::Cargo => serde_yaml::to_value(&package.cargo)?,
        PublisherKind::Homebrew => serde_yaml::to_value(&package.homebrew)?,
        PublisherKind::Rpm => serde_yaml::to_value(&package.rpm)?,
        PublisherKind::Apt => serde_yaml::to_value(&package.apt)?,
        PublisherKind::Aur => serde_yaml::to_value(&package.aur)?,
        PublisherKind::Oci => serde_yaml::to_value(&package.oci)?,
    })
}

fn enable_package_publisher(
    package: &mut PackageConfig,
    publisher: PublisherKind,
    target: &str,
) -> Result<()> {
    match (publisher, target) {
        (PublisherKind::Npm, "github") => {
            let npm = package.npm.get_or_insert_with(NpmPublisher::default);
            npm.additional_targets
                .get_or_insert_with(NpmAdditionalTargets::default)
                .github
                .get_or_insert_with(NpmGithubTarget::default);
        }
        (PublisherKind::Npm, _) => {
            package.npm.get_or_insert_with(NpmPublisher::default);
        }
        (PublisherKind::Cargo, _) => {
            package.cargo.get_or_insert_with(Default::default);
        }
        (PublisherKind::Rpm, _) => {
            package.rpm.get_or_insert_with(Default::default);
        }
        (PublisherKind::Apt, _) => {
            package.apt.get_or_insert_with(Default::default);
        }
        (PublisherKind::Aur, _) => {
            package.aur.get_or_insert_with(Default::default);
        }
        (PublisherKind::Oci, "dockerhub") => {
            return Err(explicit_configuration_error(publisher, target))
        }
        (PublisherKind::Oci, _) => {
            package
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

/// Baseline native configuration for a packager whose contract authorizes one.
fn baseline(packager: Packager, release_unit: &str) -> Result<String> {
    match packager {
        Packager::GoReleaser => Ok(format!(
            "version: 2\nproject_name: {release_unit}\nbuilds:\n  - main: .\n    binary: {release_unit}\n    env:\n      - CGO_ENABLED=0\n    goos: [ linux, darwin, windows ]\n    goarch: [ amd64, arm64 ]\n"
        )),
        Packager::Npm | Packager::Cargo | Packager::Buildx | Packager::DevContainerCli => {
            Err(Error::Validation(format!(
                "Intentional authors no baseline {packager} configuration for {release_unit}"
            )))
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
contract: contract-2
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
        let (scope_kind, scope_target) = scope.split_once('/').unwrap_or((scope, ""));
        let mut matched = false;
        for candidate in &mut plan.candidates {
            let matches_choice = candidate.capability == capability
                && candidate.choices.iter().any(|candidate_choice| {
                    candidate_choice
                        .publisher
                        .is_some_and(|value| value.as_str() == scope_kind)
                        && candidate_choice.target.as_deref() == Some(scope_target)
                        || candidate_choice
                            .packager
                            .is_some_and(|value| value.as_str() == scope)
                });
            if candidate.kind != CandidateKind::Package && matches_choice {
                candidate.resolution = Some(choice.to_owned());
                matched = true;
            } else if candidate.kind == CandidateKind::Package
                && candidate.capability == capability
                && scope.contains('/')
            {
                candidate.resolution = Some(if choice == ACCEPT_CHOICE {
                    candidate
                        .choices
                        .iter()
                        .find(|candidate_choice| {
                            candidate_choice
                                .publisher
                                .is_some_and(|value| value.as_str() == scope_kind)
                                && candidate_choice.target.as_deref() == Some(scope_target)
                        })
                        .expect("package candidate offers publisher target")
                        .id
                        .clone()
                } else {
                    choice.to_owned()
                });
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
        assert_eq!(result.plan.candidates[0].kind, CandidateKind::Package);
        assert_eq!(
            result.plan.candidates[0].package.as_deref(),
            Some("example-component")
        );
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
    fn proposes_every_publishable_package_in_a_release_unit() {
        let workspace = Workspace::new("init-package-census");
        workspace
            .write(
                ".intentional/config.yml",
                &CONFIG.replace(
                    "    path: component\n",
                    "    path: .\n    projections:\n      - { adapter: toml, file: Cargo.toml, pointer: /workspace/package/version, mode: committed }\n      - { adapter: npm, file: npm/package.json, mode: committed }\n",
                ),
            )
            .write(
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/core\", \"crates/cli\"]\n[workspace.package]\nversion = \"1.0.0\"\n",
            )
            .write(
                "npm/package.json",
                r#"{"name":"example-launcher","version":"1.0.0"}"#,
            )
            .write(
                "crates/core/Cargo.toml",
                "[package]\nname = \"example-core\"\nversion = \"1.0.0\"\n",
            )
            .write(
                "crates/cli/Cargo.toml",
                "[package]\nname = \"example-cli\"\nversion = \"1.0.0\"\n",
            )
            .write(
                "fixtures/example/Cargo.toml",
                "[package]\nname = \"example-fixture\"\nversion = \"1.0.0\"\n",
            );

        let result = run(&workspace);
        let proposals = result
            .plan
            .candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.package.as_deref().expect("package identifier"),
                    candidate.path.as_deref().expect("package path"),
                    candidate.capability.as_str(),
                )
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            proposals,
            BTreeSet::from([
                ("cli", Path::new("crates/cli"), "rust-crate"),
                ("core", Path::new("crates/core"), "rust-crate"),
                ("npm", Path::new("npm"), "node-package"),
            ])
        );
    }

    #[test]
    fn reports_colliding_proposed_package_identifiers() {
        let workspace = Workspace::new("init-package-collision");
        workspace
            .write(
                ".intentional/config.yml",
                &CONFIG.replace("    path: component\n", "    path: .\n"),
            )
            .write(
                "package.json",
                r#"{"name":"example-root","private":true,"workspaces":["first/shared","second/shared"]}"#,
            )
            .write(
                "first/shared/package.json",
                r#"{"name":"example-package","version":"1.0.0"}"#,
            )
            .write(
                "second/shared/Cargo.toml",
                "[package]\nname = \"example-crate\"\nversion = \"1.0.0\"\n",
            );

        let error = initialize_executor(workspace.root()).expect_err("collision is reported");
        let message = error.to_string();
        assert!(message.contains("identifier shared collides"), "{message}");
        assert!(message.contains("first/shared/package.json"), "{message}");
        assert!(message.contains("second/shared/Cargo.toml"), "{message}");
    }

    #[test]
    fn declined_package_stays_declined_in_a_fresh_clone() {
        let workspace = Workspace::new("init-decline-clone");
        workspace.write(".intentional/config.yml", CONFIG).write(
            "component/package.json",
            r#"{"name":"example-component","version":"1.0.0"}"#,
        );
        let git = |arguments: &[&str], directory: &Path| {
            let status = std::process::Command::new("git")
                .args(arguments)
                .current_dir(directory)
                .status()
                .expect("git runs");
            assert!(status.success(), "git {arguments:?} succeeds");
        };
        git(&["init", "-q"], workspace.root());
        git(&["config", "user.name", "Example User"], workspace.root());
        git(
            &["config", "user.email", "user@example.test"],
            workspace.root(),
        );
        git(
            &["add", ".intentional/config.yml", "component/package.json"],
            workspace.root(),
        );
        git(&["commit", "-qm", "fixture"], workspace.root());

        run(&workspace);
        resolve(
            workspace.root(),
            "node-package",
            "npm/primary",
            DECLINE_CHOICE,
        );
        assert_eq!(run(&workspace).state, ExecutorInitState::Ready);
        git(&["add", ".intentional/config.yml"], workspace.root());
        git(&["commit", "-qm", "record decline"], workspace.root());

        let clone_parent = tempfile::tempdir().expect("clone parent");
        let clone = clone_parent.path().join("clone");
        git(
            &[
                "clone",
                "-q",
                workspace.root().to_str().expect("root UTF-8"),
                clone.to_str().expect("clone UTF-8"),
            ],
            clone_parent.path(),
        );
        assert!(
            !clone.join(EXECUTOR_INIT_PLAN_PATH).exists(),
            "the untracked plan does not survive the clone"
        );
        let cloned = initialize_executor(&clone).expect("executor init runs in clone");
        assert_eq!(cloned.state, ExecutorInitState::Ready);
        assert!(cloned.plan.candidates.is_empty());
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
        assert_eq!(result.state, ExecutorInitState::Ready);
        let result = run(&workspace);
        assert_eq!(result.state, ExecutorInitState::NeedsInput);
        assert_eq!(
            result.plan.candidates.len(),
            1,
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
        assert!(component.npm().is_some());
        assert!(component
            .npm()
            .expect("npm publisher")
            .additional_targets
            .is_none());
        assert_eq!(
            github.declined_publications,
            BTreeSet::from(["component/example-component/npm/github".to_owned()])
        );

        std::fs::remove_file(workspace.root().join(EXECUTOR_INIT_PLAN_PATH))
            .expect("remove transient plan");
        let repeated = run(&workspace);
        assert_eq!(
            repeated.state,
            ExecutorInitState::Ready,
            "a converged workspace stays ready without new questions"
        );
        assert!(
            repeated.plan.candidates.is_empty(),
            "tracked configuration keeps the declined additional target resolved"
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
            "goreleaser",
            ACCEPT_CHOICE,
        );
        run(&workspace);
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
        assert!(
            baseline.is_file(),
            "the packager baseline is created: {:?}",
            result.plan.candidates
        );
        assert!(std::fs::read_to_string(&baseline)
            .expect("baseline readable")
            .contains("project_name: component"));
        assert!(Config::load(workspace.root())
            .expect("config loads")
            .release_units["component"]
            .rpm()
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
        run(&workspace);
        resolve(
            workspace.root(),
            "node-package",
            "npm/primary",
            ACCEPT_CHOICE,
        );
        run(&workspace);
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
                .plan
                .planned_operations
                .iter()
                .all(|operation| operation.starts_with("report: ")),
            "the persisted plan carries only outstanding work and standing prerequisites: {:?}",
            result.plan.planned_operations
        );

        // The applying run drops the now-configured candidate from the plan, so
        // the next run refreshes the source fingerprint without it. The run
        // after that is the first that can be a true no-op.
        run(&workspace);
        run(&workspace);
        let repeated = run(&workspace);
        assert!(
            repeated.planned_writes().is_empty(),
            "a converged rerun writes nothing"
        );
    }

    #[test]
    fn preserves_comments_and_unwritten_defaults_when_it_edits_configuration() {
        let workspace = Workspace::new("init-preserve");
        workspace
            .write(
                ".intentional/config.yml",
                &format!("---\n# repository comment\n{CONFIG}"),
            )
            .write(
                "component/Cargo.toml",
                "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\n",
            );
        run(&workspace);
        resolve(
            workspace.root(),
            "rust-crate",
            "cargo/primary",
            ACCEPT_CHOICE,
        );

        let result = initialize_executor(workspace.root()).expect("executor init runs");
        result.apply(workspace.root(), false).expect("plan applies");
        assert!(
            result
                .operations
                .iter()
                .any(|operation| operation.contains("outside the edited keys are preserved")),
            "the in-place edit is reported before it happens: {:?}",
            result.operations
        );

        let updated = std::fs::read_to_string(workspace.root().join(".intentional/config.yml"))
            .expect("config readable");
        assert!(
            updated.starts_with("---\n# repository comment"),
            "repository comments survive an executor configuration edit: {updated}"
        );
        assert!(
            !updated.contains("settings:"),
            "an edit never materializes defaults the user did not write: {updated}"
        );
        assert!(
            updated.contains("cargo: {}"),
            "the accepted publisher is written into the release unit: {updated}"
        );
        assert_eq!(
            Config::from_yaml(&updated)
                .expect("edited config parses")
                .release_units["component"]
                .publishers(),
            vec![PublisherKind::Cargo]
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
    fn proposes_each_artifact_even_when_release_unit_level_targets_were_ambiguous() {
        let workspace = Workspace::new("init-withheld");
        workspace
            .write(".intentional/config.yml", CONFIG)
            .write("component/Dockerfile", "FROM scratch\n")
            .write(
                "component/devcontainer-feature.json",
                r#"{"id":"example","version":"1.0.0"}"#,
            );
        let result = run(&workspace);
        assert_eq!(result.state, ExecutorInitState::NeedsInput);
        assert_eq!(result.plan.candidates.len(), 2);
        assert!(
            result.plan.candidates.iter().all(|candidate| {
                candidate.kind == CandidateKind::Package
                    && candidate.choices.iter().any(|choice| {
                        choice.publisher == Some(PublisherKind::Oci)
                            && choice.target.as_deref() == Some("ghcr")
                    })
            }),
            "each artifact carries its own unambiguous GHCR publisher choice"
        );
    }

    #[test]
    fn scopes_publication_candidates_to_each_package_in_an_npm_workspace() {
        let workspace = Workspace::new("init-npm-workspace");
        workspace
            .write(
                ".intentional/config.yml",
                &CONFIG.replace("    path: component\n", "    path: .\n"),
            )
            .write(
                "package.json",
                r#"{"name":"example-root","private":true,"workspaces":["packages/*"]}"#,
            )
            .write(
                "packages/alpha/package.json",
                r#"{"name":"example-alpha","version":"1.0.0"}"#,
            )
            .write(
                "packages/beta/package.json",
                r#"{"name":"example-beta","version":"1.0.0"}"#,
            );

        let proposed = run(&workspace);
        assert_eq!(
            proposed
                .plan
                .candidates
                .iter()
                .map(|candidate| candidate.package.as_deref().expect("package"))
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["alpha", "beta"]),
            "native npm workspace membership admits every member and excludes the private root"
        );
        resolve(
            workspace.root(),
            "node-package",
            "npm/primary",
            ACCEPT_CHOICE,
        );
        run(&workspace);

        let publication = run(&workspace);
        let additional = publication
            .plan
            .candidates
            .iter()
            .filter(|candidate| candidate.kind == CandidateKind::PublicationIntent)
            .collect::<Vec<_>>();
        assert_eq!(
            additional.len(),
            2,
            "each package keeps its own publisher state"
        );
        assert_eq!(
            additional
                .iter()
                .map(|candidate| candidate.id.as_str())
                .collect::<BTreeSet<_>>()
                .len(),
            2,
            "package identity separates otherwise identical publication decisions"
        );
        assert_eq!(
            additional
                .iter()
                .map(|candidate| candidate.package.as_deref().expect("package"))
                .collect::<BTreeSet<_>>(),
            BTreeSet::from(["alpha", "beta"])
        );
        resolve(
            workspace.root(),
            "node-package",
            "npm/github",
            ACCEPT_CHOICE,
        );
        run(&workspace);
        let converged = run(&workspace);
        assert_eq!(converged.state, ExecutorInitState::Ready);
        assert!(converged.plan.candidates.is_empty());
        assert!(Config::load(workspace.root())
            .expect("config loads")
            .release_units["component"]
            .packages
            .values()
            .all(|package| package
                .npm
                .as_ref()
                .and_then(|npm| npm.additional_targets.as_ref())
                .is_some_and(|targets| targets.github.is_some())));
    }

    #[test]
    fn reads_publisher_state_from_the_candidate_package() {
        let workspace = Workspace::new("init-package-publisher-state");
        workspace
            .write(
                ".intentional/config.yml",
                r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-2
release-units:
  component:
    path: component
    packages:
      alpha:
        path: alpha
        npm: { additional-targets: { github: {} } }
      beta: { path: beta, npm: {} }
    tags:
      primary: { role: primary, template: '{id}@{version}' }
github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml }
"#,
            )
            .write(
                "component/alpha/package.json",
                r#"{"name":"example-alpha","version":"1.0.0"}"#,
            )
            .write(
                "component/beta/package.json",
                r#"{"name":"example-beta","version":"1.0.0"}"#,
            );

        let candidates = initialize_executor(workspace.root())
            .expect("executor init runs")
            .plan
            .candidates;
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].package.as_deref(), Some("beta"));
        assert!(candidates[0].choices.iter().any(|choice| {
            choice.publisher == Some(PublisherKind::Npm)
                && choice.target.as_deref() == Some("github")
        }));
    }

    #[test]
    fn one_packages_primary_does_not_unlock_its_siblings_additional_target() {
        let accepted = ExecutorCandidate {
            id: "candidate:accepted".to_owned(),
            kind: CandidateKind::Package,
            release_unit: "component".to_owned(),
            package: Some("alpha".to_owned()),
            path: Some(PathBuf::from("alpha")),
            detector: Some("npm-package".to_owned()),
            capability: "node-package".to_owned(),
            evidence: vec![SourceEvidence {
                path: PathBuf::from("component/alpha/package.json"),
                digest: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    .to_owned(),
                lines: Vec::new(),
            }],
            choices: vec![Choice {
                id: ACCEPT_CHOICE.to_owned(),
                label: "Configure alpha".to_owned(),
                publisher: Some(PublisherKind::Npm),
                target: Some(PRIMARY_TARGET.to_owned()),
                packager: None,
            }],
            recommended: None,
            resolution: Some(ACCEPT_CHOICE.to_owned()),
        };
        assert!(!prerequisite_met(
            &PackageConfig::new(PathBuf::from("beta")),
            &[accepted],
            PublisherKind::Npm,
            "github",
            "component",
            "beta",
        ));
    }

    #[test]
    fn creates_one_packager_baseline_in_each_package_directory() {
        let workspace = Workspace::new("init-package-packagers");
        workspace
            .write(
                ".intentional/config.yml",
                &format!(
                    "{}github:\n  workflows:\n    release: {{ path: .github/workflows/release.yml }}\n    publish: {{ path: .github/workflows/publish.yml }}\n",
                    CONFIG.replace(
                        "    path: component\n",
                        "    path: component\n    packages:\n      alpha: { path: alpha, rpm: {} }\n      beta: { path: beta, rpm: {} }\n",
                    )
                ),
            )
            .write("component/alpha/go.mod", "module example.test/alpha\n")
            .write("component/alpha/main.go", "package main\n\nfunc main() {}\n")
            .write("component/beta/go.mod", "module example.test/beta\n")
            .write("component/beta/main.go", "package main\n\nfunc main() {}\n");

        let mut plan = initialize_executor(workspace.root())
            .expect("executor init runs")
            .plan;
        let packagers = plan
            .candidates
            .iter_mut()
            .filter_map(|candidate| {
                if candidate.kind == CandidateKind::Packager {
                    candidate.resolution = Some(ACCEPT_CHOICE.to_owned());
                    candidate.package.clone()
                } else {
                    candidate.resolution = Some(DECLINE_CHOICE.to_owned());
                    None
                }
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            packagers,
            BTreeSet::from(["alpha".to_owned(), "beta".to_owned()])
        );
        plan.state = ExecutorInitState::Ready;
        workspace.write(
            EXECUTOR_INIT_PLAN_PATH,
            &serde_yaml::to_string(&plan).expect("plan serializes"),
        );
        let result = initialize_executor(workspace.root()).expect("resolved plan runs");
        result.apply(workspace.root(), false).expect("plan applies");
        assert!(workspace
            .root()
            .join("component/alpha/.goreleaser.yaml")
            .is_file());
        assert!(workspace
            .root()
            .join("component/beta/.goreleaser.yaml")
            .is_file());
        assert!(!workspace.root().join(".goreleaser.yaml").is_file());
    }

    #[test]
    fn an_existing_package_baseline_suppresses_only_that_package() {
        let workspace = Workspace::new("init-package-packager-state");
        workspace
            .write(
                ".intentional/config.yml",
                &format!(
                    "{}github:\n  workflows:\n    release: {{ path: .github/workflows/release.yml }}\n    publish: {{ path: .github/workflows/publish.yml }}\n",
                    CONFIG.replace(
                        "    path: component\n",
                        "    path: component\n    packages:\n      alpha: { path: alpha, rpm: {} }\n      beta: { path: beta, rpm: {} }\n",
                    )
                ),
            )
            .write("component/alpha/go.mod", "module example.test/alpha\n")
            .write("component/alpha/main.go", "package main\n\nfunc main() {}\n")
            .write("component/alpha/.goreleaser.yaml", "project_name: alpha\n")
            .write("component/beta/go.mod", "module example.test/beta\n")
            .write("component/beta/main.go", "package main\n\nfunc main() {}\n");

        let packagers = initialize_executor(workspace.root())
            .expect("executor init runs")
            .plan
            .candidates
            .into_iter()
            .filter(|candidate| candidate.kind == CandidateKind::Packager)
            .map(|candidate| candidate.package.expect("packager package"))
            .collect::<BTreeSet<_>>();
        assert_eq!(packagers, BTreeSet::from(["beta".to_owned()]));
    }

    #[test]
    fn bounds_candidates_by_native_membership_without_a_matching_projection() {
        let workspace = Workspace::new("init-native-bound");
        workspace
            .write(
                ".intentional/config.yml",
                &CONFIG.replace(
                    "    path: component\n",
                    "    path: .\n    projections:\n      - { adapter: npm, file: npm/package.json, mode: committed }\n",
                ),
            )
            .write(
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/alpha\", \"crates/beta\"]\n",
            )
            .write("crates/alpha/Cargo.toml", "[package]\nname='example-alpha'\nversion='1.0.0'\n")
            .write("crates/beta/Cargo.toml", "[package]\nname='example-beta'\nversion='1.0.0'\n")
            .write("crates/alpha/tests/fixture/Cargo.toml", "[package]\nname='example-fixture'\nversion='1.0.0'\n")
            .write("npm/package.json", r#"{"name":"example-npm","version":"1.0.0"}"#);

        let result = run(&workspace);
        let paths = result
            .plan
            .candidates
            .iter()
            .map(|candidate| candidate.evidence[0].path.as_path())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            paths,
            BTreeSet::from([
                Path::new("crates/alpha/Cargo.toml"),
                Path::new("crates/beta/Cargo.toml"),
                Path::new("npm/package.json"),
            ])
        );
    }

    #[test]
    fn bounds_go_commands_to_the_projected_module() {
        let workspace = Workspace::new("init-go-bound");
        workspace
            .write(
                ".intentional/config.yml",
                &CONFIG.replace(
                    "    path: component\n",
                    "    path: .\n    projections:\n      - { adapter: go, file: apps/alpha/go.mod, mode: committed }\n",
                ),
            )
            .write("apps/alpha/go.mod", "module example.test/alpha\n")
            .write("apps/alpha/main.go", "package main\n\nfunc main() {}\n")
            .write("apps/beta/go.mod", "module example.test/beta\n")
            .write("apps/beta/main.go", "package main\n\nfunc main() {}\n");

        let result = run(&workspace);
        assert_eq!(result.plan.candidates.len(), 1);
        assert_eq!(
            result.plan.candidates[0].path.as_deref(),
            Some(Path::new("apps/alpha"))
        );
    }

    #[test]
    fn excludes_private_and_unpublishable_manifests() {
        let workspace = Workspace::new("init-publishable-bound");
        workspace
            .write(
                ".intentional/config.yml",
                &CONFIG.replace("    path: component\n", "    path: .\n"),
            )
            .write(
                "package.json",
                r#"{"name":"example-private","private":true}"#,
            )
            .write(
                "Cargo.toml",
                "[package]\nname='example-private-crate'\nversion='1.0.0'\npublish=false\n",
            );
        assert!(run(&workspace).plan.candidates.is_empty());
    }

    #[test]
    fn contains_candidates_within_their_release_unit() {
        let workspace = Workspace::new("init-release-unit-bound");
        workspace
            .write(
                ".intentional/config.yml",
                &CONFIG.replace(
                    "  component:\n    path: component\n",
                    "  alpha:\n    path: alpha\n    tags:\n      primary: { role: primary, template: '{id}@{version}' }\n  beta:\n    path: beta\n",
                ),
            )
            .write("alpha/package.json", r#"{"name":"example-alpha","version":"1.0.0"}"#)
            .write("beta/package.json", r#"{"name":"example-beta","version":"1.0.0"}"#);
        let result = run(&workspace);
        assert_eq!(
            result
                .plan
                .candidates
                .iter()
                .map(|candidate| (
                    candidate.release_unit.as_str(),
                    candidate.package.as_deref()
                ))
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                ("alpha", Some("example-alpha")),
                ("beta", Some("example-beta")),
            ])
        );

        let images = Workspace::new("init-image-release-unit-bound");
        images
            .write(
                ".intentional/config.yml",
                &CONFIG.replace(
                    "  component:\n    path: component\n",
                    "  alpha:\n    path: alpha\n    tags:\n      primary: { role: primary, template: '{id}@{version}' }\n  beta:\n    path: beta\n",
                ),
            )
            .write("alpha/Dockerfile", "FROM scratch\n")
            .write("beta/Dockerfile", "FROM scratch\n");
        assert_eq!(
            run(&images)
                .plan
                .candidates
                .iter()
                .map(|candidate| (candidate.release_unit.as_str(), candidate.path.as_deref()))
                .collect::<BTreeSet<_>>(),
            BTreeSet::from([
                ("alpha", Some(Path::new("."))),
                ("beta", Some(Path::new("."))),
            ]),
            "image definitions remain inside their owning release unit"
        );
    }

    #[test]
    fn rejects_a_proposed_identifier_already_declared_by_hand() {
        let workspace = Workspace::new("init-declared-collision");
        workspace
            .write(
                ".intentional/config.yml",
                &CONFIG.replace(
                    "    path: component\n",
                    "    path: .\n    packages:\n      shared: { path: declared }\n",
                ),
            )
            .write(
                "package.json",
                r#"{"name":"example-root","private":true,"workspaces":["shared"]}"#,
            )
            .write(
                "declared/package.json",
                r#"{"name":"example-declared","private":true}"#,
            )
            .write(
                "shared/package.json",
                r#"{"name":"example-proposed","version":"1.0.0"}"#,
            );
        let error = initialize_executor(workspace.root()).expect_err("collision is reported");
        assert!(error.to_string().contains("identifier shared collides"));
    }

    #[test]
    fn validates_every_package_candidate_identity_field() {
        let workspace = Workspace::new("init-candidate-validation");
        workspace.write(".intentional/config.yml", CONFIG).write(
            "component/package.json",
            r#"{"name":"example-component","version":"1.0.0"}"#,
        );
        let candidate = run(&workspace).plan.candidates[0].clone();
        for missing in ["package", "path", "detector"] {
            let mut invalid = candidate.clone();
            match missing {
                "package" => invalid.package = None,
                "path" => invalid.path = None,
                "detector" => invalid.detector = None,
                _ => unreachable!(),
            }
            assert!(
                invalid
                    .validate()
                    .expect_err("missing identity is rejected")
                    .to_string()
                    .contains("must name its package"),
                "missing {missing} reaches the package-field validation"
            );
        }
    }

    #[test]
    fn normalizes_scoped_npm_identity_and_withholds_unready_choices() {
        let workspace = Workspace::new("init-choice-shape");
        workspace
            .write(
                ".intentional/config.yml",
                &CONFIG.replace("    path: component\n", "    path: .\n"),
            )
            .write(
                "package.json",
                r#"{"name":"@example/thing","version":"1.0.0"}"#,
            );
        let candidate = &run(&workspace).plan.candidates[0];
        assert_eq!(candidate.package.as_deref(), Some("thing"));
        assert!(
            candidate
                .choices
                .iter()
                .all(|choice| choice.target.as_deref() != Some("github")),
            "npm GitHub remains unavailable until primary publication is accepted"
        );

        let single_segment = Workspace::new("init-single-segment-scope");
        single_segment
            .write(
                ".intentional/config.yml",
                &CONFIG.replace("    path: component\n", "    path: .\n"),
            )
            .write("package.json", r#"{"name":"@thing","version":"1.0.0"}"#);
        assert_eq!(
            run(&single_segment).plan.candidates[0].package.as_deref(),
            Some("thing"),
            "a single-segment native scope marker is not part of package identity"
        );
    }

    #[test]
    fn recommends_only_a_single_supported_publisher_choice() {
        let workspace = Workspace::new("init-recommendation");
        workspace
            .write(".intentional/config.yml", CONFIG)
            .write("component/go.mod", "module example.test/component\n")
            .write("component/main.go", "package main\n\nfunc main() {}\n");
        let candidate = &run(&workspace).plan.candidates[0];
        assert!(candidate.choices.len() > 2);
        assert_eq!(candidate.recommended, None);
    }

    #[test]
    fn separates_candidate_kind_identity_domains() {
        let package = ExecutorCandidate::stable_id(
            CandidateKind::Package,
            "component",
            "node-package",
            "component/package.json",
        );
        let publication = ExecutorCandidate::stable_id(
            CandidateKind::PublicationIntent,
            "component",
            "node-package",
            "component/package.json",
        );
        assert_ne!(
            package, publication,
            "candidate kinds never carry each other's resolution"
        );
    }

    #[test]
    fn rejects_acceptance_for_an_unknown_release_unit() {
        let workspace = Workspace::new("init-unknown-unit");
        workspace.write(".intentional/config.yml", CONFIG).write(
            "component/package.json",
            r#"{"name":"example-component","version":"1.0.0"}"#,
        );
        let mut candidate = run(&workspace).plan.candidates[0].clone();
        candidate.release_unit = "absent".to_owned();
        candidate.resolution = candidate.recommended.clone();
        let mut config = Config::load(workspace.root()).expect("config loads");
        let error = apply_candidate(
            workspace.root(),
            &mut config,
            &candidate,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .expect_err("unknown release unit is rejected");
        assert!(error
            .to_string()
            .contains("names unknown release unit absent"));
    }

    #[test]
    fn rejects_package_acceptance_without_a_publisher_target() {
        let workspace = Workspace::new("init-package-choice-validation");
        workspace.write(".intentional/config.yml", CONFIG).write(
            "component/package.json",
            r#"{"name":"example-component","version":"1.0.0"}"#,
        );
        let mut candidate = run(&workspace).plan.candidates[0].clone();
        candidate.choices[0].publisher = None;
        candidate.resolution = Some(candidate.choices[0].id.clone());
        let mut config = Config::load(workspace.root()).expect("config loads");
        let error = apply_candidate(
            workspace.root(),
            &mut config,
            &candidate,
            &mut Vec::new(),
            &mut Vec::new(),
            &mut Vec::new(),
        )
        .expect_err("publisher-less package acceptance is rejected");
        assert!(error
            .to_string()
            .contains("accepts a package without a publisher target"));
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
        for key in ["package", "path", "detector"] {
            assert!(
                document["candidates"][0].get(key).is_some(),
                "package candidate carries {key}"
            );
        }
        assert!(document["candidates"][0]["resolution"].is_null());
    }
}
