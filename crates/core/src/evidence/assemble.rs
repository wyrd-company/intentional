// ---
// relationships:
//   implements: github-release-executor
// ---

//! Deterministic assembly of one final release-evidence bundle.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::evidence::contribution::{
    namespace_hash, parse_artifact_name, ContributionArtifactName, ContributionManifest,
    ATTACHMENTS_DIRECTORY, CONTRIBUTION_ARTIFACT_PREFIX, CONTRIBUTION_MANIFEST,
    CONTRIBUTION_SCHEMA,
};
use crate::evidence::{copy_and_digest, digest_file, is_digest, is_git_object, write_bundle};
use crate::executor::recipe::resolve_publications;
use crate::model::{AttachedComponent, PublisherKind, TagPhase};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Schema identity of the final release-evidence statement.
pub const RELEASE_EVIDENCE_SCHEMA: &str = "https://intentional.foo/schemas/release-evidence/v1";
/// Contract identity of the final release-evidence statement.
pub const RELEASE_EVIDENCE_CONTRACT: &str = "release-evidence-1";
/// File name of the final release-evidence statement.
pub const RELEASE_EVIDENCE_FILE: &str = "intentional-evidence.yml";
/// Schema identity of one publisher evidence fragment.
pub const PUBLISHER_EVIDENCE_SCHEMA: &str = "https://intentional.foo/schemas/publisher-evidence/v1";
/// Contract identity of one publisher evidence fragment.
pub const PUBLISHER_EVIDENCE_CONTRACT: &str = "publisher-evidence-1";
/// Schema identity of canonical phase-tag evidence.
pub const PHASE_TAG_EVIDENCE_SCHEMA: &str = "https://intentional.foo/schemas/phase-tag-evidence/v1";

/// Annotated tag identity recorded by evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TagIdentity {
    /// Tag name.
    pub name: String,
    /// Annotated tag object.
    pub object: String,
    /// Commit the tag targets.
    pub target: String,
}

/// Supply-chain reference bound to a digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EvidenceReference {
    /// Reference kind.
    pub kind: String,
    /// Digest the reference binds.
    pub digest: String,
    /// Location of the referenced material.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

/// Component a publisher attached to its published subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttachedMetadata {
    /// Attached component kind.
    pub kind: AttachedComponent,
    /// Digest of the attached component.
    pub digest: String,
    /// Location of the attached component.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

/// Built subject a publisher distributed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subject {
    /// Subject kind.
    pub kind: String,
    /// Subject identity at its destination.
    pub identity: String,
    /// Published version.
    pub version: String,
    /// Immutable subject digest.
    pub digest: String,
}

/// Packager that produced a subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackagerRecord {
    /// Packager identity.
    pub id: String,
    /// Packager version.
    pub version: String,
}

/// Destination readback recorded by a publisher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Destination {
    /// Destination identity.
    pub identity: String,
    /// Version observed at the destination.
    pub version: String,
    /// Digest observed at the destination.
    pub digest: String,
}

/// Retrieval mode a clean-client check actually performed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CleanClientMode {
    /// Normal public consumer retrieval.
    Public,
    /// Authenticated retrieval of a draft-dependent asset before closure.
    AuthenticatedDraft,
}

/// Consumer verification a publisher truthfully performed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CleanClient {
    /// Retrieval mode performed.
    pub mode: CleanClientMode,
    /// Client used for retrieval.
    pub client: String,
    /// Client version.
    pub version: String,
    /// Digest of the retrieved subject.
    pub digest: String,
}

/// Mutable destination alias observed at publication time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DestinationAlias {
    /// Alias name.
    pub name: String,
    /// Digest the alias resolved to.
    pub digest: String,
}

/// One publisher evidence fragment produced by a verification job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PublisherEvidence {
    /// Publisher evidence schema identity.
    #[serde(rename = "$schema")]
    pub schema: String,
    /// Publisher evidence contract identity.
    pub contract: String,
    /// Release unit the publication belongs to.
    pub release_unit: String,
    /// Publisher adapter that performed the publication.
    pub publisher: PublisherKind,
    /// Canonical target identity.
    pub target: String,
    /// Source commit S.
    pub source_commit: String,
    /// Release commit R.
    pub release_commit: String,
    /// Global release tag identity.
    pub global_tag: TagIdentity,
    /// Digest of the sealed release plan.
    pub plan_digest: String,
    /// Published subject.
    pub subject: Subject,
    /// Packager that produced the subject.
    pub packager: PackagerRecord,
    /// Native build provenance.
    pub build_provenance: Vec<EvidenceReference>,
    /// Components attached to the subject.
    pub attached_metadata: Vec<AttachedMetadata>,
    /// Destination readback.
    pub destination: Destination,
    /// Consumer verification actually performed.
    pub clean_client: CleanClient,
    /// Mutable aliases observed at publication time.
    pub destination_aliases: Vec<DestinationAlias>,
    /// Phase tags bound to this publication.
    pub phase_tags: Vec<TagIdentity>,
}

impl PublisherEvidence {
    /// Stable publication identity of this fragment.
    pub fn identity(&self) -> String {
        format!("{}/{}/{}", self.release_unit, self.publisher, self.target)
    }
}

/// One subject a phase tag sealed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PhaseSubject {
    /// Release unit the subject belongs to.
    pub release_unit: String,
    /// Subject identity.
    pub identity: String,
    /// Subject version.
    pub version: String,
    /// Immutable subject digest.
    pub digest: String,
    /// Native provenance sealed with the subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Vec<EvidenceReference>>,
}

/// One destination a before-publication tag intended to publish.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct IntendedDestination {
    /// Release unit intending to publish.
    pub release_unit: String,
    /// Publisher adapter.
    pub publisher: PublisherKind,
    /// Canonical target identity.
    pub target: String,
}

/// Canonical evidence encoded by one executor phase tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PhaseTagEvidence {
    /// Phase-tag evidence schema identity.
    #[serde(rename = "$schema")]
    pub schema: String,
    /// Declared executor phase.
    pub phase: TagPhase,
    /// Source commit S.
    pub source_commit: String,
    /// Release commit R.
    pub release_commit: String,
    /// Global release tag name.
    pub global_tag: String,
    /// Digest of the sealed release plan.
    pub plan_digest: String,
    /// Subjects sealed by the phase tag.
    pub subjects: Vec<PhaseSubject>,
    /// Destinations a before-publication tag intends to publish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intended_destinations: Option<Vec<IntendedDestination>>,
    /// Publisher evidence sealed by an after-publication tag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_evidence: Option<Vec<PublisherEvidence>>,
}

/// Release identity every accepted fragment agrees on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ReleaseIdentity {
    /// Source commit S.
    pub source_commit: String,
    /// Release commit R.
    pub release_commit: String,
    /// Global release tag identity.
    pub global_tag: TagIdentity,
    /// Digest of the sealed release plan.
    pub plan_digest: String,
}

/// GitHub workflow run that assembled the evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WorkflowIdentity {
    /// Owner and repository.
    pub repository: String,
    /// Workflow name.
    pub workflow: String,
    /// Workflow run identifier.
    pub run_id: u64,
    /// Workflow run attempt.
    pub run_attempt: u32,
    /// Commit the run executed against.
    pub commit: String,
}

impl WorkflowIdentity {
    /// Report every mechanical problem in this workflow identity.
    fn findings(&self) -> Vec<String> {
        let mut findings = Vec::new();
        let (owner, name) = self.repository.split_once('/').unwrap_or(("", ""));
        let segment = |value: &str| {
            !value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
        };
        if !segment(owner) || !segment(name) {
            findings.push(format!(
                "workflow repository {:?} is not owner/name",
                self.repository
            ));
        }
        if self.workflow.is_empty() {
            findings.push("workflow name is empty".to_owned());
        }
        if self.run_id == 0 {
            findings.push("workflow run identifier is missing".to_owned());
        }
        if self.run_attempt == 0 {
            findings.push("workflow run attempt is missing".to_owned());
        }
        if !is_git_object(&self.commit) {
            findings.push(format!(
                "workflow commit {:?} is not a complete Git object identifier",
                self.commit
            ));
        }
        findings
    }
}

/// Publisher evidence for one release unit, addressable by publisher and target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseUnitEvidence {
    /// Publisher adapters that published this release unit.
    pub publishers: BTreeMap<String, PublisherTargets>,
}

/// Publisher evidence addressable by canonical target identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublisherTargets {
    /// Accepted fragments keyed by canonical target identity.
    pub targets: BTreeMap<String, PublisherEvidence>,
}

/// One accepted contributed Release asset bound to its contributor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContributionAttachmentRecord {
    /// Contributor namespace that supplied the asset.
    pub namespace: String,
    /// Flat GitHub Release asset name.
    pub name: String,
    /// Digest of the accepted asset.
    pub sha256: String,
}

/// The durable release-wide evidence statement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ReleaseEvidence {
    /// Release evidence schema identity.
    #[serde(rename = "$schema")]
    pub schema: String,
    /// Release evidence contract identity.
    pub contract: String,
    /// Release identity every fragment agrees on.
    pub release: ReleaseIdentity,
    /// Workflow run that produced this statement.
    pub workflow: WorkflowIdentity,
    /// Accepted publisher evidence by release unit.
    pub release_units: BTreeMap<String, ReleaseUnitEvidence>,
    /// Repository-owned contribution values, uninterpreted.
    pub contributions: BTreeMap<String, serde_yaml::Value>,
    /// Accepted contributed Release assets.
    pub contribution_attachments: Vec<ContributionAttachmentRecord>,
}

impl ReleaseEvidence {
    /// Render the statement as its durable YAML document.
    pub fn to_yaml(&self) -> Result<String> {
        Ok(serde_yaml::to_string(self)?)
    }
}

/// Inputs of one `intentional evidence assemble` invocation.
#[derive(Debug, Clone)]
pub struct AssembleRequest<'a> {
    /// Workspace whose configuration determines the expected publications.
    pub root: &'a Path,
    /// Directory containing downloaded evidence artifacts.
    pub input: &'a Path,
    /// Directory the closed bundle is written into.
    pub output: &'a Path,
    /// Workflow run performing the assembly.
    pub workflow: WorkflowIdentity,
}

/// One closed evidence bundle.
#[derive(Debug, Clone, PartialEq)]
pub struct Assembly {
    /// Directory containing the statement and its attachments.
    pub path: PathBuf,
    /// Written release-evidence statement path.
    pub evidence_path: PathBuf,
    /// Accepted attachment names in document order.
    pub attachments: Vec<String>,
    /// Assembled statement.
    pub evidence: ReleaseEvidence,
}

/// One contribution artifact selected for assembly.
#[derive(Debug, Clone)]
struct SelectedContribution {
    directory: PathBuf,
    job: String,
    run_attempt: u32,
}

/// Assemble publisher fragments and contributions into final release evidence.
pub fn assemble(request: &AssembleRequest<'_>) -> Result<Assembly> {
    let mut findings = request.workflow.findings();
    if !request.input.is_dir() {
        return Err(Error::Validation(format!(
            "evidence input directory {} does not exist",
            request.input.display()
        )));
    }

    let config = Config::load(request.root)?;
    let selection = resolve_publications(request.root, &config)?;
    findings.extend(selection.diagnostics);
    let expected: BTreeSet<String> = selection
        .selected
        .iter()
        .map(|publication| publication.identity())
        .collect();

    let scan = scan_input(request.input, &mut findings)?;
    let release_units = accept_publisher_evidence(&scan.publishers, &expected, &mut findings);
    let release = release_identity(&scan, &mut findings);
    compare_phase_evidence(&scan, release.as_ref(), &mut findings);
    let accepted = accept_contributions(&scan.contributions, &mut findings)?;

    if !findings.is_empty() {
        return Err(Error::Validation(findings.join("\n")));
    }
    let release = release.expect("an accepted release identity");

    let mut contributions = BTreeMap::new();
    let mut contribution_attachments = Vec::new();
    for contribution in &accepted {
        if let Some(value) = &contribution.manifest.value {
            contributions.insert(contribution.manifest.namespace.clone(), value.clone());
        }
        for attachment in &contribution.manifest.attachments {
            contribution_attachments.push(ContributionAttachmentRecord {
                namespace: contribution.manifest.namespace.clone(),
                name: attachment.name.clone(),
                sha256: attachment.sha256.clone(),
            });
        }
    }
    contribution_attachments.sort_by(|left, right| left.name.cmp(&right.name));

    let evidence = ReleaseEvidence {
        schema: RELEASE_EVIDENCE_SCHEMA.to_owned(),
        contract: RELEASE_EVIDENCE_CONTRACT.to_owned(),
        release,
        workflow: request.workflow.clone(),
        release_units,
        contributions,
        contribution_attachments,
    };

    let mut sources = BTreeMap::new();
    for contribution in &accepted {
        for attachment in &contribution.manifest.attachments {
            sources.insert(
                attachment.name.clone(),
                (
                    contribution.directory.join(&attachment.file),
                    attachment.sha256.clone(),
                ),
            );
        }
    }
    write_bundle(request.output, "evidence", |staging| {
        let attachments_directory = staging.join(ATTACHMENTS_DIRECTORY);
        if !evidence.contribution_attachments.is_empty() {
            std::fs::create_dir_all(&attachments_directory)
                .map_err(|error| Error::io(&attachments_directory, error))?;
        }
        for (name, (source, expected_digest)) in &sources {
            let digest = copy_and_digest(source, &attachments_directory.join(name))?;
            if &digest != expected_digest {
                return Err(Error::Validation(format!(
                    "contributed attachment {name} changed while it was being assembled"
                )));
            }
        }
        let evidence_path = staging.join(RELEASE_EVIDENCE_FILE);
        std::fs::write(&evidence_path, evidence.to_yaml()?)
            .map_err(|error| Error::io(&evidence_path, error))
    })?;
    Ok(Assembly {
        path: request.output.to_path_buf(),
        evidence_path: request.output.join(RELEASE_EVIDENCE_FILE),
        attachments: sources.keys().cloned().collect(),
        evidence,
    })
}

/// Keys that only canonical phase-tag evidence carries.
const PHASE_TAG_KEYS: [&str; 3] = ["phase", "intended-destinations", "publisher-evidence"];

/// Everything one input directory offers assembly.
#[derive(Debug, Default)]
struct ScannedInput {
    publishers: Vec<(PathBuf, PublisherEvidence)>,
    phases: Vec<(PathBuf, PhaseTagEvidence)>,
    contributions: Vec<(PathBuf, ContributionArtifactName)>,
}

/// Classify every artifact the input directory carries.
///
/// Contribution artifacts are recognized by their protocol-owned artifact name
/// so their attachment payloads are never scanned as evidence, and every other
/// artifact is classified by document identity so publisher jobs stay free to
/// name their own artifacts.
fn scan_input(input: &Path, findings: &mut Vec<String>) -> Result<ScannedInput> {
    let mut scan = ScannedInput::default();
    let mut entries = std::fs::read_dir(input)
        .map_err(|error| Error::io(input, error))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| Error::io(input, error))?;
    entries.sort_by_key(std::fs::DirEntry::path);
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(CONTRIBUTION_ARTIFACT_PREFIX) {
            match parse_artifact_name(&name) {
                Some(parsed) if path.is_dir() => scan.contributions.push((path, parsed)),
                _ => findings.push(format!(
                    "contribution artifact {name} does not use the contribution transport naming"
                )),
            }
            continue;
        }
        classify(&path, &mut scan, findings)?;
    }
    Ok(scan)
}

/// Classify every YAML document beneath one non-contribution artifact.
fn classify(path: &Path, scan: &mut ScannedInput, findings: &mut Vec<String>) -> Result<()> {
    let mut candidates: Vec<PathBuf> = walkdir::WalkDir::new(path)
        .follow_links(false)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.into_path())
        .filter(|candidate| {
            matches!(
                candidate
                    .extension()
                    .and_then(|extension| extension.to_str()),
                Some("yml" | "yaml")
            )
        })
        .collect();
    candidates.sort();
    for candidate in candidates {
        let text =
            std::fs::read_to_string(&candidate).map_err(|error| Error::io(&candidate, error))?;
        let Ok(document) = serde_yaml::from_str::<serde_yaml::Value>(&text) else {
            continue;
        };
        let schema = document.get("$schema").and_then(serde_yaml::Value::as_str);
        let label = candidate.display();
        match schema {
            Some(PUBLISHER_EVIDENCE_SCHEMA) => match serde_yaml::from_str(&text) {
                Ok(fragment) => scan.publishers.push((candidate.clone(), fragment)),
                Err(error) => findings.push(format!(
                    "publisher evidence {label} is not schema-valid: {error}"
                )),
            },
            Some(PHASE_TAG_EVIDENCE_SCHEMA) => match serde_yaml::from_str(&text) {
                Ok(phase) => scan.phases.push((candidate.clone(), phase)),
                Err(error) => findings.push(format!(
                    "phase-tag evidence {label} is not schema-valid: {error}"
                )),
            },
            Some(CONTRIBUTION_SCHEMA) => findings.push(format!(
                "contribution manifest {label} is outside a contribution transport artifact"
            )),
            // Evidence that resembles a phase-tag document but does not identify
            // itself is reported rather than skipped, so a drifted artifact can
            // never be mistaken for evidence that was never supplied.
            _ if PHASE_TAG_KEYS.iter().any(|key| document.get(*key).is_some()) => {
                findings.push(format!(
                    "{label} carries phase-tag evidence that does not declare $schema {PHASE_TAG_EVIDENCE_SCHEMA}"
                ))
            }
            _ => {}
        }
    }
    Ok(())
}

/// Accept exactly one fragment for every configured publication.
fn accept_publisher_evidence(
    fragments: &[(PathBuf, PublisherEvidence)],
    expected: &BTreeSet<String>,
    findings: &mut Vec<String>,
) -> BTreeMap<String, ReleaseUnitEvidence> {
    let mut accepted: BTreeMap<String, ReleaseUnitEvidence> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for (path, fragment) in fragments {
        let identity = fragment.identity();
        findings.extend(publisher_findings(fragment, &path.display().to_string()));
        if !expected.contains(&identity) {
            findings.push(format!(
                "{} carries unexpected publisher evidence {identity}; configured publications are {}",
                path.display(),
                if expected.is_empty() {
                    "none".to_owned()
                } else {
                    expected.iter().cloned().collect::<Vec<_>>().join(", ")
                }
            ));
            continue;
        }
        if !seen.insert(identity.clone()) {
            findings.push(format!(
                "publisher evidence {identity} was supplied more than once"
            ));
            continue;
        }
        accepted
            .entry(fragment.release_unit.clone())
            .or_insert_with(|| ReleaseUnitEvidence {
                publishers: BTreeMap::new(),
            })
            .publishers
            .entry(fragment.publisher.to_string())
            .or_insert_with(|| PublisherTargets {
                targets: BTreeMap::new(),
            })
            .targets
            .insert(fragment.target.clone(), fragment.clone());
    }
    for identity in expected.difference(&seen) {
        findings.push(format!("publisher evidence for {identity} is missing"));
    }
    accepted
}

/// Report every mechanical problem in one publisher fragment.
fn publisher_findings(fragment: &PublisherEvidence, label: &str) -> Vec<String> {
    let mut findings = Vec::new();
    if fragment.contract != PUBLISHER_EVIDENCE_CONTRACT {
        findings.push(format!(
            "{label} declares contract {} instead of {PUBLISHER_EVIDENCE_CONTRACT}",
            fragment.contract
        ));
    }
    for (field, value) in [
        ("source-commit", &fragment.source_commit),
        ("release-commit", &fragment.release_commit),
        ("global-tag.object", &fragment.global_tag.object),
        ("global-tag.target", &fragment.global_tag.target),
    ] {
        if !is_git_object(value) {
            findings.push(format!(
                "{label} records {field} {value:?}, which is not a complete Git object identifier"
            ));
        }
    }
    for (field, value) in [
        ("plan-digest", &fragment.plan_digest),
        ("subject.digest", &fragment.subject.digest),
    ] {
        if !is_digest(value) {
            findings.push(format!(
                "{label} records {field} {value:?}, which is not a sha256 digest"
            ));
        }
    }
    if fragment.global_tag.name.is_empty() {
        findings.push(format!("{label} records an empty global tag name"));
    }
    if fragment.release_unit.is_empty() || fragment.target.is_empty() {
        findings.push(format!("{label} records an empty release unit or target"));
    }
    findings
}

/// Determine the one release identity every fragment agrees on.
fn release_identity(scan: &ScannedInput, findings: &mut Vec<String>) -> Option<ReleaseIdentity> {
    let mut identity: Option<ReleaseIdentity> = None;
    for (path, fragment) in &scan.publishers {
        let candidate = ReleaseIdentity {
            source_commit: fragment.source_commit.clone(),
            release_commit: fragment.release_commit.clone(),
            global_tag: fragment.global_tag.clone(),
            plan_digest: fragment.plan_digest.clone(),
        };
        match &identity {
            None => identity = Some(candidate),
            Some(accepted) if accepted == &candidate => {}
            Some(_) => findings.push(format!(
                "{} disagrees with other evidence about the release identity",
                path.display()
            )),
        }
    }
    if identity.is_none() {
        // Only publisher evidence carries the annotated global tag object the
        // statement binds, so an assembly without one cannot identify its
        // release at all.
        findings.push(
            "no publisher evidence identifies the release; final evidence requires at least one \
             accepted publisher fragment"
                .to_owned(),
        );
    }
    identity
}

/// Reject phase-tag evidence that disagrees with the accepted release identity.
fn compare_phase_evidence(
    scan: &ScannedInput,
    release: Option<&ReleaseIdentity>,
    findings: &mut Vec<String>,
) {
    let Some(release) = release else {
        return;
    };
    for (path, phase) in &scan.phases {
        let label = path.display();
        if phase.source_commit != release.source_commit
            || phase.release_commit != release.release_commit
            || phase.global_tag != release.global_tag.name
            || phase.plan_digest != release.plan_digest
        {
            findings.push(format!(
                "{label} disagrees with the accepted release identity"
            ));
            continue;
        }
        for subject in &phase.subjects {
            for (fragment_path, fragment) in &scan.publishers {
                if fragment.release_unit != subject.release_unit
                    || fragment.subject.identity != subject.identity
                {
                    continue;
                }
                if fragment.subject.digest != subject.digest
                    || fragment.subject.version != subject.version
                {
                    findings.push(format!(
                        "{} records subject {} as {}@{} while {label} sealed {}@{}",
                        fragment_path.display(),
                        subject.identity,
                        fragment.subject.version,
                        fragment.subject.digest,
                        subject.version,
                        subject.digest
                    ));
                }
            }
        }
    }
}

/// One contribution accepted into the final evidence.
#[derive(Debug, Clone)]
struct AcceptedContribution {
    directory: PathBuf,
    manifest: ContributionManifest,
}

/// Resolve retries, reject collisions, and verify every contributed file.
fn accept_contributions(
    artifacts: &[(PathBuf, ContributionArtifactName)],
    findings: &mut Vec<String>,
) -> Result<Vec<AcceptedContribution>> {
    // Retry resolution happens before any manifest is read so a superseded
    // attempt can never fail assembly for a job that later succeeded.
    let mut latest: BTreeMap<(String, String), SelectedContribution> = BTreeMap::new();
    for (directory, name) in artifacts {
        let key = (name.namespace_hash.clone(), name.job.clone());
        let selected = SelectedContribution {
            directory: directory.clone(),
            job: name.job.clone(),
            run_attempt: name.run_attempt,
        };
        match latest.get(&key) {
            Some(existing) if existing.run_attempt > selected.run_attempt => {}
            Some(existing) if existing.run_attempt == selected.run_attempt => {
                findings.push(format!(
                    "job {} staged the same contributor namespace twice in run attempt {}",
                    selected.job, selected.run_attempt
                ));
            }
            _ => {
                latest.insert(key, selected);
            }
        }
    }

    let mut accepted = Vec::new();
    let mut owners: BTreeMap<String, String> = BTreeMap::new();
    let mut assets: BTreeMap<String, String> = BTreeMap::new();
    for ((claimed_hash, _), selected) in &latest {
        let manifest_path = selected.directory.join(CONTRIBUTION_MANIFEST);
        let label = manifest_path.display().to_string();
        let Ok(text) = std::fs::read_to_string(&manifest_path) else {
            findings.push(format!("contribution manifest {label} is missing"));
            continue;
        };
        let manifest: ContributionManifest = match serde_yaml::from_str(&text) {
            Ok(manifest) => manifest,
            Err(error) => {
                findings.push(format!(
                    "contribution manifest {label} is not schema-valid: {error}"
                ));
                continue;
            }
        };
        let manifest_findings = manifest.findings(&label);
        if !manifest_findings.is_empty() {
            findings.extend(manifest_findings);
            continue;
        }
        if &namespace_hash(&manifest.namespace) != claimed_hash {
            findings.push(format!(
                "contribution manifest {label} declares namespace {} which its transport artifact does not identify",
                manifest.namespace
            ));
            continue;
        }
        if let Some(previous) = owners.insert(manifest.namespace.clone(), selected.job.clone()) {
            if previous != selected.job {
                findings.push(format!(
                    "contributor namespace {} is claimed by jobs {previous} and {}",
                    manifest.namespace, selected.job
                ));
                continue;
            }
        }
        let mut usable = true;
        for attachment in &manifest.attachments {
            let file = selected.directory.join(&attachment.file);
            if !file.is_file() {
                findings.push(format!(
                    "contribution {} inventories {} but the bundle does not contain it",
                    manifest.namespace, attachment.file
                ));
                usable = false;
                continue;
            }
            let digest = digest_file(&file)?;
            if digest != attachment.sha256 {
                findings.push(format!(
                    "contribution attachment digest does not match for {} in {}",
                    attachment.name, manifest.namespace
                ));
                usable = false;
                continue;
            }
            if let Some(previous) =
                assets.insert(attachment.name.clone(), manifest.namespace.clone())
            {
                findings.push(format!(
                    "GitHub Release attachment name {} is contributed by both {previous} and {}",
                    attachment.name, manifest.namespace
                ));
                usable = false;
            }
        }
        if usable {
            accepted.push(AcceptedContribution {
                directory: selected.directory.clone(),
                manifest,
            });
        }
    }
    accepted.sort_by(|left, right| left.manifest.namespace.cmp(&right.manifest.namespace));
    Ok(accepted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evidence::contribution::{artifact_name, contribute, ContributionRequest};
    use crate::executor::fixture::Workspace;

    const SOURCE: &str = "1111111111111111111111111111111111111111";
    const RELEASE: &str = "2222222222222222222222222222222222222222";
    const TAG_OBJECT: &str = "3333333333333333333333333333333333333333";
    const PLAN_DIGEST: &str =
        "sha256:4444444444444444444444444444444444444444444444444444444444444444";
    const SUBJECT_DIGEST: &str =
        "sha256:5555555555555555555555555555555555555555555555555555555555555555";

    const CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
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
      primary: { role: primary, template: '{id}@{version}' }
"#;

    fn workflow() -> WorkflowIdentity {
        WorkflowIdentity {
            repository: "example-owner/example-repository".to_owned(),
            workflow: "publish".to_owned(),
            run_id: 42,
            run_attempt: 1,
            commit: RELEASE.to_owned(),
        }
    }

    fn workspace(label: &str) -> Workspace {
        let workspace = Workspace::new(label);
        workspace.write(".intentional/config.yml", CONFIG).write(
            "component/package.json",
            r#"{"name":"example-component","version":"1.0.0"}"#,
        );
        workspace
    }

    fn fragment(release_unit: &str, publisher: &str, target: &str) -> String {
        format!(
            r#"$schema: {PUBLISHER_EVIDENCE_SCHEMA}
contract: {PUBLISHER_EVIDENCE_CONTRACT}
release-unit: {release_unit}
publisher: {publisher}
target: {target}
source-commit: {SOURCE}
release-commit: {RELEASE}
global-tag:
  name: release/1.0.0
  object: {TAG_OBJECT}
  target: {RELEASE}
plan-digest: {PLAN_DIGEST}
subject:
  kind: npm-package
  identity: example-component
  version: 1.0.0
  digest: {SUBJECT_DIGEST}
packager:
  id: npm
  version: 10.8.2
build-provenance: []
attached-metadata: []
destination:
  identity: registry.example.test/example-component
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
        )
    }

    /// Stage one contribution artifact exactly as its workflow transport would.
    fn stage(
        workspace: &Workspace,
        input: &Path,
        namespace: &str,
        job: &str,
        run_attempt: u32,
        value: Option<&str>,
        attachments: &[(&str, &str)],
    ) -> PathBuf {
        let staging = workspace
            .root()
            .join(format!("staging/{namespace}-{job}-{run_attempt}"));
        std::fs::create_dir_all(&staging).expect("staging directory");
        let value_file = value.map(|text| {
            let path = staging.join("value.yml");
            std::fs::write(&path, text).expect("value file");
            path
        });
        let files = attachments
            .iter()
            .map(|(name, contents)| {
                let path = staging.join(name);
                std::fs::write(&path, contents).expect("attachment");
                path
            })
            .collect::<Vec<_>>();
        let output = input.join(artifact_name(namespace, job, run_attempt));
        contribute(&ContributionRequest {
            namespace,
            value_file: value_file.as_deref(),
            attachments: &files,
            output: &output,
            job,
            run_attempt,
        })
        .expect("contribution staged");
        output
    }

    fn request<'a>(
        workspace: &'a Workspace,
        input: &'a Path,
        output: &'a Path,
    ) -> AssembleRequest<'a> {
        AssembleRequest {
            root: workspace.root(),
            input,
            output,
            workflow: workflow(),
        }
    }

    #[test]
    fn assembles_one_deterministic_bundle() {
        let workspace = workspace("assemble-bundle");
        let input = workspace.root().join("artifacts");
        std::fs::create_dir_all(input.join("publisher")).expect("publisher artifact");
        std::fs::write(
            input.join("publisher/publisher-evidence.yml"),
            fragment("component", "npm", "primary"),
        )
        .expect("fragment");
        stage(
            &workspace,
            &input,
            "assessment",
            "scan",
            1,
            Some("outcome: clean\nfindings: []\n"),
            &[("report.json", "{\"ok\":true}")],
        );
        stage(
            &workspace,
            &input,
            "observations",
            "observe",
            1,
            Some("- one\n- two\n"),
            &[],
        );

        let output = workspace.root().join("release-evidence");
        let assembly = assemble(&request(&workspace, &input, &output)).expect("assembly succeeds");
        assert_eq!(
            assembly.evidence.release,
            ReleaseIdentity {
                source_commit: SOURCE.to_owned(),
                release_commit: RELEASE.to_owned(),
                global_tag: TagIdentity {
                    name: "release/1.0.0".to_owned(),
                    object: TAG_OBJECT.to_owned(),
                    target: RELEASE.to_owned(),
                },
                plan_digest: PLAN_DIGEST.to_owned(),
            }
        );
        assert_eq!(
            assembly.evidence.release_units["component"].publishers["npm"].targets["primary"]
                .subject
                .digest,
            SUBJECT_DIGEST
        );
        assert_eq!(
            assembly.evidence.contributions["observations"],
            serde_yaml::from_str::<serde_yaml::Value>("- one\n- two\n").expect("sequence")
        );
        assert_eq!(assembly.evidence.contribution_attachments.len(), 1);
        assert_eq!(
            assembly.evidence.contribution_attachments[0].namespace,
            "assessment"
        );
        assert!(output.join("attachments/report.json").is_file());
        assert!(output.join(RELEASE_EVIDENCE_FILE).is_file());

        let repeat = workspace.root().join("release-evidence-repeat");
        let second = assemble(&request(&workspace, &input, &repeat)).expect("assembly repeats");
        assert_eq!(
            std::fs::read_to_string(&assembly.evidence_path).expect("first"),
            std::fs::read_to_string(&second.evidence_path).expect("second"),
            "assembly is deterministic"
        );
    }

    #[test]
    fn preserves_arbitrary_contributor_yaml_without_interpretation() {
        let workspace = workspace("assemble-opaque");
        let input = workspace.root().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment("component", "npm", "primary"),
        )
        .expect("fragment");
        let contributed =
            "verdict: {score: 7, notes: [ok, \"quoted string\"], nested: {deep: null}}\n";
        stage(
            &workspace,
            &input,
            "producer-defined",
            "assess",
            1,
            Some(contributed),
            &[],
        );
        let output = workspace.root().join("release-evidence");
        let assembly = assemble(&request(&workspace, &input, &output)).expect("assembly succeeds");
        assert_eq!(
            assembly.evidence.contributions["producer-defined"],
            serde_yaml::from_str::<serde_yaml::Value>(contributed).expect("value")
        );
    }

    #[test]
    fn selects_the_highest_attempt_of_a_retried_job() {
        let workspace = workspace("assemble-retry");
        let input = workspace.root().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment("component", "npm", "primary"),
        )
        .expect("fragment");
        let first = stage(
            &workspace,
            &input,
            "assessment",
            "scan",
            1,
            Some("attempt: 1\n"),
            &[],
        );
        stage(
            &workspace,
            &input,
            "assessment",
            "scan",
            2,
            Some("attempt: 2\n"),
            &[],
        );
        // A superseded attempt is ignored entirely, even when it is unusable.
        std::fs::write(first.join(CONTRIBUTION_MANIFEST), "not: [a, manifest\n").expect("corrupt");

        let output = workspace.root().join("release-evidence");
        let assembly = assemble(&request(&workspace, &input, &output)).expect("assembly succeeds");
        assert_eq!(
            assembly.evidence.contributions["assessment"],
            serde_yaml::from_str::<serde_yaml::Value>("attempt: 2\n").expect("value")
        );
    }

    #[test]
    fn rejects_a_namespace_claimed_by_two_jobs() {
        let workspace = workspace("assemble-namespace-collision");
        let input = workspace.root().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment("component", "npm", "primary"),
        )
        .expect("fragment");
        stage(
            &workspace,
            &input,
            "assessment",
            "scan",
            1,
            Some("a: 1\n"),
            &[],
        );
        stage(
            &workspace,
            &input,
            "assessment",
            "audit",
            1,
            Some("a: 2\n"),
            &[],
        );
        let output = workspace.root().join("release-evidence");
        let error =
            assemble(&request(&workspace, &input, &output)).expect_err("collision rejected");
        assert!(error.to_string().contains("is claimed by jobs"), "{error}");
        assert!(!output.exists(), "a rejected assembly writes nothing");
    }

    #[test]
    fn rejects_a_release_asset_name_collision() {
        let workspace = workspace("assemble-asset-collision");
        let input = workspace.root().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment("component", "npm", "primary"),
        )
        .expect("fragment");
        stage(
            &workspace,
            &input,
            "assessment",
            "scan",
            1,
            None,
            &[("report.json", "first")],
        );
        stage(
            &workspace,
            &input,
            "observations",
            "observe",
            1,
            None,
            &[("report.json", "second")],
        );
        let output = workspace.root().join("release-evidence");
        let error =
            assemble(&request(&workspace, &input, &output)).expect_err("collision rejected");
        assert!(
            error
                .to_string()
                .contains("GitHub Release attachment name report.json is contributed by both"),
            "{error}"
        );
        assert!(!output.exists(), "a rejected assembly writes nothing");
    }

    #[test]
    fn rejects_a_contribution_whose_attachment_no_longer_matches_its_digest() {
        let workspace = workspace("assemble-corrupt-attachment");
        let input = workspace.root().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment("component", "npm", "primary"),
        )
        .expect("fragment");
        let bundle = stage(
            &workspace,
            &input,
            "assessment",
            "scan",
            1,
            None,
            &[("report.json", "trusted")],
        );
        std::fs::write(bundle.join("attachments/report.json"), "tampered").expect("tamper");
        let output = workspace.root().join("release-evidence");
        let error = assemble(&request(&workspace, &input, &output)).expect_err("tamper rejected");
        assert!(
            error
                .to_string()
                .contains("contribution attachment digest does not match for report.json"),
            "{error}"
        );

        let missing = stage(
            &workspace,
            &input,
            "observations",
            "observe",
            1,
            None,
            &[("notes.txt", "present")],
        );
        std::fs::remove_file(missing.join("attachments/notes.txt")).expect("remove");
        let error = assemble(&request(&workspace, &input, &output)).expect_err("missing rejected");
        assert!(
            error
                .to_string()
                .contains("but the bundle does not contain it"),
            "{error}"
        );
    }

    #[test]
    fn rejects_attachment_paths_that_leave_the_bundle() {
        let workspace = workspace("assemble-traversal");
        let input = workspace.root().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment("component", "npm", "primary"),
        )
        .expect("fragment");
        let bundle = stage(
            &workspace,
            &input,
            "assessment",
            "scan",
            1,
            None,
            &[("report.json", "trusted")],
        );
        std::fs::write(workspace.root().join("secret.txt"), "secret").expect("secret");
        for (name, file) in [
            ("report.json", "../../secret.txt"),
            ("../escape.json", "attachments/../escape.json"),
            ("/etc/passwd", "attachments//etc/passwd"),
        ] {
            std::fs::write(
                bundle.join(CONTRIBUTION_MANIFEST),
                format!(
                    "$schema: {CONTRIBUTION_SCHEMA}\nnamespace: assessment\nattachments:\n  - name: {name:?}\n    file: {file:?}\n    sha256: {SUBJECT_DIGEST}\n"
                ),
            )
            .expect("manifest");
            let output = workspace
                .root()
                .join(format!("release-evidence-{}", name.len()));
            let error =
                assemble(&request(&workspace, &input, &output)).expect_err("traversal rejected");
            let message = error.to_string();
            assert!(
                message.contains("unusable Release asset name") || message.contains("instead of"),
                "{message}"
            );
            assert!(!output.exists(), "a rejected assembly writes nothing");
        }
    }

    #[test]
    fn reports_missing_and_unexpected_publisher_evidence_in_one_run() {
        let workspace = workspace("assemble-fragments");
        let input = workspace.root().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("unexpected.yml"),
            fragment("component", "cargo", "primary"),
        )
        .expect("fragment");
        let output = workspace.root().join("release-evidence");
        let error =
            assemble(&request(&workspace, &input, &output)).expect_err("fragments rejected");
        let message = error.to_string();
        assert!(
            message.contains("unexpected publisher evidence component/cargo/primary"),
            "{message}"
        );
        assert!(
            message.contains("publisher evidence for component/npm/primary is missing"),
            "{message}"
        );
    }

    #[test]
    fn rejects_duplicate_publisher_evidence() {
        let workspace = workspace("assemble-duplicate");
        let input = workspace.root().join("artifacts");
        std::fs::create_dir_all(input.join("first")).expect("first");
        std::fs::create_dir_all(input.join("second")).expect("second");
        std::fs::write(
            input.join("first/publisher-evidence.yml"),
            fragment("component", "npm", "primary"),
        )
        .expect("fragment");
        std::fs::write(
            input.join("second/publisher-evidence.yml"),
            fragment("component", "npm", "primary"),
        )
        .expect("fragment");
        let output = workspace.root().join("release-evidence");
        let error =
            assemble(&request(&workspace, &input, &output)).expect_err("duplicate rejected");
        assert!(
            error.to_string().contains("was supplied more than once"),
            "{error}"
        );
    }

    #[test]
    fn rejects_phase_tag_evidence_that_disagrees_with_publisher_evidence() {
        let workspace = workspace("assemble-phase");
        let input = workspace.root().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment("component", "npm", "primary"),
        )
        .expect("fragment");
        std::fs::write(
            input.join("phase-tag-evidence.yml"),
            format!(
                r#"$schema: {PHASE_TAG_EVIDENCE_SCHEMA}
phase: before-publication
source-commit: {SOURCE}
release-commit: {RELEASE}
global-tag: release/1.0.0
plan-digest: {PLAN_DIGEST}
subjects:
  - release-unit: component
    identity: example-component
    version: 1.0.1
    digest: {SUBJECT_DIGEST}
intended-destinations:
  - release-unit: component
    publisher: npm
    target: primary
"#
            ),
        )
        .expect("phase evidence");
        let output = workspace.root().join("release-evidence");
        let error =
            assemble(&request(&workspace, &input, &output)).expect_err("disagreement rejected");
        assert!(
            error.to_string().contains("while") && error.to_string().contains("sealed"),
            "{error}"
        );
    }

    #[test]
    fn reports_phase_tag_evidence_that_does_not_identify_itself() {
        let workspace = workspace("assemble-unidentified-phase");
        let input = workspace.root().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment("component", "npm", "primary"),
        )
        .expect("fragment");
        std::fs::write(
            input.join("phase-tag-evidence.yml"),
            format!(
                r#"phase: before-publication
source-commit: {SOURCE}
release-commit: {RELEASE}
tag: release/1.0.0
plan-digest: {PLAN_DIGEST}
subjects: []
"#
            ),
        )
        .expect("phase evidence");
        let output = workspace.root().join("release-evidence");
        let error =
            assemble(&request(&workspace, &input, &output)).expect_err("silent drop rejected");
        assert!(
            error
                .to_string()
                .contains("carries phase-tag evidence that does not declare $schema"),
            "{error}"
        );
    }

    #[test]
    fn requires_publisher_evidence_to_identify_the_release() {
        let workspace = workspace("assemble-unidentified");
        let input = workspace.root().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        let output = workspace.root().join("release-evidence");
        let error = assemble(&request(&workspace, &input, &output)).expect_err("rejected");
        assert!(
            error
                .to_string()
                .contains("no publisher evidence identifies the release"),
            "{error}"
        );
    }

    #[test]
    fn includes_available_contributions_without_waiting_for_absent_ones() {
        let workspace = workspace("assemble-best-effort");
        let input = workspace.root().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment("component", "npm", "primary"),
        )
        .expect("fragment");
        stage(
            &workspace,
            &input,
            "assessment",
            "scan",
            1,
            Some("ok: true\n"),
            &[],
        );
        let output = workspace.root().join("release-evidence");
        let assembly = assemble(&request(&workspace, &input, &output)).expect("assembly succeeds");
        assert_eq!(assembly.evidence.contributions.len(), 1);
        assert!(assembly.evidence.contributions.contains_key("assessment"));
    }

    #[test]
    fn refuses_to_assemble_into_a_populated_directory() {
        let workspace = workspace("assemble-populated");
        let input = workspace.root().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment("component", "npm", "primary"),
        )
        .expect("fragment");
        let output = workspace.root().join("release-evidence");
        std::fs::create_dir_all(&output).expect("output");
        std::fs::write(output.join(RELEASE_EVIDENCE_FILE), "stale").expect("stale evidence");
        let error =
            assemble(&request(&workspace, &input, &output)).expect_err("populated output rejected");
        assert!(error.to_string().contains("is not empty"), "{error}");
        assert_eq!(
            std::fs::read_to_string(output.join(RELEASE_EVIDENCE_FILE)).expect("stale"),
            "stale",
            "assembly never replaces evidence incrementally"
        );
    }

    #[test]
    fn rejects_an_unusable_workflow_identity() {
        let workspace = workspace("assemble-workflow");
        let input = workspace.root().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment("component", "npm", "primary"),
        )
        .expect("fragment");
        let output = workspace.root().join("release-evidence");
        let mut request = request(&workspace, &input, &output);
        request.workflow.repository = "example-owner".to_owned();
        request.workflow.commit = "abc".to_owned();
        request.workflow.run_id = 0;
        let error = assemble(&request).expect_err("workflow identity rejected");
        let message = error.to_string();
        assert!(message.contains("is not owner/name"), "{message}");
        assert!(message.contains("run identifier is missing"), "{message}");
        assert!(
            message.contains("not a complete Git object identifier"),
            "{message}"
        );
    }
}
