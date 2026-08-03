// ---
// relationships:
//   implements: github-release-executor
// ---

//! Verification of a completed Intentional GitHub Release and its durable evidence.
//!
//! Intentional commands never write a GitHub Release and never hold publishing
//! credentials, so this module reads one through an injectable source seam
//! rather than embedding an HTTP client. The seam exposes no write operation of
//! any kind, which is what makes the specification's rule — live verification
//! reports and never rewrites the immutable Release — a property of the code
//! rather than a discipline reviewers have to enforce.
//!
//! The default source drives the `gh` command line, the repository's
//! established GitHub read path, so verification runs under the operator's own
//! credentials and never asks for new ones.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::evidence::assemble::{
    CleanClient, CleanClientMode, Destination, PhaseTagEvidence, PublisherEvidence,
    ReleaseEvidence, PHASE_TAG_EVIDENCE_SCHEMA, RELEASE_EVIDENCE_CONTRACT, RELEASE_EVIDENCE_FILE,
    RELEASE_EVIDENCE_SCHEMA,
};
use crate::evidence::phase::{decode, PHASE_EVIDENCE_FIELD};
use crate::evidence::{digest_bytes, is_digest, is_git_object};
use crate::model::TagPhase;
use crate::publication::draft::is_draft_dependent;
use crate::publication::observation::{
    ObservationState, PublicationObservation, PUBLICATION_OBSERVATION_CONTRACT,
    PUBLICATION_OBSERVATION_SCHEMA,
};
use crate::release::git::GitCommand;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Stdio};

/// One asset held by a GitHub Release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseAsset {
    /// Stable GitHub Release asset identifier.
    pub id: u64,
    /// Flat GitHub Release asset name.
    pub name: String,
    /// Asset size in bytes as GitHub reports it.
    pub size: u64,
    /// Asset media type as GitHub reports it.
    pub media_type: String,
}

/// One GitHub Release resolved by tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseRecord {
    /// Stable GitHub Release identifier.
    pub id: u64,
    /// Tag the Release is bound to.
    pub tag: String,
    /// Whether the Release is still a draft.
    pub draft: bool,
}

/// One GitHub artifact attestation resolved for a Release asset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attestation {
    /// Digest the attestation binds as its subject.
    pub subject_digest: String,
    /// Repository the attested workflow ran in.
    pub repository: String,
    /// Workflow that produced the attested subject.
    pub workflow: String,
    /// Workflow run that produced the attested subject.
    pub run_id: u64,
}

/// Read-only access to one repository's GitHub Releases.
///
/// The seam carries no write operation, so no verification path can create,
/// replace, or append to a Release or one of its assets even by mistake.
pub trait ReleaseSource {
    /// Resolve the Release bound to one tag, including whether it is a draft.
    fn release(&self, repository: &str, tag: &str) -> Result<ReleaseRecord>;

    /// List the assets one Release holds.
    fn assets(&self, repository: &str, release_id: u64) -> Result<Vec<ReleaseAsset>>;

    /// Read exactly the bytes of one Release asset.
    fn asset_bytes(&self, repository: &str, asset_id: u64) -> Result<Vec<u8>>;

    /// Resolve the GitHub artifact attestation covering one asset's bytes.
    fn attestation(&self, repository: &str, name: &str, bytes: &[u8]) -> Result<Attestation>;
}

/// One `gh` invocation and its captured result.
///
/// Arguments are passed literally and never through a shell, so a tag, asset
/// name, or repository value can never become executable text.
struct GhCommand<'a> {
    directory: &'a Path,
    arguments: Vec<String>,
}

impl<'a> GhCommand<'a> {
    /// Start a `gh` invocation inside `directory`.
    fn new(directory: &'a Path) -> Self {
        Self {
            directory,
            arguments: Vec::new(),
        }
    }

    /// Append several literal arguments.
    fn args<I, S>(mut self, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for value in values {
            self.arguments.push(value.as_ref().to_owned());
        }
        self
    }

    /// Append one literal argument.
    fn arg(mut self, value: impl AsRef<str>) -> Self {
        self.arguments.push(value.as_ref().to_owned());
        self
    }

    /// Run the invocation and require a successful exit status.
    fn run(self) -> Result<Vec<u8>> {
        let description = format!("gh {}", self.arguments.join(" "));
        let mut command = Command::new("gh");
        command
            .current_dir(self.directory)
            .args(&self.arguments)
            // A pager or prompt would block a non-interactive verification run
            // and corrupt the parsed output.
            .env("GH_PAGER", "")
            .env("GH_PROMPT_DISABLED", "1")
            .env("NO_COLOR", "1")
            // Reading GitHub never supplies input, and a closed standard
            // input keeps a prompt from silently waiting on one.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command
            .spawn()
            .map_err(|error| Error::Validation(format!("failed to run {description}: {error}")))?;
        let output = child.wait_with_output().map_err(|error| {
            Error::Validation(format!("failed to collect {description} output: {error}"))
        })?;
        if !output.status.success() {
            return Err(Error::Validation(format!(
                "{description} failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(output.stdout)
    }

    /// Run the invocation and parse its standard output as JSON.
    fn json(self) -> Result<serde_json::Value> {
        let description = format!("gh {}", self.arguments.join(" "));
        let bytes = self.run()?;
        serde_json::from_slice(&bytes).map_err(|error| {
            Error::Validation(format!("{description} produced unreadable JSON: {error}"))
        })
    }
}

/// A release source backed by the `gh` command line.
///
/// `gh` already carries the operator's GitHub credentials and is the read path
/// this repository's workflows use, so verification adds no new credential
/// surface of its own.
#[derive(Debug, Clone)]
pub struct GhReleaseSource {
    directory: std::path::PathBuf,
}

impl GhReleaseSource {
    /// Read GitHub through `gh` invoked inside `directory`.
    pub fn new(directory: impl Into<std::path::PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }
}

/// Read one required unsigned integer out of a JSON object.
fn json_u64(value: &serde_json::Value, pointer: &str) -> Result<u64> {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_u64)
        .ok_or_else(|| {
            Error::Validation(format!(
            "GitHub response is missing an integer at {pointer}; expected a complete Release record"
        ))
        })
}

/// Read one required string out of a JSON object.
fn json_str(value: &serde_json::Value, pointer: &str) -> Result<String> {
    value
        .pointer(pointer)
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| {
            Error::Validation(format!(
                "GitHub response is missing a string at {pointer}; expected a complete Release record"
            ))
        })
}

impl ReleaseSource for GhReleaseSource {
    fn release(&self, repository: &str, tag: &str) -> Result<ReleaseRecord> {
        let value = GhCommand::new(&self.directory)
            .args(["api", "--method", "GET"])
            .arg(format!("repos/{repository}/releases/tags/{tag}"))
            .json()?;
        Ok(ReleaseRecord {
            id: json_u64(&value, "/id")?,
            tag: json_str(&value, "/tag_name")?,
            draft: value
                .pointer("/draft")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        })
    }

    fn assets(&self, repository: &str, release_id: u64) -> Result<Vec<ReleaseAsset>> {
        let value = GhCommand::new(&self.directory)
            .args(["api", "--paginate", "--method", "GET"])
            .arg(format!("repos/{repository}/releases/{release_id}/assets"))
            .json()?;
        let entries = value.as_array().ok_or_else(|| {
            Error::Validation(format!(
                "GitHub returned no asset array for Release {release_id} in {repository}"
            ))
        })?;
        entries
            .iter()
            .map(|entry| {
                Ok(ReleaseAsset {
                    id: json_u64(entry, "/id")?,
                    name: json_str(entry, "/name")?,
                    size: json_u64(entry, "/size")?,
                    media_type: json_str(entry, "/content_type")?,
                })
            })
            .collect()
    }

    fn asset_bytes(&self, repository: &str, asset_id: u64) -> Result<Vec<u8>> {
        GhCommand::new(&self.directory)
            .args([
                "api",
                "--method",
                "GET",
                "-H",
                "Accept: application/octet-stream",
            ])
            .arg(format!("repos/{repository}/releases/assets/{asset_id}"))
            .run()
    }

    fn attestation(&self, repository: &str, name: &str, bytes: &[u8]) -> Result<Attestation> {
        // `gh attestation verify` performs the cryptographic verification and
        // reports the verified statement, so the decision stays with the tool
        // that owns the trust roots rather than being re-derived here.
        let directory = tempfile::Builder::new()
            .prefix("intentional-attestation")
            .tempdir()
            .map_err(|error| {
                Error::Validation(format!("failed to stage an attested asset: {error}"))
            })?;
        let path = directory.path().join(name);
        std::fs::write(&path, bytes).map_err(|error| Error::io(&path, error))?;
        let value = GhCommand::new(&self.directory)
            .args(["attestation", "verify"])
            .arg(path.display().to_string())
            .args(["--repo", repository, "--format", "json"])
            .json()?;
        let result = value
            .as_array()
            .and_then(|results| results.first())
            .unwrap_or(&value);
        let subject_digest = json_str(
            result,
            "/verificationResult/statement/subject/0/digest/sha256",
        )?;
        let invocation = json_str(
            result,
            "/verificationResult/statement/predicate/runDetails/metadata/invocationId",
        )?;
        Ok(Attestation {
            subject_digest: format!("sha256:{subject_digest}"),
            repository: json_str(
                result,
                "/verificationResult/statement/predicate/buildDefinition/externalParameters/workflow/repository",
            )
            .map(|uri| uri.trim_start_matches("https://github.com/").to_owned())
            .unwrap_or_else(|_| repository.to_owned()),
            workflow: json_str(
                result,
                "/verificationResult/statement/predicate/buildDefinition/externalParameters/workflow/path",
            )
            .unwrap_or_default(),
            run_id: run_identifier(&invocation)?,
        })
    }
}

/// Extract the workflow run identifier from an attested invocation reference.
fn run_identifier(invocation: &str) -> Result<u64> {
    invocation
        .split('/')
        .filter_map(|segment| segment.parse::<u64>().ok())
        .next_back()
        .ok_or_else(|| {
            Error::Validation(format!(
                "attested invocation {invocation:?} names no workflow run identifier"
            ))
        })
}

/// The document one fresh live observation produces.
///
/// Live verification and repository-local readback describe a destination with
/// the same contract, so they name one type rather than two that must be kept
/// in agreement.
pub type LiveObservation = PublicationObservation;

/// Fresh post-closure observation of one published destination.
///
/// Live verification needs the same destination adapters publication uses, so
/// the observation contract is a seam rather than something this module
/// implements per publisher. An observer reports the same document a
/// repository-local readback writes, so the two paths stay one contract.
pub trait DestinationObserver {
    /// Read one publication back and retrieve it through its public client.
    fn observe(&self, repository: &str, fragment: &PublisherEvidence) -> Result<LiveObservation>;
}

/// The live observer that a closed GitHub Release is itself sufficient for.
///
/// A draft-dependent publisher could only record authenticated draft retrieval
/// before closure, so its first genuine public client check is exactly the one
/// this observer performs: resolve the closed Release's assets and retrieve the
/// subject through them. Every other publisher needs its own destination
/// adapter, which this observer does not claim to be.
struct ReleaseAssetObserver<'a> {
    source: &'a dyn ReleaseSource,
    release_id: u64,
    assets: &'a [ReleaseAsset],
}

impl DestinationObserver for ReleaseAssetObserver<'_> {
    fn observe(
        &self,
        repository: &str,
        fragment: &PublisherEvidence,
    ) -> Result<PublicationObservation> {
        if !is_draft_dependent(fragment.publisher) {
            return Err(Error::Validation(format!(
                "live readback of {} needs a destination observer for publisher {}; a closed GitHub Release only proves draft-dependent publications",
                fragment.identity(),
                fragment.publisher
            )));
        }
        for asset in self.assets {
            let digest = digest_bytes(&self.source.asset_bytes(repository, asset.id)?);
            if digest != fragment.subject.digest {
                continue;
            }
            return Ok(PublicationObservation {
                schema: PUBLICATION_OBSERVATION_SCHEMA.to_owned(),
                contract: PUBLICATION_OBSERVATION_CONTRACT.to_owned(),
                release_unit: fragment.release_unit.clone(),
                publisher: fragment.publisher,
                target: fragment.target.clone(),
                state: ObservationState::Present,
                subject: Some(fragment.subject.clone()),
                packager: None,
                build_provenance: Vec::new(),
                attached_metadata: Vec::new(),
                destination: Some(Destination {
                    identity: fragment.destination.identity.clone(),
                    version: fragment.subject.version.clone(),
                    digest: digest.clone(),
                }),
                retrieval: Some(CleanClient {
                    mode: CleanClientMode::Public,
                    client: "github-release".to_owned(),
                    version: crate::VERSION.to_owned(),
                    digest,
                }),
                destination_aliases: Vec::new(),
                conflict: None,
            });
        }
        Err(Error::Validation(format!(
            "public retrieval of {} found no asset carrying subject digest {} in Release {}",
            fragment.identity(),
            fragment.subject.digest,
            self.release_id
        )))
    }
}

/// Everything one `intentional verify release` invocation proved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseVerification {
    /// Global release tag the Release is bound to.
    pub global_tag: String,
    /// Stable GitHub Release identifier.
    pub release_id: u64,
    /// Whether fresh destination observation was performed.
    pub live: bool,
    entries: Vec<String>,
}

impl ReleaseVerification {
    /// What verified, in a stable order, one line per proved claim.
    ///
    /// Rendering stays here rather than in the command so the same report can
    /// be projected by any caller without core writing to a terminal.
    pub fn report(&self) -> Vec<String> {
        self.entries.clone()
    }
}

/// Verify one released version's GitHub Release and its durable evidence.
///
/// Live observation of a destination that is not a GitHub Release asset needs
/// a publisher-specific adapter, which `verify_release_observed` accepts.
pub fn verify_release(
    root: &Path,
    version: &str,
    live: bool,
    source: &dyn ReleaseSource,
) -> Result<ReleaseVerification> {
    verify_release_observed(root, version, live, source, None)
}

/// Verify one released version with an explicit live destination observer.
pub fn verify_release_observed(
    root: &Path,
    version: &str,
    live: bool,
    source: &dyn ReleaseSource,
    observer: Option<&dyn DestinationObserver>,
) -> Result<ReleaseVerification> {
    let config = Config::load(root)?;
    let repository = repository_identity(root)?;
    let global_tag = global_tag_name(&config, version)?;

    let release = source.release(&repository, &global_tag)?;
    if release.draft {
        // Closure is the final authority transition, so a draft is not a
        // release and nothing downstream of it is worth verifying.
        return Err(Error::Validation(format!(
            "GitHub Release {} for tag {global_tag} is still a draft; verification applies to a closed release",
            release.id
        )));
    }
    if release.tag != global_tag {
        return Err(Error::Validation(format!(
            "GitHub Release {} is bound to tag {} instead of {global_tag}",
            release.id, release.tag
        )));
    }

    let mut findings = Vec::new();
    let mut entries = vec![format!(
        "release {global_tag} is published in {repository} as GitHub Release {}",
        release.id
    )];

    let assets = source.assets(&repository, release.id)?;
    let asset = assets
        .iter()
        .find(|asset| asset.name == RELEASE_EVIDENCE_FILE)
        .ok_or_else(|| {
            Error::Validation(format!(
                "GitHub Release {} holds no {RELEASE_EVIDENCE_FILE} asset; the durable evidence record is required",
                release.id
            ))
        })?;
    let bytes = source.asset_bytes(&repository, asset.id)?;
    let digest = digest_bytes(&bytes);
    if bytes.len() as u64 != asset.size {
        findings.push(format!(
            "evidence asset {RELEASE_EVIDENCE_FILE} declares {} bytes but retrieved {}",
            asset.size,
            bytes.len()
        ));
    }
    let text = String::from_utf8(bytes.clone()).map_err(|error| {
        Error::Validation(format!(
            "evidence asset {RELEASE_EVIDENCE_FILE} is not UTF-8: {error}"
        ))
    })?;
    let evidence: ReleaseEvidence = serde_yaml::from_str(&text)?;
    if evidence.schema != RELEASE_EVIDENCE_SCHEMA || evidence.contract != RELEASE_EVIDENCE_CONTRACT
    {
        findings.push(format!(
            "evidence asset {RELEASE_EVIDENCE_FILE} declares {} / {} instead of {RELEASE_EVIDENCE_SCHEMA} / {RELEASE_EVIDENCE_CONTRACT}",
            evidence.schema, evidence.contract
        ));
    }
    entries.push(format!(
        "evidence asset {RELEASE_EVIDENCE_FILE} is {digest}"
    ));

    match source.attestation(&repository, &asset.name, &bytes) {
        Err(error) => findings.push(format!(
            "artifact attestation for {RELEASE_EVIDENCE_FILE} did not verify: {error}"
        )),
        Ok(attestation) => {
            if attestation.subject_digest != digest {
                findings.push(format!(
                    "artifact attestation binds {} but {RELEASE_EVIDENCE_FILE} is {digest}",
                    attestation.subject_digest
                ));
            }
            if attestation.repository != evidence.workflow.repository {
                findings.push(format!(
                    "artifact attestation names repository {} but the evidence records {}",
                    attestation.repository, evidence.workflow.repository
                ));
            }
            if attestation.run_id != evidence.workflow.run_id {
                findings.push(format!(
                    "artifact attestation names workflow run {} but the evidence records {}",
                    attestation.run_id, evidence.workflow.run_id
                ));
            }
            entries.push(format!(
                "attestation binds {digest} to {} run {}",
                attestation.repository, attestation.run_id
            ));
        }
    }

    verify_identity_chain(root, &evidence, &global_tag, &mut findings, &mut entries)?;
    verify_phase_tags(
        root,
        &config,
        version,
        &evidence,
        &mut findings,
        &mut entries,
    )?;
    verify_publishers(&evidence, &mut findings, &mut entries);
    if live {
        let default = ReleaseAssetObserver {
            source,
            release_id: release.id,
            assets: &assets,
        };
        verify_live(
            &repository,
            &evidence,
            observer.unwrap_or(&default),
            &mut findings,
            &mut entries,
        );
    }

    if !findings.is_empty() {
        return Err(Error::Validation(findings.join("\n")));
    }
    Ok(ReleaseVerification {
        global_tag,
        release_id: release.id,
        live,
        entries,
    })
}

/// Resolve the owner and repository the workspace pushes its release to.
fn repository_identity(root: &Path) -> Result<String> {
    let url = GitCommand::new(root)
        .args(["remote", "get-url", "origin"])
        .run()?
        .line()?;
    let path = url
        .rsplit_once(':')
        .map_or(url.as_str(), |(_, path)| path)
        .rsplit("github.com/")
        .next()
        .unwrap_or_default()
        .trim_end_matches('/')
        .trim_end_matches(".git");
    let segments = path.split('/').collect::<Vec<_>>();
    match segments.as_slice() {
        [owner, name] if !owner.is_empty() && !name.is_empty() => Ok(format!("{owner}/{name}")),
        _ => Err(Error::Validation(format!(
            "origin remote {url:?} does not name a GitHub owner/repository"
        ))),
    }
}

/// Render the one global release tag the configuration declares.
fn global_tag_name(config: &Config, version: &str) -> Result<String> {
    let unphased = config.unphased_tags();
    match unphased.as_slice() {
        [tag] => Ok(tag.template.replace("{version}", version)),
        _ => Err(Error::Validation(format!(
            "configuration declares {} tags without an executor phase; exactly one is the global release tag",
            unphased.len()
        ))),
    }
}

/// Prove S, R, and the global tag agree with the repository they came from.
fn verify_identity_chain(
    root: &Path,
    evidence: &ReleaseEvidence,
    global_tag: &str,
    findings: &mut Vec<String>,
    entries: &mut Vec<String>,
) -> Result<()> {
    let release = &evidence.release;
    if release.global_tag.name != global_tag {
        findings.push(format!(
            "evidence records global tag {} but the requested version renders {global_tag}",
            release.global_tag.name
        ));
    }
    for (field, value) in [
        ("source-commit", &release.source_commit),
        ("release-commit", &release.release_commit),
        ("global-tag.object", &release.global_tag.object),
        ("global-tag.target", &release.global_tag.target),
    ] {
        if !is_git_object(value) {
            findings.push(format!(
                "evidence records {field} {value:?}, which is not a complete Git object identifier"
            ));
        }
    }
    if !is_digest(&release.plan_digest) {
        findings.push(format!(
            "evidence records plan-digest {:?}, which is not a sha256 digest",
            release.plan_digest
        ));
    }
    if release.global_tag.target != release.release_commit {
        findings.push(format!(
            "evidence global tag targets {} but records release commit {}",
            release.global_tag.target, release.release_commit
        ));
    }

    let tag_object = rev_parse(root, &format!("refs/tags/{}", release.global_tag.name))?;
    match tag_object {
        None => findings.push(format!(
            "repository holds no tag {}; the release identity cannot be checked against it",
            release.global_tag.name
        )),
        Some(object) if object != release.global_tag.object => findings.push(format!(
            "repository tag {} is object {object} but the evidence records {}",
            release.global_tag.name, release.global_tag.object
        )),
        Some(_) => {}
    }
    match rev_parse(
        root,
        &format!("refs/tags/{}^{{commit}}", release.global_tag.name),
    )? {
        None => {}
        Some(commit) if commit != release.release_commit => findings.push(format!(
            "repository tag {} targets commit {commit} but the evidence records {}",
            release.global_tag.name, release.release_commit
        )),
        Some(_) => {}
    }
    let reachable = GitCommand::new(root)
        .args(["merge-base", "--is-ancestor"])
        .arg(&release.source_commit)
        .arg(&release.release_commit)
        .output()?
        .succeeded();
    if !reachable {
        findings.push(format!(
            "source commit {} is not an ancestor of release commit {} in this repository",
            release.source_commit, release.release_commit
        ));
    } else {
        entries.push(format!(
            "identity chain {} -> {} -> {} matches the repository",
            release.source_commit, release.release_commit, release.global_tag.name
        ));
    }
    Ok(())
}

/// Resolve one revision, reporting absence rather than failing.
fn rev_parse(root: &Path, revision: &str) -> Result<Option<String>> {
    let output = GitCommand::new(root)
        .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
        .arg(revision)
        .output()?;
    if !output.succeeded() {
        return Ok(None);
    }
    let line = output.line()?;
    Ok((!line.is_empty()).then_some(line))
}

/// One configured tag whose creation declares an executor phase.
struct PhasedTag {
    name: String,
    phase: TagPhase,
}

/// Render every configured phase tag for one version, in stable order.
fn phased_tags(config: &Config, version: &str) -> Vec<PhasedTag> {
    let mut tags = Vec::new();
    for (release_unit_id, release_unit) in &config.release_units {
        for tag in release_unit.tags.values() {
            if let Some(phase) = tag.require_phase {
                tags.push(PhasedTag {
                    name: tag
                        .template
                        .replace("{id}", release_unit_id)
                        .replace("{version}", version),
                    phase,
                });
            }
        }
    }
    for tag in config.workspace_tags.values() {
        if let Some(phase) = tag.require_phase {
            tags.push(PhasedTag {
                name: tag.template.replace("{version}", version),
                phase,
            });
        }
    }
    tags
}

/// Compare every applicable phase tag with the fragments the evidence recorded.
fn verify_phase_tags(
    root: &Path,
    config: &Config,
    version: &str,
    evidence: &ReleaseEvidence,
    findings: &mut Vec<String>,
    entries: &mut Vec<String>,
) -> Result<()> {
    let recorded = recorded_fragments(evidence);
    let identities = recorded
        .iter()
        .map(|(identity, _)| identity.clone())
        .collect::<BTreeSet<_>>();
    for tag in phased_tags(config, version) {
        let Some(phase) = read_phase_evidence(root, &tag.name)? else {
            findings.push(format!(
                "repository holds no phase tag {} carrying a {PHASE_EVIDENCE_FIELD} record",
                tag.name
            ));
            continue;
        };
        if phase.schema != PHASE_TAG_EVIDENCE_SCHEMA {
            findings.push(format!(
                "phase tag {} declares schema {:?} instead of {PHASE_TAG_EVIDENCE_SCHEMA}",
                tag.name, phase.schema
            ));
        }
        if phase.phase != tag.phase {
            findings.push(format!(
                "phase tag {} declares {} but the configuration requires {}",
                tag.name, phase.phase, tag.phase
            ));
        }
        if phase.source_commit != evidence.release.source_commit
            || phase.release_commit != evidence.release.release_commit
            || phase.global_tag != evidence.release.global_tag.name
            || phase.plan_digest != evidence.release.plan_digest
        {
            findings.push(format!(
                "phase tag {} disagrees with the release identity the evidence records",
                tag.name
            ));
            continue;
        }
        if let Some(intended) = &phase.intended_destinations {
            let sealed = intended
                .iter()
                .map(|destination| {
                    format!(
                        "{}/{}/{}",
                        destination.release_unit, destination.publisher, destination.target
                    )
                })
                .collect::<BTreeSet<_>>();
            for identity in sealed.difference(&identities) {
                findings.push(format!(
                    "phase tag {} intends publication {identity}, which the evidence does not record",
                    tag.name
                ));
            }
            for identity in identities.difference(&sealed) {
                findings.push(format!(
                    "phase tag {} does not intend recorded publication {identity}",
                    tag.name
                ));
            }
        }
        if let Some(sealed) = &phase.publisher_evidence {
            for fragment in sealed {
                let identity = fragment.identity();
                match recorded.iter().find(|(recorded, _)| recorded == &identity) {
                    None => findings.push(format!(
                        "phase tag {} seals publisher evidence {identity} the evidence does not record",
                        tag.name
                    )),
                    Some((_, current)) if *current != fragment => findings.push(format!(
                        "phase tag {} seals publisher evidence {identity} that differs from the recorded fragment",
                        tag.name
                    )),
                    Some(_) => {}
                }
            }
        }
        for subject in &phase.subjects {
            for (identity, fragment) in &recorded {
                if fragment.release_unit != subject.release_unit
                    || fragment.subject.identity != subject.identity
                {
                    continue;
                }
                if fragment.subject.digest != subject.digest
                    || fragment.subject.version != subject.version
                {
                    findings.push(format!(
                        "{identity} records subject {} as {}@{} while phase tag {} sealed {}@{}",
                        subject.identity,
                        fragment.subject.version,
                        fragment.subject.digest,
                        tag.name,
                        subject.version,
                        subject.digest
                    ));
                }
            }
        }
        entries.push(format!(
            "phase tag {} agrees with the recorded evidence",
            tag.name
        ));
    }
    Ok(())
}

/// Read the phase evidence one annotated tag embeds, if the tag exists.
fn read_phase_evidence(root: &Path, name: &str) -> Result<Option<PhaseTagEvidence>> {
    let output = GitCommand::new(root)
        .args(["tag", "-l", "--format=%(contents)"])
        .arg(name)
        .output()?;
    if !output.succeeded() {
        return Ok(None);
    }
    let Some(encoded) = output
        .text()?
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{PHASE_EVIDENCE_FIELD}: ")))
    else {
        return Ok(None);
    };
    decode(encoded).map(Some).map_err(|error| {
        Error::Validation(format!(
            "phase tag {name} carries unusable phase evidence: {error}"
        ))
    })
}

/// Every recorded publisher fragment in stable publication-identity order.
fn recorded_fragments(evidence: &ReleaseEvidence) -> Vec<(String, &PublisherEvidence)> {
    evidence
        .release_units
        .values()
        .flat_map(|unit| unit.publishers.values())
        .flat_map(|publisher| publisher.targets.values())
        .map(|fragment| (fragment.identity(), fragment))
        .collect()
}

/// Prove every fragment's native provenance binds the subject it published.
fn verify_publishers(
    evidence: &ReleaseEvidence,
    findings: &mut Vec<String>,
    entries: &mut Vec<String>,
) {
    for (identity, fragment) in recorded_fragments(evidence) {
        if !is_digest(&fragment.subject.digest) {
            findings.push(format!(
                "{identity} records subject digest {:?}, which is not a sha256 digest",
                fragment.subject.digest
            ));
        }
        if fragment.build_provenance.is_empty() {
            findings.push(format!(
                "{identity} records no native build provenance for subject {}",
                fragment.subject.identity
            ));
            continue;
        }
        for reference in &fragment.build_provenance {
            if !is_digest(&reference.digest) {
                findings.push(format!(
                    "{identity} records {} provenance digest {:?}, which is not a sha256 digest",
                    reference.kind, reference.digest
                ));
            }
        }
        if !fragment
            .build_provenance
            .iter()
            .any(|reference| reference.digest == fragment.subject.digest)
        {
            findings.push(format!(
                "{identity} records provenance that does not bind subject digest {}",
                fragment.subject.digest
            ));
            continue;
        }
        entries.push(format!(
            "{identity} provenance binds subject {}@{} as {}",
            fragment.subject.identity, fragment.subject.version, fragment.subject.digest
        ));
    }
}

/// Observe every recorded publication again, reporting what it looks like now.
///
/// Nothing here writes: a disagreement is a finding, never a correction to the
/// immutable evidence assembled before closure.
fn verify_live(
    repository: &str,
    evidence: &ReleaseEvidence,
    observer: &dyn DestinationObserver,
    findings: &mut Vec<String>,
    entries: &mut Vec<String>,
) {
    for (identity, fragment) in recorded_fragments(evidence) {
        let observation = match observer.observe(repository, fragment) {
            Ok(observation) => observation,
            Err(error) => {
                findings.push(format!("live verification of {identity} failed: {error}"));
                continue;
            }
        };
        if observation.identity() != identity {
            findings.push(format!(
                "live verification of {identity} returned an observation of {}",
                observation.identity()
            ));
            continue;
        }
        if observation.state != ObservationState::Present {
            findings.push(format!(
                "live readback of {identity} reports {} rather than a present publication",
                observation.state
            ));
            continue;
        }
        match &observation.destination {
            None => findings.push(format!(
                "live readback of {identity} reports no destination identity, version, or digest"
            )),
            Some(destination) if destination != &fragment.destination => findings.push(format!(
                "live readback of {identity} reports {} {} {} but the evidence records {} {} {}",
                destination.identity,
                destination.version,
                destination.digest,
                fragment.destination.identity,
                fragment.destination.version,
                fragment.destination.digest
            )),
            Some(destination) => entries.push(format!(
                "live readback of {identity} reports {} at {}",
                destination.identity, destination.digest
            )),
        }
        match &observation.retrieval {
            None => findings.push(format!(
                "live verification of {identity} performed no consumer retrieval"
            )),
            Some(retrieval) if retrieval.mode != CleanClientMode::Public => findings.push(format!(
                "live retrieval of {identity} was performed by a {:?} client rather than a public one",
                retrieval.mode
            )),
            Some(retrieval) if retrieval.digest != fragment.clean_client.digest => {
                findings.push(format!(
                    "live retrieval of {identity} produced {} but the evidence records {}",
                    retrieval.digest, fragment.clean_client.digest
                ))
            }
            Some(retrieval) => entries.push(format!(
                "live public retrieval of {identity} produced {}",
                retrieval.digest
            )),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::evidence::assemble::{
        EvidenceReference, PhaseSubject, PublisherTargets, ReleaseIdentity, ReleaseUnitEvidence,
        Subject, TagIdentity, WorkflowIdentity, PUBLISHER_EVIDENCE_CONTRACT,
        PUBLISHER_EVIDENCE_SCHEMA,
    };
    use crate::evidence::assemble::{PackagerRecord, PhaseTagEvidence};
    use crate::evidence::phase::encode;
    use crate::executor::fixture::Workspace;
    use crate::model::PublisherKind;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    const PLAN_DIGEST: &str =
        "sha256:4444444444444444444444444444444444444444444444444444444444444444";
    const REPOSITORY: &str = "example-owner/example-repository";
    const DELIVERABLE: &[u8] = b"component deliverable bytes";

    const CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-1
github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml }
workspace-tags:
  published:
    template: 'published/{version}'
    require-phase: after-publication
release-units:
  component:
    path: component
    homebrew:
      repository: example-owner/homebrew-example
    tags:
      primary: { role: primary, template: '{id}@{version}' }
"#;

    /// An in-memory Release used by every verification test.
    ///
    /// The fake carries no write path at all, which is the same property the
    /// production seam has: verification cannot alter what it observes.
    pub(crate) struct FakeReleaseSource {
        repository: String,
        tag: String,
        release_id: u64,
        draft: bool,
        assets: Vec<(ReleaseAsset, Vec<u8>)>,
        attestation: Option<Attestation>,
        reads: RefCell<Vec<String>>,
    }

    impl FakeReleaseSource {
        /// A draft Release holding no assets yet.
        pub(crate) fn draft(repository: &str, tag: &str, release_id: u64) -> Self {
            Self {
                repository: repository.to_owned(),
                tag: tag.to_owned(),
                release_id,
                draft: true,
                assets: Vec::new(),
                attestation: None,
                reads: RefCell::new(Vec::new()),
            }
        }

        /// The same Release after closure.
        pub(crate) fn published(mut self) -> Self {
            self.draft = false;
            self
        }

        /// Add one asset carrying exactly `bytes`.
        pub(crate) fn with_asset(
            mut self,
            id: u64,
            name: &str,
            media_type: &str,
            bytes: &[u8],
        ) -> Self {
            self.assets.push((
                ReleaseAsset {
                    id,
                    name: name.to_owned(),
                    size: bytes.len() as u64,
                    media_type: media_type.to_owned(),
                },
                bytes.to_vec(),
            ));
            self
        }

        /// Attest whatever bytes are presented, as GitHub would after closure.
        fn attesting(mut self, attestation: Attestation) -> Self {
            self.attestation = Some(attestation);
            self
        }

        /// Drop one asset, as a destination losing a published deliverable would.
        fn without_asset(mut self, name: &str) -> Self {
            self.assets.retain(|(asset, _)| asset.name != name);
            self
        }

        /// Every read this source served, in call order.
        fn reads(&self) -> Vec<String> {
            self.reads.borrow().clone()
        }

        /// The write path a release source deliberately does not have.
        fn write(&self, name: &str) -> ! {
            panic!("the release source is read-only; {name} cannot be written")
        }
    }

    impl ReleaseSource for FakeReleaseSource {
        fn release(&self, repository: &str, tag: &str) -> Result<ReleaseRecord> {
            self.reads.borrow_mut().push(format!("release {tag}"));
            if repository != self.repository || tag != self.tag {
                return Err(Error::Validation(format!(
                    "no release for {repository} tag {tag}"
                )));
            }
            Ok(ReleaseRecord {
                id: self.release_id,
                tag: self.tag.clone(),
                draft: self.draft,
            })
        }

        fn assets(&self, _repository: &str, release_id: u64) -> Result<Vec<ReleaseAsset>> {
            self.reads.borrow_mut().push(format!("assets {release_id}"));
            Ok(self.assets.iter().map(|(asset, _)| asset.clone()).collect())
        }

        fn asset_bytes(&self, _repository: &str, asset_id: u64) -> Result<Vec<u8>> {
            self.reads.borrow_mut().push(format!("bytes {asset_id}"));
            self.assets
                .iter()
                .find(|(asset, _)| asset.id == asset_id)
                .map(|(_, bytes)| bytes.clone())
                .ok_or_else(|| Error::Validation(format!("no asset {asset_id}")))
        }

        fn attestation(&self, _repository: &str, name: &str, bytes: &[u8]) -> Result<Attestation> {
            self.reads
                .borrow_mut()
                .push(format!("attestation {name} {}", bytes.len()));
            self.attestation
                .clone()
                .ok_or_else(|| Error::Validation(format!("no artifact attestation covers {name}")))
        }
    }

    /// An observer that reports whatever a test wants live observation to find.
    struct FakeObserver(PublicationObservation);

    impl DestinationObserver for FakeObserver {
        fn observe(
            &self,
            _repository: &str,
            _fragment: &PublisherEvidence,
        ) -> Result<PublicationObservation> {
            Ok(self.0.clone())
        }
    }

    /// One workspace whose repository actually carries S, R, and the tags.
    struct Released {
        workspace: Workspace,
        source_commit: String,
        release_commit: String,
        tag_object: String,
    }

    impl Released {
        /// Create the after-publication phase tag sealing one evidence document.
        fn seal(&self, phase: &PhaseTagEvidence) {
            let encoded = encode(phase).expect("phase evidence");
            git(
                self.workspace.root(),
                &[
                    "tag",
                    "-a",
                    "published/1.0.0",
                    "-m",
                    &format!("{PHASE_EVIDENCE_FIELD}: {encoded}"),
                ],
            );
        }
    }

    fn git(root: &Path, arguments: &[&str]) -> String {
        GitCommand::new(root)
            .args(arguments)
            .run()
            .expect("git succeeds")
            .line()
            .expect("git output")
    }

    /// Build a repository holding S, R, and the global release tag.
    fn released(label: &str) -> Released {
        let workspace = Workspace::new(label);
        workspace
            .write(".intentional/config.yml", CONFIG)
            .write(
                "component/go.mod",
                "module example.test/component\n\ngo 1.22\n",
            )
            .write("component/main.go", "package main\n\nfunc main() {}\n");
        let root = workspace.root().to_path_buf();
        git(&root, &["init", "--quiet", "--initial-branch=main"]);
        git(&root, &["config", "user.email", "release@example.test"]);
        git(&root, &["config", "user.name", "Example Release"]);
        git(
            &root,
            &[
                "remote",
                "add",
                "origin",
                "git@github.com:example-owner/example-repository.git",
            ],
        );
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "--quiet", "-m", "source"]);
        let source_commit = git(&root, &["rev-parse", "HEAD"]);
        std::fs::write(root.join("component/RELEASE"), "1.0.0\n").expect("release file");
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "--quiet", "-m", "release"]);
        let release_commit = git(&root, &["rev-parse", "HEAD"]);
        git(
            &root,
            &["tag", "-a", "component@1.0.0", "-m", "global release tag"],
        );
        let tag_object = git(&root, &["rev-parse", "refs/tags/component@1.0.0"]);
        Released {
            workspace,
            source_commit,
            release_commit,
            tag_object,
        }
    }

    fn fragment(released: &Released) -> PublisherEvidence {
        let digest = digest_bytes(DELIVERABLE);
        PublisherEvidence {
            schema: PUBLISHER_EVIDENCE_SCHEMA.to_owned(),
            contract: PUBLISHER_EVIDENCE_CONTRACT.to_owned(),
            release_unit: "component".to_owned(),
            publisher: PublisherKind::Homebrew,
            target: "primary".to_owned(),
            source_commit: released.source_commit.clone(),
            release_commit: released.release_commit.clone(),
            global_tag: TagIdentity {
                name: "component@1.0.0".to_owned(),
                object: released.tag_object.clone(),
                target: released.release_commit.clone(),
            },
            plan_digest: PLAN_DIGEST.to_owned(),
            subject: Subject {
                kind: "homebrew-formula".to_owned(),
                identity: "example-component".to_owned(),
                version: "1.0.0".to_owned(),
                digest: digest.clone(),
            },
            packager: PackagerRecord {
                id: "brew".to_owned(),
                version: "4.3.0".to_owned(),
            },
            build_provenance: vec![EvidenceReference {
                kind: "slsa-provenance".to_owned(),
                digest: digest.clone(),
                reference: None,
            }],
            attached_metadata: Vec::new(),
            destination: Destination {
                identity: "example-owner/homebrew-example".to_owned(),
                version: "1.0.0".to_owned(),
                digest: digest.clone(),
            },
            clean_client: CleanClient {
                mode: CleanClientMode::AuthenticatedDraft,
                client: "brew".to_owned(),
                version: "4.3.0".to_owned(),
                digest,
            },
            destination_aliases: Vec::new(),
            phase_tags: Vec::new(),
        }
    }

    fn evidence(released: &Released, fragment: PublisherEvidence) -> ReleaseEvidence {
        ReleaseEvidence {
            schema: RELEASE_EVIDENCE_SCHEMA.to_owned(),
            contract: RELEASE_EVIDENCE_CONTRACT.to_owned(),
            release: ReleaseIdentity {
                source_commit: released.source_commit.clone(),
                release_commit: released.release_commit.clone(),
                global_tag: TagIdentity {
                    name: "component@1.0.0".to_owned(),
                    object: released.tag_object.clone(),
                    target: released.release_commit.clone(),
                },
                plan_digest: PLAN_DIGEST.to_owned(),
            },
            workflow: WorkflowIdentity {
                repository: REPOSITORY.to_owned(),
                workflow: "publish".to_owned(),
                run_id: 42,
                run_attempt: 1,
                commit: released.release_commit.clone(),
            },
            release_units: BTreeMap::from([(
                "component".to_owned(),
                ReleaseUnitEvidence {
                    publishers: BTreeMap::from([(
                        "homebrew".to_owned(),
                        PublisherTargets {
                            targets: BTreeMap::from([("primary".to_owned(), fragment)]),
                        },
                    )]),
                },
            )]),
            contributions: BTreeMap::new(),
            contribution_attachments: Vec::new(),
        }
    }

    fn phase_evidence(released: &Released, fragment: &PublisherEvidence) -> PhaseTagEvidence {
        PhaseTagEvidence {
            schema: PHASE_TAG_EVIDENCE_SCHEMA.to_owned(),
            phase: TagPhase::AfterPublication,
            source_commit: released.source_commit.clone(),
            release_commit: released.release_commit.clone(),
            global_tag: "component@1.0.0".to_owned(),
            plan_digest: PLAN_DIGEST.to_owned(),
            subjects: vec![PhaseSubject {
                release_unit: "component".to_owned(),
                identity: fragment.subject.identity.clone(),
                version: fragment.subject.version.clone(),
                digest: fragment.subject.digest.clone(),
                provenance: None,
            }],
            intended_destinations: None,
            publisher_evidence: Some(vec![fragment.clone()]),
        }
    }

    fn source(evidence: &ReleaseEvidence) -> FakeReleaseSource {
        let document = evidence.to_yaml().expect("evidence document");
        FakeReleaseSource::draft(REPOSITORY, "component@1.0.0", 7)
            .published()
            .with_asset(21, RELEASE_EVIDENCE_FILE, "text/yaml", document.as_bytes())
            .with_asset(
                22,
                "component-1.0.0.tar.gz",
                "application/gzip",
                DELIVERABLE,
            )
            .attesting(Attestation {
                subject_digest: digest_bytes(document.as_bytes()),
                repository: REPOSITORY.to_owned(),
                workflow: "publish".to_owned(),
                run_id: 42,
            })
    }

    /// A complete release, its evidence, and a source that serves both.
    ///
    /// The phase evidence seals the commits it was created from, so the
    /// repository exists before the tag content can be computed and the tag
    /// exists before verification reads it.
    fn scenario(label: &str) -> (Released, ReleaseEvidence, FakeReleaseSource) {
        let released = released(label);
        let fragment = fragment(&released);
        released.seal(&phase_evidence(&released, &fragment));
        let evidence = evidence(&released, fragment);
        let source = source(&evidence);
        (released, evidence, source)
    }

    #[test]
    fn verifies_a_complete_release_from_its_durable_evidence() {
        let (released, _, source) = scenario("verify-complete");
        let verification =
            verify_release(released.workspace.root(), "1.0.0", false, &source).expect("verifies");
        assert_eq!(verification.global_tag, "component@1.0.0");
        assert_eq!(verification.release_id, 7);
        assert!(!verification.live);
        let report = verification.report();
        assert!(
            report[0].contains("published in example-owner/example-repository"),
            "{report:?}"
        );
        assert!(
            report.iter().any(|line| line.contains("identity chain")),
            "{report:?}"
        );
        assert!(
            report
                .iter()
                .any(|line| line.contains("phase tag published/1.0.0 agrees")),
            "{report:?}"
        );
        assert!(
            report
                .iter()
                .any(|line| line.contains("component/homebrew/primary provenance binds")),
            "{report:?}"
        );
    }

    #[test]
    fn rejects_a_release_that_holds_no_evidence_asset() {
        let (released, _, source) = scenario("verify-missing-evidence");
        let stripped = source.without_asset(RELEASE_EVIDENCE_FILE);
        let error = verify_release(released.workspace.root(), "1.0.0", false, &stripped)
            .expect_err("the missing asset is refused");
        assert!(error.to_string().contains(RELEASE_EVIDENCE_FILE), "{error}");
    }

    #[test]
    fn rejects_an_evidence_asset_whose_attestation_binds_another_digest() {
        let (released, evidence, _) = scenario("verify-digest");
        let document = evidence.to_yaml().expect("document");
        let source = FakeReleaseSource::draft(REPOSITORY, "component@1.0.0", 7)
            .published()
            .with_asset(21, RELEASE_EVIDENCE_FILE, "text/yaml", document.as_bytes())
            .with_asset(
                22,
                "component-1.0.0.tar.gz",
                "application/gzip",
                DELIVERABLE,
            )
            .attesting(Attestation {
                subject_digest: digest_bytes(b"other bytes"),
                repository: REPOSITORY.to_owned(),
                workflow: "publish".to_owned(),
                run_id: 42,
            });
        let error = verify_release(released.workspace.root(), "1.0.0", false, &source)
            .expect_err("the digest is refused");
        assert!(
            error.to_string().contains("artifact attestation binds"),
            "{error}"
        );
    }

    #[test]
    fn rejects_an_evidence_asset_whose_attestation_does_not_verify() {
        let (released, evidence, _) = scenario("verify-attestation");
        let document = evidence.to_yaml().expect("document");
        let source = FakeReleaseSource::draft(REPOSITORY, "component@1.0.0", 7)
            .published()
            .with_asset(21, RELEASE_EVIDENCE_FILE, "text/yaml", document.as_bytes())
            .with_asset(
                22,
                "component-1.0.0.tar.gz",
                "application/gzip",
                DELIVERABLE,
            );
        let error = verify_release(released.workspace.root(), "1.0.0", false, &source)
            .expect_err("the attestation is refused");
        assert!(error.to_string().contains("did not verify"), "{error}");
    }

    #[test]
    fn rejects_an_identity_chain_the_repository_does_not_carry() {
        let (released, mut evidence, _) = scenario("verify-chain");
        evidence.release.release_commit = "2222222222222222222222222222222222222222".to_owned();
        evidence.release.global_tag.target = evidence.release.release_commit.clone();
        let source = source(&evidence);
        let error = verify_release(released.workspace.root(), "1.0.0", false, &source)
            .expect_err("the chain is refused");
        assert!(
            error
                .to_string()
                .contains("is not an ancestor of release commit")
                || error.to_string().contains("targets commit"),
            "{error}"
        );
    }

    #[test]
    fn rejects_phase_tag_evidence_that_disagrees_with_the_recorded_fragments() {
        let (released, mut evidence, _) = scenario("verify-phase");
        let fragment = evidence
            .release_units
            .get_mut("component")
            .and_then(|unit| unit.publishers.get_mut("homebrew"))
            .and_then(|publisher| publisher.targets.get_mut("primary"))
            .expect("recorded fragment");
        fragment.packager.version = "9.9.9".to_owned();
        let source = source(&evidence);
        let error = verify_release(released.workspace.root(), "1.0.0", false, &source)
            .expect_err("the sealed fragment is refused");
        assert!(
            error
                .to_string()
                .contains("differs from the recorded fragment"),
            "{error}"
        );
    }

    #[test]
    fn rejects_a_subject_digest_that_disagrees_with_its_phase_tag() {
        let (released, mut evidence, _) = scenario("verify-subject");
        let replacement = digest_bytes(b"a different subject");
        let fragment = evidence
            .release_units
            .get_mut("component")
            .and_then(|unit| unit.publishers.get_mut("homebrew"))
            .and_then(|publisher| publisher.targets.get_mut("primary"))
            .expect("recorded fragment");
        fragment.subject.digest = replacement.clone();
        fragment.build_provenance[0].digest = replacement;
        let source = source(&evidence);
        let error = verify_release(released.workspace.root(), "1.0.0", false, &source)
            .expect_err("the subject digest is refused");
        assert!(error.to_string().contains("while phase tag"), "{error}");
    }

    #[test]
    fn rejects_a_release_that_is_still_a_draft() {
        let (released, evidence, _) = scenario("verify-draft");
        let document = evidence.to_yaml().expect("document");
        let source = FakeReleaseSource::draft(REPOSITORY, "component@1.0.0", 7).with_asset(
            21,
            RELEASE_EVIDENCE_FILE,
            "text/yaml",
            document.as_bytes(),
        );
        let error = verify_release(released.workspace.root(), "1.0.0", false, &source)
            .expect_err("a draft is refused");
        assert!(error.to_string().contains("still a draft"), "{error}");
    }

    #[test]
    fn live_verification_performs_the_public_check_closure_made_possible() {
        let (released, _, source) = scenario("verify-live");
        let verification =
            verify_release(released.workspace.root(), "1.0.0", true, &source).expect("verifies");
        assert!(verification.live);
        assert!(
            verification
                .report()
                .iter()
                .any(|line| line.contains("live public retrieval of component/homebrew/primary")),
            "{:?}",
            verification.report()
        );
    }

    #[test]
    fn live_verification_reports_a_fresh_failure_without_writing_anything() {
        let (released, evidence, source) = scenario("verify-live-failure");
        let document = evidence.to_yaml().expect("document");
        let stripped = source.without_asset("component-1.0.0.tar.gz");
        let error = verify_release(released.workspace.root(), "1.0.0", true, &stripped)
            .expect_err("the fresh failure is reported");
        assert!(
            error
                .to_string()
                .contains("live verification of component/homebrew/primary"),
            "{error}"
        );
        assert_eq!(
            stripped
                .asset_bytes(REPOSITORY, 21)
                .expect("evidence asset"),
            document.as_bytes(),
            "live verification never rewrites the immutable evidence"
        );
        assert!(
            stripped
                .reads()
                .iter()
                .all(|read| read.starts_with("release")
                    || read.starts_with("assets")
                    || read.starts_with("bytes")
                    || read.starts_with("attestation")),
            "the release source served only reads: {:?}",
            stripped.reads()
        );
        let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            stripped.write(RELEASE_EVIDENCE_FILE)
        }));
        assert!(
            refused.is_err(),
            "the release source refuses every attempt to write"
        );
    }

    #[test]
    fn live_verification_reports_a_destination_that_disagrees() {
        let (released, _, source) = scenario("verify-live-disagreement");
        let observer = FakeObserver(PublicationObservation {
            schema: PUBLICATION_OBSERVATION_SCHEMA.to_owned(),
            contract: PUBLICATION_OBSERVATION_CONTRACT.to_owned(),
            release_unit: "component".to_owned(),
            publisher: PublisherKind::Homebrew,
            target: "primary".to_owned(),
            state: ObservationState::Present,
            subject: None,
            packager: None,
            build_provenance: Vec::new(),
            attached_metadata: Vec::new(),
            destination: Some(Destination {
                identity: "example-owner/homebrew-example".to_owned(),
                version: "1.0.0".to_owned(),
                digest: digest_bytes(b"a replaced deliverable"),
            }),
            retrieval: Some(CleanClient {
                mode: CleanClientMode::Public,
                client: "brew".to_owned(),
                version: "4.3.0".to_owned(),
                digest: digest_bytes(b"a replaced deliverable"),
            }),
            destination_aliases: Vec::new(),
            conflict: None,
        });
        let error = verify_release_observed(
            released.workspace.root(),
            "1.0.0",
            true,
            &source,
            Some(&observer),
        )
        .expect_err("the disagreement is reported");
        assert!(error.to_string().contains("live readback"), "{error}");
    }
}
