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
use crate::evidence::identity::{fragment_disagreements, phase_disagreements, proved_release};
use crate::evidence::{copy_and_digest, digest_file, is_digest, is_git_object, write_bundle};
use crate::executor::recipe::{resolve_publications, SelectedPublication};
use crate::model::{AttachedComponent, PublisherKind, TagPhase};
use crate::release::git::GitCommand;
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
    /// Normal consumer retrieval from a destination that admits no anonymous read.
    ///
    /// A destination whose ordinary client path requires a credential — GitHub
    /// Package Registry is the maintained one — cannot be retrieved publicly at
    /// any point, before or after closure. Recording that retrieval as `public`
    /// would be an untrue claim in affirmative evidence, and omitting it would
    /// leave the destination with no consumer check at all, so it is named for
    /// what it is: the destination's normal client path, driven with the
    /// credential that path always requires.
    AuthenticatedRegistry,
}

impl CleanClientMode {
    /// Wire spelling of this mode, as every schema and diagnostic names it.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::AuthenticatedDraft => "authenticated-draft",
            Self::AuthenticatedRegistry => "authenticated-registry",
        }
    }
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

impl IntendedDestination {
    /// Stable publication identity this destination intends.
    pub fn identity(&self) -> String {
        format!("{}/{}/{}", self.release_unit, self.publisher, self.target)
    }
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
    #[serde(
        default,
        deserialize_with = "omitted_or_valued",
        skip_serializing_if = "Option::is_none"
    )]
    pub intended_destinations: Option<Vec<IntendedDestination>>,
    /// Publisher evidence sealed by an after-publication tag.
    #[serde(
        default,
        deserialize_with = "omitted_or_valued",
        skip_serializing_if = "Option::is_none"
    )]
    pub publisher_evidence: Option<Vec<PublisherEvidence>>,
}

/// Read a phase member that is either omitted or carries a value.
///
/// The published schema forbids one member per phase with `not: required`, which
/// rejects a present key whatever value it holds. Plain `Option` erases that
/// distinction, so a producer emitting `publisher-evidence: null` on a
/// before-publication document would satisfy every consumer check by omission
/// while failing its own schema. Reading an explicit null as a present member
/// keeps the two readings identical.
fn omitted_or_valued<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)?
        .map(Some)
        .ok_or_else(|| {
            serde::de::Error::custom(
                "a phase member is present with an explicit null; omit the member its phase forbids",
            )
        })
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

    require_proved_working_tree(request.root, &mut findings);
    let config = Config::load(request.root)?;
    let selection = resolve_publications(request.root, &config)?;
    findings.extend(selection.diagnostics);
    let expected: BTreeSet<String> = selection
        .selected
        .iter()
        .map(|publication| publication.identity())
        .collect();

    let proved = match proved_release(request.root) {
        Ok(proved) => Some(proved),
        Err(error) => {
            findings.push(format!(
                "the assembling checkout is not a proved release commit: {error}"
            ));
            None
        }
    };
    if let Some(proved) = &proved {
        reconcile_publications(&proved.plan, &selection.selected, &mut findings);
    }
    let (release, sealed) = match proved {
        Some(proved) => (Some(proved.identity), Some(proved.plan)),
        None => (None, None),
    };

    let scan = scan_input(request.input, &mut findings)?;
    let release_units = accept_publisher_evidence(&scan.publishers, &expected, &mut findings);
    bind_publisher_evidence(&scan, release.as_ref(), &mut findings);
    if let Some(plan) = &sealed {
        bind_subject_versions(&scan, plan, &mut findings);
    }
    compare_phase_evidence(
        &scan,
        release.as_ref(),
        &expected,
        &accepted_by_identity(&release_units),
        &mut findings,
    );
    require_declared_phases(&config, &scan, &mut findings);
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
    findings.extend(subject_disagreements(fragments));
    accepted
}

/// Report any subject that two destinations describe differently.
///
/// A distinct subject is built once and promoted, so two destinations that
/// resolve one subject must resolve the same immutable bytes. Two fragments
/// naming one subject at different versions or digests describe a rebuild,
/// which the protocol does not permit and evidence must not record.
fn subject_disagreements(fragments: &[(PathBuf, PublisherEvidence)]) -> Vec<String> {
    let mut built: BTreeMap<(String, String), (&Path, &Subject)> = BTreeMap::new();
    let mut findings = Vec::new();
    for (path, fragment) in fragments {
        let key = (
            fragment.release_unit.clone(),
            fragment.subject.identity.clone(),
        );
        match built.get(&key) {
            None => {
                built.insert(key, (path, &fragment.subject));
            }
            Some((first, subject))
                if subject.digest != fragment.subject.digest
                    || subject.version != fragment.subject.version =>
            {
                findings.push(format!(
                    "{} and {} record subject {} as {}@{} and {}@{}; one subject is built once and promoted",
                    first.display(),
                    path.display(),
                    fragment.subject.identity,
                    subject.version,
                    subject.digest,
                    fragment.subject.version,
                    fragment.subject.digest
                ));
            }
            Some(_) => {}
        }
    }
    findings
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

/// Report every mechanical problem in one phase-tag evidence document.
///
/// The published schema pairs a phase with the claim it must carry and the
/// claim it must not, and the serde path cannot express that pairing because
/// both claim fields are optional. Without this check an after-publication
/// document that seals nothing, or a before-publication document that intends
/// nothing, would pass every later comparison by omission.
fn phase_findings(phase: &PhaseTagEvidence, label: &str) -> Vec<String> {
    let mut findings = Vec::new();
    let (required, present, forbidden, absent) = match phase.phase {
        TagPhase::BeforePublication => (
            "intended-destinations",
            phase.intended_destinations.is_some(),
            "publisher-evidence",
            phase.publisher_evidence.is_none(),
        ),
        TagPhase::AfterPublication => (
            "publisher-evidence",
            phase.publisher_evidence.is_some(),
            "intended-destinations",
            phase.intended_destinations.is_none(),
        ),
    };
    if !present {
        findings.push(format!(
            "{label} declares {} without {required}",
            phase.phase
        ));
    }
    if !absent {
        findings.push(format!(
            "{label} declares {} but carries {forbidden}",
            phase.phase
        ));
    }
    for (field, value) in [
        ("source-commit", &phase.source_commit),
        ("release-commit", &phase.release_commit),
    ] {
        if !is_git_object(value) {
            findings.push(format!(
                "{label} records {field} {value:?}, which is not a complete Git object identifier"
            ));
        }
    }
    if !is_digest(&phase.plan_digest) {
        findings.push(format!(
            "{label} records plan-digest {:?}, which is not a sha256 digest",
            phase.plan_digest
        ));
    }
    if phase.global_tag.is_empty() {
        findings.push(format!("{label} records an empty global tag name"));
    }
    findings
}

/// Refuse a working tree that is not the one reproduction proved.
///
/// Reproduction proves the *committed* tree at the release commit, and every
/// other read assembly performs — the configuration, and each manifest the
/// publication selection consults — opens a file on disk. Those are two
/// different trees whenever the checkout is dirty, and nothing about the proof
/// notices: a checkout sitting exactly on R, carrying the genuine annotated
/// tag, reproducing cleanly, can still hand assembly a rewritten
/// `.intentional/config.yml` and shift the expected publication set to
/// anything at all.
///
/// Both halves of a dirty tree are refused, because both replace a proved
/// read. A modified or deleted tracked path substitutes a file the release did
/// not carry at that path; an untracked file supplies one the release did not
/// carry at all, which is what turns an unresolvable release unit into a
/// selected publication. Ignored files are not refused, because nothing
/// assembly reads can be ignored and still be part of the release.
///
/// Refusing is the whole repair rather than half of one: with the working tree
/// held to R, the configuration assembly reads *is* the configuration at the
/// proved release commit, which is what the design and the command contract
/// both state.
fn require_proved_working_tree(root: &Path, findings: &mut Vec<String>) {
    let status = match GitCommand::new(root)
        .args(["status", "--porcelain", "--untracked-files=all"])
        .run()
    {
        Ok(status) => status,
        Err(error) => {
            findings.push(format!(
                "the assembling checkout cannot be compared with the release it sits on: {error}"
            ));
            return;
        }
    };
    let text = match status.text() {
        Ok(text) => text,
        Err(error) => {
            findings.push(format!(
                "the assembling checkout cannot be compared with the release it sits on: {error}"
            ));
            return;
        }
    };
    let dirty: Vec<&str> = text
        .lines()
        .filter_map(|line| line.get(3..))
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .collect();
    if !dirty.is_empty() {
        findings.push(format!(
            "the assembling working tree is not the released tree; reproduction proves the commit, and assembly reads these paths from disk: {}",
            dirty.join(", ")
        ));
    }
}

/// Prove the configured publications describe the release the plan sealed.
///
/// The plan is rebuilt from the configuration at the accepted source commit,
/// not at the release commit, and the two are bound rather than identical.
/// Reproduction proves the rebuilt release tree is the released tree, so the
/// configuration this assembly reads at R is the deterministic product of the
/// configuration the plan was rebuilt from at S; tag verification separately
/// binds the tag record's interpretation contract to the configuration at R.
/// Agreement on the contract and on which release units exist therefore holds
/// by construction, and checking either would be a comparison no input could
/// fail. What the plan does decide independently is which release units this
/// release actually versions. A release unit with publishers and no version
/// bump is not in the plan, and a publication configured for it publishes
/// nothing this release sealed.
fn reconcile_publications(
    plan: &crate::plan::ReleasePlan,
    selected: &[SelectedPublication],
    findings: &mut Vec<String>,
) {
    let released: BTreeSet<&str> = plan
        .release_units
        .iter()
        .map(|release_unit| release_unit.id.as_str())
        .collect();
    for publication in selected {
        if !released.contains(publication.release_unit.as_str()) {
            findings.push(format!(
                "the configuration selects publication {}, whose release unit {} the sealed \
                 release plan does not release",
                publication.identity(),
                publication.release_unit
            ));
        }
    }
}

/// Require every published subject to carry the version the plan assigned it.
///
/// The build job derives a subject's version by reproducing the plan from the
/// accepted source commit, so a fragment recording another version describes a
/// build of some other release. Assembly holds the sealed plan, so it says so
/// against the plan rather than trusting that the reproduction happened.
fn bind_subject_versions(
    scan: &ScannedInput,
    plan: &crate::plan::ReleasePlan,
    findings: &mut Vec<String>,
) {
    for (path, fragment) in &scan.publishers {
        let Some(release_unit) = plan
            .release_units
            .iter()
            .find(|release_unit| release_unit.id == fragment.release_unit)
        else {
            // Such a fragment is already reported: as an unexpected
            // publication if the configuration does not select it, and against
            // the sealed plan if it does. Saying it a third time here would
            // tell a reader nothing new.
            continue;
        };
        if fragment.subject.version != release_unit.new_version {
            findings.push(format!(
                "{} records subject {} at version {}, and the sealed release plan assigns \
                 release unit {} version {}",
                path.display(),
                fragment.subject.identity,
                fragment.subject.version,
                fragment.release_unit,
                release_unit.new_version
            ));
        }
    }
}

/// Require every publisher fragment to identify the proved release.
///
/// The release identity comes from the checkout this run proved by reproducing
/// the release from its accepted source commit, not from the fragments, so a
/// fragment is measured against an identity the run derived rather than against
/// one any fragment produced. Comparing the fragments only with each other would let them
/// agree unanimously about a release that was never the one being closed, and
/// would leave a release with no configured publications unable to name itself
/// at all.
fn bind_publisher_evidence(
    scan: &ScannedInput,
    release: Option<&ReleaseIdentity>,
    findings: &mut Vec<String>,
) {
    let Some(release) = release else {
        return;
    };
    for (path, fragment) in &scan.publishers {
        findings.extend(fragment_disagreements(
            &path.display().to_string(),
            release,
            &ReleaseIdentity {
                source_commit: fragment.source_commit.clone(),
                release_commit: fragment.release_commit.clone(),
                global_tag: fragment.global_tag.clone(),
                plan_digest: fragment.plan_digest.clone(),
            },
        ));
    }
}

/// Reject phase-tag evidence that disagrees with the accepted release identity.
fn accepted_by_identity(
    release_units: &BTreeMap<String, ReleaseUnitEvidence>,
) -> BTreeMap<String, &PublisherEvidence> {
    release_units
        .values()
        .flat_map(|unit| unit.publishers.values())
        .flat_map(|publisher| publisher.targets.values())
        .map(|fragment| (fragment.identity(), fragment))
        .collect()
}

/// Require the sealed phase evidence the configuration declares to be present.
///
/// Every phase comparison assembly performs iterates the phase documents it
/// was given, so a run that supplies none agrees with everything. A release
/// that declares phased tags therefore has to hand assembly what those tags
/// sealed, or the check that binds a publisher fragment to its release passes
/// by omission rather than by agreement.
fn require_declared_phases(config: &Config, scan: &ScannedInput, findings: &mut Vec<String>) {
    // Phase evidence is executor-scoped. Without exactly one unphased global
    // release tag no phase seals anything at all, so requiring a document here
    // would refuse a release the protocol never asked to phase.
    if config.unphased_tags().len() != 1 {
        return;
    }
    for phase in config.declared_phases() {
        if !scan.phases.iter().any(|(_, sealed)| sealed.phase == phase) {
            findings.push(format!(
                "the configuration declares {phase} release tags but no sealed {phase} evidence reached assembly"
            ));
        }
    }
}

fn compare_phase_evidence(
    scan: &ScannedInput,
    release: Option<&ReleaseIdentity>,
    expected: &BTreeSet<String>,
    accepted: &BTreeMap<String, &PublisherEvidence>,
    findings: &mut Vec<String>,
) {
    for (path, phase) in &scan.phases {
        findings.extend(phase_findings(phase, &path.display().to_string()));
    }
    let Some(release) = release else {
        return;
    };
    for (path, phase) in &scan.phases {
        let label = path.display();
        let disagreements = phase_disagreements(
            &label.to_string(),
            release,
            &phase.source_commit,
            &phase.release_commit,
            &phase.global_tag,
            &phase.plan_digest,
        );
        if !disagreements.is_empty() {
            findings.extend(disagreements);
            continue;
        }
        // A phase tag seals what the release was committed to before or after
        // publication, so assembly compares those sealed claims with what it
        // expects and with what actually shipped.
        if let Some(intended) = &phase.intended_destinations {
            let sealed = intended
                .iter()
                .map(IntendedDestination::identity)
                .collect::<BTreeSet<_>>();
            for identity in sealed.difference(expected) {
                findings.push(format!(
                    "{label} intends publication {identity}, which the configuration does not select"
                ));
            }
            for identity in expected.difference(&sealed) {
                findings.push(format!(
                    "{label} does not intend configured publication {identity}"
                ));
            }
        }
        if let Some(sealed) = &phase.publisher_evidence {
            for fragment in sealed {
                let identity = fragment.identity();
                match accepted.get(&identity) {
                    None => findings.push(format!(
                        "{label} seals publisher evidence {identity} that assembly did not accept"
                    )),
                    Some(current) if *current != fragment => findings.push(format!(
                        "{label} seals publisher evidence {identity} that differs from the accepted fragment"
                    )),
                    Some(_) => {}
                }
            }
        }
        // A fragment whose subject identity no sealed subject of its release
        // unit names is compared against nothing by the loop below. That is the
        // same silence as supplying no phase evidence at all, so it is reported
        // here rather than left to look like agreement.
        for (fragment_path, fragment) in &scan.publishers {
            let sealed_for_unit = phase
                .subjects
                .iter()
                .filter(|subject| subject.release_unit == fragment.release_unit)
                .collect::<Vec<_>>();
            if sealed_for_unit.is_empty() {
                continue;
            }
            if !sealed_for_unit
                .iter()
                .any(|subject| subject.identity == fragment.subject.identity)
            {
                findings.push(format!(
                    "{} records subject {} for release unit {}, which {label} did not seal",
                    fragment_path.display(),
                    fragment.subject.identity,
                    fragment.release_unit
                ));
            }
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

    /// Published schemas that must contribute a mode enumeration.
    ///
    /// The guard reads every schema in the directory rather than this list.
    /// These two are named so that a rename or a deletion that removes the only
    /// enumeration fails here, instead of leaving a glob that reads nothing and
    /// reports clean over it.
    const MODE_SCHEMAS: [&str; 2] = [
        "publisher-evidence.json-schema.yml",
        "publication-observation.json-schema.yml",
    ];

    // The mode set is written down in four places: this enum, the schema of the
    // document a recipe writes, the schema of the fragment verification derives
    // from it, and the live check that decides which modes a published release
    // may still be retrieved by. Widening the enum alone is silent -- nothing
    // deserializes these schemas at runtime -- so a publication would write a
    // fragment that fails its own published schema and only a consumer
    // validating the release evidence would ever find out. Reading the schemas
    // here is what makes the enum and the documents one decision.
    #[test]
    fn every_published_schema_enumerates_the_retrieval_modes_this_enum_admits() {
        let admitted = [
            CleanClientMode::Public,
            CleanClientMode::AuthenticatedDraft,
            CleanClientMode::AuthenticatedRegistry,
        ]
        .into_iter()
        .map(|mode| {
            serde_yaml::to_string(&mode)
                .expect("a mode serializes")
                .trim()
                .to_owned()
        })
        .collect::<BTreeSet<_>>();

        let directory =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/specifications");
        let mut contributed = BTreeSet::new();
        let mut schemas = std::fs::read_dir(&directory)
            .expect("the specification directory is readable")
            .filter_map(|entry| {
                let path = entry.expect("directory entry").path();
                path.to_str()?.ends_with(".json-schema.yml").then_some(path)
            })
            .collect::<Vec<_>>();
        schemas.sort();
        assert!(
            !schemas.is_empty(),
            "published schemas live in {directory:?}"
        );

        for path in schemas {
            let name = path
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .expect("a schema file name")
                .to_owned();
            let document: serde_yaml::Value = serde_yaml::from_str(
                &std::fs::read_to_string(&path).expect("the published schema is readable"),
            )
            .expect("the published schema parses");
            for (pointer, values) in mode_enumerations(&document) {
                // `mode` is not a reserved word: a projection has one too, and
                // it enumerates something else entirely. An enumeration is this
                // enumeration when it names any of these modes, which is what a
                // schema enumerating the set would do however it nests it, and
                // is why the check is an intersection rather than a fixed path.
                if values.is_disjoint(&admitted) {
                    continue;
                }
                contributed.insert(name.clone());
                assert_eq!(
                    values, admitted,
                    "{name} at {pointer} enumerates the modes this crate admits"
                );
            }
        }

        for name in MODE_SCHEMAS {
            assert!(
                contributed.contains(name),
                "{name} still enumerates the retrieval modes; contributing schemas are {contributed:?}"
            );
        }
    }

    /// Every `mode` enumeration one schema document declares, by rough location.
    ///
    /// The search is by property name rather than by a fixed path so a schema
    /// that grows a second `mode` enumeration is read too, instead of passing
    /// because the one path this knew about still agreed.
    fn mode_enumerations(document: &serde_yaml::Value) -> Vec<(String, BTreeSet<String>)> {
        fn walk(
            value: &serde_yaml::Value,
            path: &str,
            found: &mut Vec<(String, BTreeSet<String>)>,
        ) {
            let Some(mapping) = value.as_mapping() else {
                if let Some(sequence) = value.as_sequence() {
                    for (index, item) in sequence.iter().enumerate() {
                        walk(item, &format!("{path}/{index}"), found);
                    }
                }
                return;
            };
            for (key, child) in mapping {
                let key = key.as_str().unwrap_or_default();
                let child_path = format!("{path}/{key}");
                if key == "mode" {
                    if let Some(values) = child.get("enum").and_then(serde_yaml::Value::as_sequence)
                    {
                        found.push((
                            child_path.clone(),
                            values
                                .iter()
                                .filter_map(|value| value.as_str().map(str::to_owned))
                                .collect(),
                        ));
                    }
                }
                walk(child, &child_path, found);
            }
        }
        let mut found = Vec::new();
        walk(document, "", &mut found);
        found
    }

    use crate::release::tag::tests::ReleasedWorkspace;

    const SUBJECT_DIGEST: &str =
        "sha256:5555555555555555555555555555555555555555555555555555555555555555";

    /// The version the fixture release publishes, one minor bump from baseline.
    const VERSION: &str = "1.1.0";

    /// A repository that releases one component and publishes it to npm.
    ///
    /// The primary tag carries no required phase, so it is the single global
    /// release tag the executor protocol requires, and the release this fixture
    /// performs is the one assembly proves its checkout against.
    const CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-1
github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml }
workspace-tags:
  release:
    template: 'release/{version}'
release-units:
  component:
    path: component
    npm: {}
    projections:
      - adapter: json
        file: package.json
        pointer: /version
        mode: committed
    tags:
      primary: { role: primary, template: '{id}@{version}', require-phase: before-publication }
"#;

    /// The same repository with every publisher removed.
    ///
    /// Only the publisher block differs from `CONFIG`, so a release that
    /// assembles under this configuration and not under that one differs by
    /// publication and by nothing else.
    const UNPUBLISHED_CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-1
github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml }
workspace-tags:
  release:
    template: 'release/{version}'
release-units:
  component:
    path: component
    projections:
      - adapter: json
        file: package.json
        pointer: /version
        mode: committed
    tags:
      primary: { role: primary, template: '{id}@{version}', require-phase: before-publication }
"#;

    /// One genuinely released checkout for assembly to prove.
    ///
    /// Assembly derives its identity by reproducing the release from the
    /// accepted source commit and refusing a checkout the reproduction does not
    /// match, so a fixture that assembled a workspace out of parts would be
    /// testing against a release the repository never carried.
    fn workspace() -> ReleasedWorkspace {
        ReleasedWorkspace::with(
            CONFIG,
            &[(
                "component/package.json",
                "{\n  \"name\": \"example-component\",\n  \"version\": \"1.0.0\"\n}\n",
            )],
            "component",
        )
    }

    fn workflow(workspace: &ReleasedWorkspace) -> WorkflowIdentity {
        WorkflowIdentity {
            repository: "example-owner/example-repository".to_owned(),
            workflow: "publish".to_owned(),
            run_id: 42,
            run_attempt: 1,
            commit: workspace.release.clone(),
        }
    }

    /// One publisher fragment carrying the identities this release actually has.
    fn fragment(
        workspace: &ReleasedWorkspace,
        release_unit: &str,
        publisher: &str,
        target: &str,
    ) -> String {
        let source = &workspace.source;
        let release = &workspace.release;
        let tag_name = &workspace.tag_name;
        let tag_object = &workspace.tag_object;
        let plan_digest = &workspace.plan_digest;
        format!(
            r#"$schema: {PUBLISHER_EVIDENCE_SCHEMA}
contract: {PUBLISHER_EVIDENCE_CONTRACT}
release-unit: {release_unit}
publisher: {publisher}
target: {target}
source-commit: {source}
release-commit: {release}
global-tag:
  name: {tag_name}
  object: {tag_object}
  target: {release}
plan-digest: {plan_digest}
subject:
  kind: npm-package
  identity: example-component
  version: {VERSION}
  digest: {SUBJECT_DIGEST}
packager:
  id: npm
  version: 10.8.2
build-provenance: []
attached-metadata: []
destination:
  identity: registry.example.test/example-component
  version: {VERSION}
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
        workspace: &ReleasedWorkspace,
        input: &Path,
        namespace: &str,
        job: &str,
        run_attempt: u32,
        value: Option<&str>,
        attachments: &[(&str, &str)],
    ) -> PathBuf {
        let staging = workspace
            .scratch()
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

    /// One assembly request against a released checkout.
    ///
    /// The request carries no identity of its own and this helper stages
    /// nothing: every test binds to the release its own checkout carries, which
    /// assembly proves by reproducing it from the accepted source commit.
    fn request<'a>(
        workspace: &'a ReleasedWorkspace,
        input: &'a Path,
        output: &'a Path,
    ) -> AssembleRequest<'a> {
        AssembleRequest {
            root: &workspace.root,
            input,
            output,
            workflow: workflow(workspace),
        }
    }

    #[test]
    fn assembles_one_deterministic_bundle() {
        let workspace = workspace();
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(input.join("publisher")).expect("publisher artifact");
        stage_phase(&workspace, &input);
        std::fs::write(
            input.join("publisher/publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
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

        let output = workspace.scratch().join("release-evidence");
        let assembly = assemble(&request(&workspace, &input, &output)).expect("assembly succeeds");
        assert_eq!(
            assembly.evidence.release,
            ReleaseIdentity {
                source_commit: workspace.source.clone(),
                release_commit: workspace.release.clone(),
                global_tag: TagIdentity {
                    name: workspace.tag_name.clone(),
                    object: workspace.tag_object.clone(),
                    target: workspace.release.clone(),
                },
                plan_digest: workspace.plan_digest.clone(),
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

        let repeat = workspace.scratch().join("release-evidence-repeat");
        let second = assemble(&request(&workspace, &input, &repeat)).expect("assembly repeats");
        assert_eq!(
            std::fs::read_to_string(&assembly.evidence_path).expect("first"),
            std::fs::read_to_string(&second.evidence_path).expect("second"),
            "assembly is deterministic"
        );
    }

    #[test]
    fn preserves_arbitrary_contributor_yaml_without_interpretation() {
        let workspace = workspace();
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        stage_phase(&workspace, &input);
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
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
        let output = workspace.scratch().join("release-evidence");
        let assembly = assemble(&request(&workspace, &input, &output)).expect("assembly succeeds");
        assert_eq!(
            assembly.evidence.contributions["producer-defined"],
            serde_yaml::from_str::<serde_yaml::Value>(contributed).expect("value")
        );
    }

    #[test]
    fn selects_the_highest_attempt_of_a_retried_job() {
        let workspace = workspace();
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        stage_phase(&workspace, &input);
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
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

        let output = workspace.scratch().join("release-evidence");
        let assembly = assemble(&request(&workspace, &input, &output)).expect("assembly succeeds");
        assert_eq!(
            assembly.evidence.contributions["assessment"],
            serde_yaml::from_str::<serde_yaml::Value>("attempt: 2\n").expect("value")
        );
    }

    #[test]
    fn rejects_a_namespace_claimed_by_two_jobs() {
        let workspace = workspace();
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
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
        let output = workspace.scratch().join("release-evidence");
        let error =
            assemble(&request(&workspace, &input, &output)).expect_err("collision rejected");
        assert!(error.to_string().contains("is claimed by jobs"), "{error}");
        assert!(!output.exists(), "a rejected assembly writes nothing");
    }

    #[test]
    fn rejects_a_release_asset_name_collision() {
        let workspace = workspace();
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
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
        let output = workspace.scratch().join("release-evidence");
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
        let workspace = workspace();
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
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
        let output = workspace.scratch().join("release-evidence");
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
        let workspace = workspace();
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
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
        std::fs::write(workspace.root.join("secret.txt"), "secret").expect("secret");
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
                .root
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
        let workspace = workspace();
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("unexpected.yml"),
            fragment(&workspace, "component", "cargo", "primary"),
        )
        .expect("fragment");
        let output = workspace.scratch().join("release-evidence");
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
        let workspace = workspace();
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(input.join("first")).expect("first");
        std::fs::create_dir_all(input.join("second")).expect("second");
        std::fs::write(
            input.join("first/publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
        )
        .expect("fragment");
        std::fs::write(
            input.join("second/publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
        )
        .expect("fragment");
        let output = workspace.scratch().join("release-evidence");
        let error =
            assemble(&request(&workspace, &input, &output)).expect_err("duplicate rejected");
        assert!(
            error.to_string().contains("was supplied more than once"),
            "{error}"
        );
    }

    #[test]
    fn rejects_two_destinations_that_describe_one_subject_differently() {
        let workspace = workspace();
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("primary.yml"),
            fragment(&workspace, "component", "npm", "primary"),
        )
        .expect("fragment");
        // The same subject promoted to a second destination cannot carry a
        // different digest; a second digest is a rebuild rather than a
        // promotion.
        std::fs::write(
            input.join("github.yml"),
            fragment(&workspace, "component", "npm", "github").replace(
                SUBJECT_DIGEST,
                "sha256:1111111111111111111111111111111111111111111111111111111111111111",
            ),
        )
        .expect("fragment");
        let output = workspace.scratch().join("release-evidence");
        let error =
            assemble(&request(&workspace, &input, &output)).expect_err("a rebuild is rejected");
        assert!(
            error
                .to_string()
                .contains("one subject is built once and promoted"),
            "{error}"
        );
        assert!(!output.exists(), "a rejected assembly writes nothing");
    }

    #[test]
    fn rejects_phase_tag_evidence_that_disagrees_with_publisher_evidence() {
        let workspace = workspace();
        let source = &workspace.source;
        let release = &workspace.release;
        let tag_name = &workspace.tag_name;
        let plan_digest = &workspace.plan_digest;
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
        )
        .expect("fragment");
        std::fs::write(
            input.join("phase-tag-evidence.yml"),
            format!(
                r#"$schema: {PHASE_TAG_EVIDENCE_SCHEMA}
phase: before-publication
source-commit: {source}
release-commit: {release}
global-tag: {tag_name}
plan-digest: {plan_digest}
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
        let output = workspace.scratch().join("release-evidence");
        let error =
            assemble(&request(&workspace, &input, &output)).expect_err("disagreement rejected");
        assert!(
            error.to_string().contains("while") && error.to_string().contains("sealed"),
            "{error}"
        );
    }

    /// Configuration that declares a before-publication tag as well as the global one.
    /// Two publishing release units, one of which this release does not bump.
    const UNRELEASED_UNIT_CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-1
github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml }
workspace-tags:
  release:
    template: 'release/{version}'
release-units:
  component:
    path: component
    npm: {}
    projections:
      - adapter: json
        file: package.json
        pointer: /version
        mode: committed
    tags:
      primary: { role: primary, template: '{id}@{version}', require-phase: after-publication }
  spare:
    path: spare
    npm: {}
    projections:
      - adapter: json
        file: package.json
        pointer: /version
        mode: committed
    tags:
      primary: { role: primary, template: '{id}@{version}', require-phase: after-publication }
"#;

    // Every phase comparison assembly performs iterates the documents it was
    // given, so a run that supplies none agrees with everything. A release that
    // declares phased tags has to hand assembly what they sealed.
    #[test]
    fn refuses_assembly_when_a_declared_phase_sealed_nothing() {
        let workspace = workspace();
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
        )
        .expect("fragment");

        let output = workspace.scratch().join("release-evidence");
        let error = assemble(&request(&workspace, &input, &output))
            .expect_err("a declared phase that sealed nothing is refused");
        assert!(
            error.to_string().contains(
                "declares before-publication release tags but no sealed before-publication evidence"
            ),
            "{error}"
        );

        // The same run passes once the phase it declares actually reaches it.
        std::fs::write(
            input.join("before.yml"),
            before_publication_evidence(
                &workspace,
                VERSION,
                "  - release-unit: component\n    publisher: npm\n    target: primary\n",
            ),
        )
        .expect("phase evidence");
        assemble(&request(&workspace, &input, &output)).expect("the sealed phase completes it");
    }

    // A fragment whose subject identity the phase never sealed is compared
    // against nothing, which reads exactly like agreement. It is the silent
    // form of supplying no phase evidence at all.
    #[test]
    fn refuses_a_fragment_naming_a_subject_the_phase_did_not_seal() {
        let workspace = workspace();
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
        )
        .expect("fragment");
        std::fs::write(
            input.join("before.yml"),
            before_publication_evidence(
                &workspace,
                VERSION,
                "  - release-unit: component\n    publisher: npm\n    target: primary\n",
            )
            .replace("identity: example-component", "identity: other-component"),
        )
        .expect("phase evidence");

        let output = workspace.scratch().join("release-evidence");
        let error = assemble(&request(&workspace, &input, &output))
            .expect_err("an unsealed subject identity is refused");
        assert!(
            error
                .to_string()
                .contains("records subject example-component for release unit component"),
            "{error}"
        );
    }

    /// Stage the sealed before-publication evidence this configuration declares.
    ///
    /// Every release-unit tag carries a required phase, because the executor
    /// admits exactly one unphased tag and that one is the workspace-level
    /// global release tag. A declared phase whose sealed evidence never reaches
    /// assembly is a refusal, so a run that expects to assemble has to hand
    /// assembly what the phase tag sealed.
    fn stage_phase(workspace: &ReleasedWorkspace, input: &Path) {
        stage_intending(
            workspace,
            input,
            "  - release-unit: component\n    publisher: npm\n    target: primary\n",
        );
    }

    /// The same, for a release whose configuration selects the given set.
    fn stage_intending(workspace: &ReleasedWorkspace, input: &Path, destinations: &str) {
        std::fs::write(
            input.join("phase-tag-evidence.yml"),
            before_publication_evidence(workspace, VERSION, destinations),
        )
        .expect("sealed phase evidence");
    }

    fn before_publication_evidence(
        workspace: &ReleasedWorkspace,
        version: &str,
        destinations: &str,
    ) -> String {
        let source = &workspace.source;
        let release = &workspace.release;
        let tag_name = &workspace.tag_name;
        let plan_digest = &workspace.plan_digest;
        format!(
            r#"$schema: {PHASE_TAG_EVIDENCE_SCHEMA}
phase: before-publication
source-commit: {source}
release-commit: {release}
global-tag: {tag_name}
plan-digest: {plan_digest}
subjects:
  - release-unit: component
    identity: example-component
    version: {version}
    digest: {SUBJECT_DIGEST}
intended-destinations:
{destinations}"#
        )
    }

    #[test]
    fn accepts_phase_tag_evidence_that_agrees_with_the_accepted_fragments() {
        let workspace = workspace();
        let source = &workspace.source;
        let release = &workspace.release;
        let tag_name = &workspace.tag_name;
        let plan_digest = &workspace.plan_digest;
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
        )
        .expect("fragment");
        std::fs::write(
            input.join("before.yml"),
            before_publication_evidence(
                &workspace,
                VERSION,
                "  - release-unit: component\n    publisher: npm\n    target: primary\n",
            ),
        )
        .expect("before evidence");
        std::fs::write(
            input.join("after.yml"),
            format!(
                r#"$schema: {PHASE_TAG_EVIDENCE_SCHEMA}
phase: after-publication
source-commit: {source}
release-commit: {release}
global-tag: {tag_name}
plan-digest: {plan_digest}
subjects: []
publisher-evidence:
  - {}
"#,
                fragment(&workspace, "component", "npm", "primary")
                    .trim_end()
                    .replace('\n', "\n    ")
            ),
        )
        .expect("after evidence");
        let output = workspace.scratch().join("release-evidence");
        assemble(&request(&workspace, &input, &output)).expect("agreeing phase evidence assembles");
    }

    #[test]
    fn rejects_a_phase_tag_intent_that_disagrees_with_the_configured_publications() {
        let workspace = workspace();
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
        )
        .expect("fragment");
        std::fs::write(
            input.join("before.yml"),
            before_publication_evidence(
                &workspace,
                VERSION,
                "  - release-unit: component\n    publisher: cargo\n    target: primary\n",
            ),
        )
        .expect("before evidence");
        let output = workspace.scratch().join("release-evidence");
        let error = assemble(&request(&workspace, &input, &output)).expect_err("intent rejected");
        let message = error.to_string();
        assert!(
            message.contains("intends publication component/cargo/primary"),
            "{message}"
        );
        assert!(
            message.contains("does not intend configured publication component/npm/primary"),
            "{message}"
        );
    }

    #[test]
    fn rejects_sealed_publisher_evidence_that_differs_from_what_shipped() {
        let workspace = workspace();
        let source = &workspace.source;
        let release = &workspace.release;
        let tag_name = &workspace.tag_name;
        let plan_digest = &workspace.plan_digest;
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
        )
        .expect("fragment");
        let sealed = fragment(&workspace, "component", "npm", "primary")
            .replace("version: 10.8.2", "version: 10.9.0")
            .trim_end()
            .replace('\n', "\n    ");
        std::fs::write(
            input.join("after.yml"),
            format!(
                r#"$schema: {PHASE_TAG_EVIDENCE_SCHEMA}
phase: after-publication
source-commit: {source}
release-commit: {release}
global-tag: {tag_name}
plan-digest: {plan_digest}
subjects: []
publisher-evidence:
  - {sealed}
"#
            ),
        )
        .expect("after evidence");
        let output = workspace.scratch().join("release-evidence");
        let error = assemble(&request(&workspace, &input, &output)).expect_err("seal rejected");
        assert!(
            error.to_string().contains(
                "seals publisher evidence component/npm/primary that differs from the accepted"
            ),
            "{error}"
        );
    }

    #[test]
    fn rejects_a_phase_tag_that_omits_the_claim_its_phase_requires() {
        for (label, phase, tail, expected) in [
            (
                "after-without-seal",
                "after-publication",
                "",
                "declares after-publication without publisher-evidence",
            ),
            (
                "before-without-intent",
                "before-publication",
                "",
                "declares before-publication without intended-destinations",
            ),
            (
                "before-with-seal",
                "before-publication",
                "intended-destinations:\n  - release-unit: component\n    publisher: npm\n    target: primary\npublisher-evidence: []\n",
                "declares before-publication but carries publisher-evidence",
            ),
            (
                "after-with-intent",
                "after-publication",
                "publisher-evidence: []\nintended-destinations:\n  - release-unit: component\n    publisher: npm\n    target: primary\n",
                "declares after-publication but carries intended-destinations",
            ),
            // The published schema forbids the member itself, so an explicit
            // null is refused exactly like a populated one rather than being
            // read as the omission its phase requires.
            (
                "before-with-null-seal",
                "before-publication",
                "intended-destinations:\n  - release-unit: component\n    publisher: npm\n    target: primary\npublisher-evidence: null\n",
                "a phase member is present with an explicit null",
            ),
            (
                "after-with-null-intent",
                "after-publication",
                "publisher-evidence: []\nintended-destinations: null\n",
                "a phase member is present with an explicit null",
            ),
        ] {
            let workspace = workspace();
            let source = &workspace.source;
            let release = &workspace.release;
            let tag_name = &workspace.tag_name;
            let plan_digest = &workspace.plan_digest;
            let input = workspace.scratch().join("artifacts");
            std::fs::create_dir_all(&input).expect("artifacts");
            std::fs::write(
                input.join("publisher-evidence.yml"),
                fragment(&workspace, "component", "npm", "primary"),
            )
            .expect("fragment");
            std::fs::write(
                input.join("phase.yml"),
                format!(
                    r#"$schema: {PHASE_TAG_EVIDENCE_SCHEMA}
phase: {phase}
source-commit: {source}
release-commit: {release}
global-tag: {tag_name}
plan-digest: {plan_digest}
subjects: []
{tail}"#
                ),
            )
            .expect("phase evidence");
            let output = workspace.scratch().join("release-evidence");
            let error =
                assemble(&request(&workspace, &input, &output)).expect_err("omission rejected");
            assert!(error.to_string().contains(expected), "{label}: {error}");
            assert!(!output.exists(), "{label}: a rejected assembly writes nothing");
        }
    }

    #[test]
    fn reports_phase_tag_evidence_that_does_not_identify_itself() {
        let workspace = workspace();
        let source = &workspace.source;
        let release = &workspace.release;
        let plan_digest = &workspace.plan_digest;
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
        )
        .expect("fragment");
        std::fs::write(
            input.join("phase-tag-evidence.yml"),
            format!(
                r#"phase: before-publication
source-commit: {source}
release-commit: {release}
tag: release/1.0.0
plan-digest: {plan_digest}
subjects: []
"#
            ),
        )
        .expect("phase evidence");
        let output = workspace.scratch().join("release-evidence");
        let error =
            assemble(&request(&workspace, &input, &output)).expect_err("silent drop rejected");
        assert!(
            error
                .to_string()
                .contains("carries phase-tag evidence that does not declare $schema"),
            "{error}"
        );
    }

    /// A repository that configures no publisher at all still releases.
    ///
    /// Publishing is opt-in, so this is the first state of every adopter: a
    /// release commit, a global tag, a GitHub Release, and nothing published.
    /// Such a release still wants a durable evidence statement, and the
    /// statement's identity contract does not weaken to get one.
    #[test]
    fn assembles_a_release_that_configures_no_publications() {
        let workspace = ReleasedWorkspace::with(
            UNPUBLISHED_CONFIG,
            &[(
                "component/package.json",
                "{\n  \"name\": \"example-component\",\n  \"version\": \"1.0.0\"\n}\n",
            )],
            "component",
        );
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        // The release still phases its tags, and a release that publishes
        // nothing seals a phase that intends nothing.
        stage_intending(&workspace, &input, "  []\n");
        let output = workspace.scratch().join("release-evidence");
        let assembly = assemble(&request(&workspace, &input, &output))
            .expect("a publication-less release assembles");
        assert_eq!(
            assembly.evidence.release,
            ReleaseIdentity {
                source_commit: workspace.source.clone(),
                release_commit: workspace.release.clone(),
                global_tag: TagIdentity {
                    name: workspace.tag_name.clone(),
                    object: workspace.tag_object.clone(),
                    target: workspace.release.clone(),
                },
                plan_digest: workspace.plan_digest.clone(),
            },
            "the proved checkout identifies the release when no publisher can"
        );
        assert!(
            assembly.evidence.release_units.is_empty(),
            "a release that publishes nothing records no publisher evidence"
        );
        assert!(assembly.evidence_path.is_file());
    }

    /// A dirty checkout is refused, not silently believed.
    ///
    /// Reproduction proves the committed tree; the configuration and every
    /// manifest the publication selection consults are read from disk. This is
    /// the input that separates them: a checkout sitting on the release commit,
    /// carrying the genuine annotated tag, reproducing cleanly, whose
    /// `.intentional/config.yml` on disk selects no publication for a release
    /// whose committed configuration selects one.
    ///
    /// Without the working-tree check it assembles successfully and writes a
    /// complete statement with an empty `release-units` section — a true
    /// document about a release that did not happen, which is the failure the
    /// affirmative-evidence rule exists to prevent.
    #[test]
    fn refuses_a_working_tree_whose_configuration_the_release_never_carried() {
        let workspace = workspace();
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        stage_intending(&workspace, &input, "  []\n");
        let output = workspace.scratch().join("release-evidence");
        std::fs::write(
            workspace.root.join(".intentional/config.yml"),
            UNPUBLISHED_CONFIG,
        )
        .expect("drifted configuration");

        let error = assemble(&request(&workspace, &input, &output))
            .expect_err("a working tree the release never carried is refused");
        let message = error.to_string();
        assert!(
            message.contains("the assembling working tree is not the released tree"),
            "{message}"
        );
        assert!(
            message.contains(".intentional/config.yml"),
            "the diagnostic names the path that drifted: {message}"
        );
        assert!(
            !output.exists(),
            "no statement is written about a release the checkout does not carry"
        );
    }

    /// A file the release never carried is refused too.
    ///
    /// The other half of a dirty tree. An untracked file does not replace a
    /// proved read; it adds a file to the tree assembly reads, and the
    /// publication selection opens paths the configuration names rather than a
    /// fixed list. Refusing it is what makes "the configuration at the proved
    /// release commit" exact rather than approximate: every path assembly
    /// reads is a path the release commit carries, with the content it carried.
    #[test]
    fn refuses_a_working_tree_carrying_a_file_the_release_never_carried() {
        let workspace = workspace();
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        stage_intending(&workspace, &input, "  []\n");
        let output = workspace.scratch().join("release-evidence");
        std::fs::write(workspace.root.join("component/npm-shrinkwrap.json"), "{}\n")
            .expect("untracked manifest");

        let error = assemble(&request(&workspace, &input, &output))
            .expect_err("a file the release never carried is refused");
        let message = error.to_string();
        assert!(
            message.contains("the assembling working tree is not the released tree"),
            "{message}"
        );
        assert!(
            message.contains("component/npm-shrinkwrap.json"),
            "the diagnostic names the file the release never carried: {message}"
        );
    }

    /// Unanimous fragments are not evidence of the release being closed.
    ///
    /// Every fragment here agrees with every other fragment about a release,
    /// which is exactly what an assembly bound only to its own inputs would
    /// accept. They are refused because they disagree with the release the
    /// checkout was proved to be, and the diagnostic names the component that
    /// disagreed.
    #[test]
    fn refuses_fragments_that_agree_with_each_other_and_not_with_the_proved_release() {
        let workspace = workspace();
        let release = &workspace.release;
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        let elsewhere = "7".repeat(40);
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary").replace(
                &format!("release-commit: {release}"),
                &format!("release-commit: {elsewhere}"),
            ),
        )
        .expect("fragment");
        let output = workspace.scratch().join("release-evidence");
        let error = assemble(&request(&workspace, &input, &output))
            .expect_err("a fragment about another release is refused");
        let message = error.to_string();
        assert!(message.contains("records release-commit"), "{message}");
        assert!(message.contains(&elsewhere), "{message}");
        assert!(message.contains(&workspace.release), "{message}");
        assert!(
            !message.contains("records source-commit"),
            "only the component that disagreed is reported: {message}"
        );
    }

    /// Configuration is a second input, so assembly proves the two agree.
    ///
    /// The expected publication set comes from the assembling job's checkout.
    /// A checkout that selects a publication for a release unit the sealed plan
    /// does not release is a checkout that is not describing this release.
    #[test]
    fn refuses_a_configuration_selecting_a_publication_the_sealed_plan_does_not_release() {
        // Two release units both configure npm and only one is bumped, so the
        // sealed plan releases one of them and the configuration still selects
        // a publication for the other. The global release tag is the workspace
        // tag, because the executor requires exactly one unphased tag.
        let workspace = ReleasedWorkspace::with(
            UNRELEASED_UNIT_CONFIG,
            &[
                (
                    "component/package.json",
                    "{\n  \"name\": \"example-component\",\n  \"version\": \"1.0.0\"\n}\n",
                ),
                (
                    "spare/package.json",
                    "{\n  \"name\": \"example-spare\",\n  \"version\": \"1.0.0\"\n}\n",
                ),
            ],
            "component",
        );
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        let output = workspace.scratch().join("release-evidence");
        let error = assemble(&request(&workspace, &input, &output))
            .expect_err("a publication the plan does not release is refused");
        let message = error.to_string();
        assert!(message.contains("spare/npm/primary"), "{message}");
        assert!(
            message.contains("the sealed release plan does not release"),
            "{message}"
        );
    }

    /// A published subject carries the version its release unit was planned.
    ///
    /// The build job derives the version by reproducing the plan from S, so a
    /// fragment recording another version describes a build that is not this
    /// release's. Assembly holds the sealed plan and can say so directly.
    #[test]
    fn binds_a_published_subject_to_the_version_the_sealed_plan_assigns() {
        let workspace = workspace();
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary")
                .replace(&format!("version: {VERSION}"), "version: 9.9.9"),
        )
        .expect("fragment");
        stage_phase(&workspace, &input);
        let output = workspace.scratch().join("release-evidence");
        let error = assemble(&request(&workspace, &input, &output))
            .expect_err("a subject version the plan does not assign is refused");
        let message = error.to_string();
        assert!(
            message.contains("the sealed release plan assigns release unit component version"),
            "{message}"
        );
        assert!(message.contains("9.9.9"), "{message}");
        assert!(message.contains(VERSION), "{message}");
        assert!(message.contains("example-component"), "{message}");
    }

    /// Phase documents name the component that disagreed, as fragments do.
    #[test]
    fn names_the_identity_component_a_phase_document_spells_differently() {
        let workspace = workspace();
        let release = &workspace.release;
        let tag_name = &workspace.tag_name;
        let plan_digest = &workspace.plan_digest;
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
        )
        .expect("fragment");
        let elsewhere = "7".repeat(40);
        std::fs::write(
            input.join("phase.yml"),
            format!(
                r#"$schema: {PHASE_TAG_EVIDENCE_SCHEMA}
phase: before-publication
source-commit: {elsewhere}
release-commit: {release}
global-tag: {tag_name}
plan-digest: {plan_digest}
subjects: []
intended-destinations:
  - release-unit: component
    publisher: npm
    target: primary
"#
            ),
        )
        .expect("phase evidence");
        let output = workspace.scratch().join("release-evidence");
        let error = assemble(&request(&workspace, &input, &output))
            .expect_err("a phase document about another release is refused");
        let message = error.to_string();
        assert!(message.contains("records source-commit"), "{message}");
        assert!(message.contains(&elsewhere), "{message}");
        assert!(
            !message.contains("records release-commit"),
            "only the component that disagreed is reported: {message}"
        );
    }

    /// A checkout assembly cannot prove is a finding, not an early return.
    ///
    /// The checkout is moved off the released commit, which is the state the
    /// whole binding exists for. A second, unrelated problem is staged with it
    /// so the run has something else to report: an unprovable checkout is a
    /// finding, so both reach one diagnostic.
    #[test]
    fn refuses_a_checkout_it_cannot_prove_and_still_reports_the_rest() {
        let workspace = workspace();
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
        )
        .expect("fragment");
        let output = workspace.scratch().join("release-evidence");
        workspace.checkout(&workspace.source.clone());

        let mut request = request(&workspace, &input, &output);
        request.workflow.run_id = 0;
        let error = assemble(&request).expect_err("an unproved checkout is refused");
        let message = error.to_string();
        assert!(
            message.contains("the assembling checkout is not a proved release commit"),
            "{message}"
        );
        assert!(
            message.contains("run identifier is missing"),
            "every observable problem is reported in one run: {message}"
        );
    }

    #[test]
    fn includes_available_contributions_without_waiting_for_absent_ones() {
        let workspace = workspace();
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        stage_phase(&workspace, &input);
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
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
        let output = workspace.scratch().join("release-evidence");
        let assembly = assemble(&request(&workspace, &input, &output)).expect("assembly succeeds");
        assert_eq!(assembly.evidence.contributions.len(), 1);
        assert!(assembly.evidence.contributions.contains_key("assessment"));
    }

    #[test]
    fn refuses_to_assemble_into_a_populated_directory() {
        let workspace = workspace();
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        stage_phase(&workspace, &input);
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
        )
        .expect("fragment");
        let output = workspace.scratch().join("release-evidence");
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
        let workspace = workspace();
        let input = workspace.scratch().join("artifacts");
        std::fs::create_dir_all(&input).expect("artifacts");
        std::fs::write(
            input.join("publisher-evidence.yml"),
            fragment(&workspace, "component", "npm", "primary"),
        )
        .expect("fragment");
        let output = workspace.scratch().join("release-evidence");
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
