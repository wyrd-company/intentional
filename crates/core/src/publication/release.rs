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
    CleanClientMode, PhaseTagEvidence, PublisherEvidence, ReleaseEvidence,
    PHASE_TAG_EVIDENCE_SCHEMA, RELEASE_EVIDENCE_CONTRACT, RELEASE_EVIDENCE_FILE,
    RELEASE_EVIDENCE_SCHEMA,
};
use crate::evidence::phase::{decode, PHASE_EVIDENCE_FIELD};
use crate::evidence::{digest_bytes, is_digest, is_git_object};
use crate::model::TagPhase;
use crate::publication::draft::is_draft_dependent;
use crate::publication::observation::{ObservationState, PublicationObservation};
use crate::release::git::GitCommand;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
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
        // The asset name is whatever GitHub reports, so an absolute or
        // parent-relative name would otherwise choose where the staged bytes
        // land rather than staying inside the temporary directory.
        if !crate::evidence::is_flat_name(name) {
            return Err(Error::Validation(format!(
                "Release asset name {name:?} is not a flat name; expected a name carrying no path separator so an attested asset stages inside its temporary directory"
            )));
        }
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
        attestation_from_json(name, &value)
    }
}

/// Where a verified attestation statement carries the attested subject digest.
const SUBJECT_DIGEST_POINTER: &str = "/verificationResult/statement/subject/0/digest/sha256";

/// Where a verified attestation statement carries the workflow run reference.
const INVOCATION_POINTER: &str =
    "/verificationResult/statement/predicate/runDetails/metadata/invocationId";

/// Where a verified attestation statement carries the workflow's repository.
const WORKFLOW_REPOSITORY_POINTER: &str =
    "/verificationResult/statement/predicate/buildDefinition/externalParameters/workflow/repository";

/// Where a verified attestation statement carries the workflow's path.
const WORKFLOW_PATH_POINTER: &str =
    "/verificationResult/statement/predicate/buildDefinition/externalParameters/workflow/path";

/// Read one verified attestation statement into the claim it binds.
fn attestation_from_json(name: &str, value: &serde_json::Value) -> Result<Attestation> {
    let result = value
        .as_array()
        .and_then(|results| results.first())
        .unwrap_or(value);
    let subject_digest = json_str(result, SUBJECT_DIGEST_POINTER)?;
    let invocation = json_str(result, INVOCATION_POINTER)?;
    // Substituting the repository the caller asked about would make the later
    // comparison against the recorded evidence agree with itself, so a missing
    // workflow repository stays missing and is reported.
    let workflow_repository = json_str(result, WORKFLOW_REPOSITORY_POINTER).map_err(|_| {
        Error::Validation(format!(
            "artifact attestation for {name} carries no workflow repository at {WORKFLOW_REPOSITORY_POINTER}; expected the attested workflow's own repository to compare with the recorded evidence"
        ))
    })?;
    Ok(Attestation {
        subject_digest: format!("sha256:{subject_digest}"),
        repository: workflow_repository
            .trim_start_matches("https://github.com/")
            .to_owned(),
        workflow: json_str(result, WORKFLOW_PATH_POINTER).unwrap_or_default(),
        run_id: run_identifier(&invocation)?,
    })
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

/// Post-closure observations a public consumer client left on disk.
///
/// The publication protocol never speaks a registry protocol itself: a
/// credential-bearing, client-specific readback runs where the client is
/// installed and reaches the command as a schema-backed document. Live
/// verification is the same problem after closure, and it has the same answer.
/// A recipe that resolves its release with `brew`, `dnf`, `apt`, or `pacman`
/// writes what that client found, and this observer reads it.
///
/// That is what makes the deferred public path checkable at all. Before closure
/// a draft-dependent publisher cannot claim public retrieval, so its public
/// consumer check is deferred to here; without an observer the only available
/// read is the closed Release asset, which proves the bytes are unchanged and
/// deliberately claims nothing about what an unauthenticated consumer resolves.
///
/// Naming each document by publication identity is the whole binding. An
/// observer that scanned a directory would let one publication's proved
/// retrieval stand in for another's, and `verify_live` compares the identity it
/// receives precisely because a document can be written by anyone.
#[derive(Debug, Clone)]
pub struct ObservedPublications {
    directory: PathBuf,
}

impl ObservedPublications {
    /// Read post-closure observations from one directory.
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    /// File one publication's observation is read from.
    ///
    /// The name is the publication identity with every character outside
    /// `A-Za-z0-9._-` written as `%` and its two uppercase hexadecimal digits,
    /// followed by `.yml`. So `component/homebrew/primary` is read from
    /// `component%2Fhomebrew%2Fprimary.yml`.
    ///
    /// The encoding is reversible, and that is the point rather than tidiness.
    /// This observer exists to keep one publication's proved retrieval from
    /// standing in for another's, and a lossy name defeats it directly: folding
    /// every separator to one character makes `component/homebrew/primary` and a
    /// release unit named `component-homebrew` publishing `primary` resolve to
    /// the same file. `%` is itself encoded, so no two identities can collide.
    ///
    /// The name is derived here rather than read from the document, so a
    /// document written for another publication is read as the publication whose
    /// file it occupies and then reported by the identity comparison.
    pub fn path(&self, identity: &str) -> PathBuf {
        let mut name = String::with_capacity(identity.len() + ".yml".len());
        for byte in identity.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_') {
                name.push(char::from(byte));
            } else {
                name.push_str(&format!("%{byte:02X}"));
            }
        }
        name.push_str(".yml");
        self.directory.join(name)
    }
}

impl DestinationObserver for ObservedPublications {
    fn observe(&self, _repository: &str, fragment: &PublisherEvidence) -> Result<LiveObservation> {
        let identity = fragment.identity();
        let path = self.path(&identity);
        if !path.exists() {
            return Err(Error::Validation(format!(
                "no post-closure observation of {identity} was supplied at {}; a public consumer client writes what it resolved and live verification reads it",
                path.display()
            )));
        }
        PublicationObservation::load(&path)
    }
}

/// The readback a closed GitHub Release is itself sufficient for.
///
/// A draft-dependent publisher consumes its subject from a Release asset, so
/// closure makes one claim checkable without any destination adapter: the
/// closed Release still carries the exact bytes the evidence recorded. The read
/// runs through `gh` under the operator's own credentials, so it proves the
/// asset is present and unchanged and deliberately claims nothing about what an
/// unauthenticated consumer can retrieve. Proving that needs a destination
/// adapter for the publisher, which this readback is not.
struct ReleaseAssetReadback<'a> {
    source: &'a dyn ReleaseSource,
    release_id: u64,
    assets: &'a [ReleaseAsset],
}

impl ReleaseAssetReadback<'_> {
    /// Name the closed Release asset whose bytes carry a fragment's subject.
    fn read(&self, repository: &str, fragment: &PublisherEvidence) -> Result<String> {
        for asset in self.assets {
            if digest_bytes(&self.source.asset_bytes(repository, asset.id)?)
                == fragment.subject.digest
            {
                return Ok(asset.name.clone());
            }
        }
        Err(Error::Validation(format!(
            "authenticated readback of {} found no asset carrying subject digest {} in Release {}",
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
    let repository = crate::publication::origin_identity(root)?;
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
        verify_live(
            &repository,
            &evidence,
            observer,
            &ReleaseAssetReadback {
                source,
                release_id: release.id,
                assets: &assets,
            },
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

/// Render the one global release tag the configuration declares.
fn global_tag_name(config: &Config, version: &str) -> Result<String> {
    let unphased = config.unphased_tags();
    match unphased.as_slice() {
        [tag] => Ok(tag.template.replace("{version}", version)),
        _ => Err(Error::Validation(format!(
            "configuration declares {} workspace tags without an executor phase; exactly one workspace tag is the global release tag",
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
    // The protocol makes R a commit whose sole parent is S. Ancestry alone
    // would accept any descendant of S, so the claim the affirmative entry
    // makes is the one checked here.
    let parents = GitCommand::new(root)
        .args(["rev-list", "--parents", "-n", "1", "--end-of-options"])
        .arg(&release.release_commit)
        .output()?;
    if !parents.succeeded() {
        findings.push(format!(
            "repository cannot read release commit {}: {}",
            release.release_commit,
            parents.diagnostic()
        ));
    } else {
        let observed = parents.line()?;
        let expected = format!("{} {}", release.release_commit, release.source_commit);
        if observed != expected {
            findings.push(format!(
                "repository records release commit and parents {observed:?} but the evidence requires exactly {expected:?}, with source commit {} as the sole parent",
                release.source_commit
            ));
        } else {
            entries.push(format!(
                "identity chain {} -> {} -> {} matches the repository",
                release.source_commit, release.release_commit, release.global_tag.name
            ));
        }
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
    /// Release unit the tag belongs to, absent when it spans the workspace.
    release_unit: Option<String>,
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
                    release_unit: Some(release_unit_id.clone()),
                });
            }
        }
    }
    for tag in config.workspace_tags.values() {
        if let Some(phase) = tag.require_phase {
            tags.push(PhasedTag {
                name: tag.template.replace("{version}", version),
                phase,
                release_unit: None,
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
    for tag in phased_tags(config, version) {
        // A release-unit tag speaks for its own unit's publications only, so a
        // sibling unit's publication is neither missing from its intent nor
        // unintended by it.
        let identities = recorded
            .iter()
            .filter(|(_, fragment)| {
                tag.release_unit
                    .as_ref()
                    .is_none_or(|unit| &fragment.release_unit == unit)
            })
            .map(|(identity, _)| identity.clone())
            .collect::<BTreeSet<_>>();
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
                .filter(|destination| {
                    tag.release_unit
                        .as_ref()
                        .is_none_or(|unit| &destination.release_unit == unit)
                })
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
    // A tag name is read as a literal ref rather than as a pattern: a
    // configured template carrying `*`, `?`, or `[` would otherwise let one
    // tag's sealed evidence be attributed to a tag that never carried it.
    let output = GitCommand::new(root)
        .args(["cat-file", "tag"])
        .arg(format!("refs/tags/{name}"))
        .output()?;
    if !output.succeeded() {
        return Ok(None);
    }
    let contents = output.text()?;
    if contents.trim().is_empty() {
        return Ok(None);
    }
    let Some(encoded) = contents
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
///
/// Without a destination observer the only available check is the closed
/// Release readback, which reaches a draft-dependent publisher's subject and no
/// other publisher's destination. Reporting the rest as unverifiable keeps the
/// report to what was actually proved.
fn verify_live(
    repository: &str,
    evidence: &ReleaseEvidence,
    observer: Option<&dyn DestinationObserver>,
    readback: &ReleaseAssetReadback<'_>,
    findings: &mut Vec<String>,
    entries: &mut Vec<String>,
) {
    // The closed Release proves one claim about a draft-dependent publication
    // without any destination adapter: the asset still carries the subject the
    // evidence recorded. A consumer client proves a different one. Supplying
    // observations asks for the second, never to give up the first, so both run
    // and both are reported.
    verify_live_readback(
        repository,
        evidence,
        readback,
        observer.is_some(),
        findings,
        entries,
    );
    let Some(observer) = observer else {
        return;
    };
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
            // Live verification runs after closure, so what a destination
            // admits then is not always what it admitted during publication. A
            // draft-dependent publisher sealed `authenticated-draft` against a
            // Release that is now published, and its consumer path has become
            // the public one; a destination that serves no anonymous client
            // never had a public path and still does not. Requiring `public` of
            // everything reports the second as a finding for doing exactly what
            // its recipe fixes.
            Some(retrieval) if retrieval.mode != live_mode(fragment.clean_client.mode) => {
                findings.push(format!(
                    "live retrieval of {identity} was performed by a {:?} client, but a published release at this destination is retrieved by a {:?} one",
                    retrieval.mode,
                    live_mode(fragment.clean_client.mode)
                ))
            }
            Some(retrieval) if retrieval.digest != fragment.clean_client.digest => {
                findings.push(format!(
                    "live retrieval of {identity} produced {} but the evidence records {}",
                    retrieval.digest, fragment.clean_client.digest
                ))
            }
            // The mode is reported rather than assumed, for the same reason
            // the check above no longer assumes it: a destination that admits
            // no anonymous read verifies live through its own consumer path,
            // and an entry naming that retrieval "public" describes a check
            // that did not happen.
            Some(retrieval) => entries.push(format!(
                "live {} retrieval of {identity} produced {}",
                retrieval.mode.as_str(),
                retrieval.digest
            )),
        }
    }
}

/// Retrieval mode a published release admits, given what publication recorded.
///
/// Publication happened before the GitHub Release was published, so a
/// draft-dependent destination's sealed `authenticated-draft` describes a
/// window that has closed: its consumer path is public now. A destination that
/// admits no anonymous read is the one whose mode does not change, because
/// nothing about closure gives it one.
const fn live_mode(sealed: CleanClientMode) -> CleanClientMode {
    match sealed {
        CleanClientMode::AuthenticatedRegistry => CleanClientMode::AuthenticatedRegistry,
        CleanClientMode::Public | CleanClientMode::AuthenticatedDraft => CleanClientMode::Public,
    }
}

/// Read every draft-dependent publication back from the closed Release itself.
///
/// The claim this reports is exactly the one the read supports: an
/// authenticated readback of a Release asset. A publisher whose subject does
/// not live in the Release has nothing here to read, so with no destination
/// observer it is reported as unverifiable rather than as either a proved or a
/// broken publication. With one, that publisher's consumer check is the
/// observer's, and reporting it unverifiable here would contradict it.
fn verify_live_readback(
    repository: &str,
    evidence: &ReleaseEvidence,
    readback: &ReleaseAssetReadback<'_>,
    observed: bool,
    findings: &mut Vec<String>,
    entries: &mut Vec<String>,
) {
    for (identity, fragment) in recorded_fragments(evidence) {
        if !is_draft_dependent(fragment.publisher) {
            if !observed {
                findings.push(format!(
                    "live verification of {identity} is unavailable: publisher {} needs a destination observer, and a closed GitHub Release asset reaches only draft-dependent publications",
                    fragment.publisher
                ));
            }
            continue;
        }
        match readback.read(repository, fragment) {
            Err(error) => findings.push(format!("live verification of {identity} failed: {error}")),
            Ok(name) => entries.push(format!(
                "authenticated readback of {identity} found subject {} in Release asset {name} at {}",
                fragment.subject.identity, fragment.subject.digest
            )),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::evidence::assemble::{
        CleanClient, Destination, EvidenceReference, IntendedDestination, PhaseSubject,
        PublisherTargets, ReleaseIdentity, ReleaseUnitEvidence, Subject, TagIdentity,
        WorkflowIdentity, PUBLISHER_EVIDENCE_CONTRACT, PUBLISHER_EVIDENCE_SCHEMA,
    };
    use crate::evidence::assemble::{PackagerRecord, PhaseTagEvidence};
    use crate::evidence::phase::encode;
    use crate::executor::fixture::Workspace;
    use crate::model::PublisherKind;
    use crate::publication::observation::{
        PUBLICATION_OBSERVATION_CONTRACT, PUBLICATION_OBSERVATION_SCHEMA,
    };
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
  release:
    template: 'component@{version}'
release-units:
  component:
    path: component
    homebrew:
      repository: example-owner/homebrew-example
    tags:
      primary:
        role: primary
        template: 'sealed/{id}/{version}'
        require-phase: after-publication
"#;

    /// A workspace whose after-publication tag template carries a glob character.
    const GLOB_TAG_CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-1
github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml }
workspace-tags:
  published:
    template: 'published/*{version}'
    require-phase: after-publication
  release:
    template: 'component@{version}'
release-units:
  component:
    path: component
    homebrew:
      repository: example-owner/homebrew-component
    tags:
      primary:
        role: primary
        template: 'sealed/{id}/{version}'
        require-phase: after-publication
"#;

    /// A workspace whose two release units each seal their own intent.
    const TWO_UNIT_CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
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
    homebrew:
      repository: example-owner/homebrew-component
    tags:
      primary:
        role: primary
        template: 'sealed/{id}/{version}'
        require-phase: before-publication
  library:
    path: library
    homebrew:
      repository: example-owner/homebrew-library
    tags:
      primary:
        role: primary
        template: 'sealed/{id}/{version}'
        require-phase: before-publication
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
        /// Create the after-publication phase tags sealing one evidence document.
        ///
        /// The workspace tag and the release unit's own tag both declare the
        /// after-publication phase, so a released checkout carries both.
        fn seal(&self, phase: &PhaseTagEvidence) {
            self.seal_as("published/1.0.0", phase);
            self.seal_as("sealed/component/1.0.0", phase);
        }

        /// Create one named annotated phase tag carrying sealed phase evidence.
        fn seal_as(&self, name: &str, phase: &PhaseTagEvidence) {
            let encoded = encode(phase).expect("phase evidence");
            git(
                self.workspace.root(),
                &[
                    "tag",
                    "-a",
                    name,
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

    /// How a fixture repository places the release commit relative to its source.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ReleaseShape {
        /// The release commit's sole parent is the source commit.
        SoleParent,
        /// An unrelated commit sits between the source and release commits.
        Interposed,
    }

    /// Build a repository holding S, R, and the global release tag.
    fn released(label: &str) -> Released {
        released_from(
            label,
            CONFIG,
            &["component"],
            "component@1.0.0",
            ReleaseShape::SoleParent,
        )
    }

    /// Build a repository for one configuration, its units, and its tag shape.
    fn released_from(
        label: &str,
        config: &str,
        units: &[&str],
        global_tag: &str,
        shape: ReleaseShape,
    ) -> Released {
        let workspace = Workspace::new(label);
        workspace.write(".intentional/config.yml", config);
        for unit in units {
            workspace
                .write(
                    &format!("{unit}/go.mod"),
                    &format!("module example.test/{unit}\n\ngo 1.22\n"),
                )
                .write(
                    &format!("{unit}/main.go"),
                    "package main\n\nfunc main() {}\n",
                );
        }
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
        if shape == ReleaseShape::Interposed {
            std::fs::write(root.join("NOTES"), "an unrelated change\n").expect("interposed file");
            git(&root, &["add", "-A"]);
            git(&root, &["commit", "--quiet", "-m", "interposed"]);
        }
        for unit in units {
            std::fs::write(root.join(unit).join("RELEASE"), "1.0.0\n").expect("release file");
        }
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "--quiet", "-m", "release"]);
        let release_commit = git(&root, &["rev-parse", "HEAD"]);
        git(
            &root,
            &["tag", "-a", global_tag, "-m", "global release tag"],
        );
        let tag_object = git(&root, &["rev-parse", &format!("refs/tags/{global_tag}")]);
        Released {
            workspace,
            source_commit,
            release_commit,
            tag_object,
        }
    }

    fn fragment(released: &Released) -> PublisherEvidence {
        fragment_for(
            released,
            "component",
            PublisherKind::Homebrew,
            "component@1.0.0",
            DELIVERABLE,
        )
    }

    /// One recorded fragment for a named unit, publisher, and deliverable.
    fn fragment_for(
        released: &Released,
        release_unit: &str,
        publisher: PublisherKind,
        global_tag: &str,
        deliverable: &[u8],
    ) -> PublisherEvidence {
        let digest = digest_bytes(deliverable);
        PublisherEvidence {
            schema: PUBLISHER_EVIDENCE_SCHEMA.to_owned(),
            contract: PUBLISHER_EVIDENCE_CONTRACT.to_owned(),
            release_unit: release_unit.to_owned(),
            publisher,
            target: "primary".to_owned(),
            source_commit: released.source_commit.clone(),
            release_commit: released.release_commit.clone(),
            global_tag: TagIdentity {
                name: global_tag.to_owned(),
                object: released.tag_object.clone(),
                target: released.release_commit.clone(),
            },
            plan_digest: PLAN_DIGEST.to_owned(),
            subject: Subject {
                kind: "homebrew-formula".to_owned(),
                identity: format!("example-{release_unit}"),
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
                identity: format!("example-owner/homebrew-{release_unit}"),
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
        evidence_for(released, "component@1.0.0", vec![fragment])
    }

    /// One evidence document recording every supplied fragment.
    fn evidence_for(
        released: &Released,
        global_tag: &str,
        fragments: Vec<PublisherEvidence>,
    ) -> ReleaseEvidence {
        let mut release_units: BTreeMap<String, ReleaseUnitEvidence> = BTreeMap::new();
        for fragment in fragments {
            release_units
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
                .insert(fragment.target.clone(), fragment);
        }
        ReleaseEvidence {
            schema: RELEASE_EVIDENCE_SCHEMA.to_owned(),
            contract: RELEASE_EVIDENCE_CONTRACT.to_owned(),
            release: ReleaseIdentity {
                source_commit: released.source_commit.clone(),
                release_commit: released.release_commit.clone(),
                global_tag: TagIdentity {
                    name: global_tag.to_owned(),
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
            release_units,
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

    /// The before-publication intent one release unit's phase tag seals.
    fn intent(
        released: &Released,
        global_tag: &str,
        fragment: &PublisherEvidence,
    ) -> PhaseTagEvidence {
        PhaseTagEvidence {
            schema: PHASE_TAG_EVIDENCE_SCHEMA.to_owned(),
            phase: TagPhase::BeforePublication,
            source_commit: released.source_commit.clone(),
            release_commit: released.release_commit.clone(),
            global_tag: global_tag.to_owned(),
            plan_digest: PLAN_DIGEST.to_owned(),
            subjects: vec![PhaseSubject {
                release_unit: fragment.release_unit.clone(),
                identity: fragment.subject.identity.clone(),
                version: fragment.subject.version.clone(),
                digest: fragment.subject.digest.clone(),
                provenance: None,
            }],
            intended_destinations: Some(vec![IntendedDestination {
                release_unit: fragment.release_unit.clone(),
                publisher: fragment.publisher,
                target: fragment.target.clone(),
            }]),
            publisher_evidence: None,
        }
    }

    fn source(evidence: &ReleaseEvidence) -> FakeReleaseSource {
        source_for(evidence, "component@1.0.0")
    }

    /// A closed Release serving one evidence document under a named tag.
    fn source_for(evidence: &ReleaseEvidence, tag: &str) -> FakeReleaseSource {
        let document = evidence.to_yaml().expect("evidence document");
        FakeReleaseSource::draft(REPOSITORY, tag, 7)
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
    fn live_verification_reports_the_closed_release_readback_as_authenticated() {
        let (released, _, source) = scenario("verify-live");
        let verification =
            verify_release(released.workspace.root(), "1.0.0", true, &source).expect("verifies");
        assert!(verification.live);
        let report = verification.report();
        assert!(
            report.iter().any(|line| line.contains(
                "authenticated readback of component/homebrew/primary found subject example-component in Release asset component-1.0.0.tar.gz"
            )),
            "{report:?}"
        );
        assert!(
            !report.iter().any(|line| line.contains("public retrieval")),
            "a credentialed read never reports public retrieval: {report:?}"
        );
    }

    #[test]
    fn the_closed_release_readback_claims_only_the_digest_it_read() {
        let (released, evidence, source) = scenario("verify-live-claims");
        let verification =
            verify_release(released.workspace.root(), "1.0.0", true, &source).expect("verifies");
        let destination = &evidence.release_units["component"].publishers["homebrew"].targets
            ["primary"]
            .destination
            .identity;
        let report = verification.report();
        assert!(
            !report.iter().any(|line| line.contains(destination)),
            "reading a Release asset observes no destination identity: {report:?}"
        );
    }

    #[test]
    fn live_verification_reports_a_publisher_without_an_observer_as_unavailable() {
        let released = released("verify-live-unavailable");
        let fragment = fragment_for(
            &released,
            "component",
            PublisherKind::Npm,
            "component@1.0.0",
            DELIVERABLE,
        );
        released.seal(&phase_evidence(&released, &fragment));
        let evidence = evidence(&released, fragment);
        let source = source(&evidence);
        let error = verify_release(released.workspace.root(), "1.0.0", true, &source)
            .expect_err("the missing observer is reported");
        assert!(
            error.to_string().contains(
                "live verification of component/npm/primary is unavailable: publisher npm needs a destination observer"
            ),
            "{error}"
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
    }

    #[test]
    fn the_release_source_seam_declares_only_read_operations() {
        let declaration = include_str!("release.rs")
            .split_once("pub trait ReleaseSource {")
            .expect("the release source seam is declared in this module")
            .1
            .split_once("\n}\n")
            .expect("the release source seam declaration is closed")
            .0;
        let declared = declaration
            .lines()
            .filter_map(|line| line.trim().strip_prefix("fn "))
            .filter_map(|method| method.split_once('('))
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert_eq!(
            declared,
            ["release", "assets", "asset_bytes", "attestation"],
            "a write operation added to the seam would let verification alter what it observes"
        );
    }

    // Live verification runs after closure, so the mode a destination admits
    // then is not always the mode publication recorded. A draft-dependent
    // publisher's Release is published by now and its consumer path has become
    // the public one; a destination that serves no anonymous client never had a
    // public path and closure does not give it one. Holding every live
    // retrieval to `public` reported the second as a finding for doing exactly
    // what its recipe fixes, which is a finding about the check rather than
    // about the release.
    #[test]
    fn live_verification_holds_each_destination_to_the_mode_it_still_admits() {
        let observed = |sealed: CleanClientMode, live: CleanClientMode| {
            let released = released(&format!("verify-live-mode-{sealed:?}-{live:?}"));
            let mut fragment = fragment_for(
                &released,
                "component",
                PublisherKind::Npm,
                "component@1.0.0",
                DELIVERABLE,
            );
            fragment.clean_client.mode = sealed;
            released.seal(&phase_evidence(&released, &fragment));
            let observer = FakeObserver(PublicationObservation {
                schema: PUBLICATION_OBSERVATION_SCHEMA.to_owned(),
                contract: PUBLICATION_OBSERVATION_CONTRACT.to_owned(),
                release_unit: "component".to_owned(),
                publisher: PublisherKind::Npm,
                target: "primary".to_owned(),
                state: ObservationState::Present,
                subject: None,
                packager: None,
                build_provenance: Vec::new(),
                attached_metadata: Vec::new(),
                destination: Some(fragment.destination.clone()),
                retrieval: Some(CleanClient {
                    mode: live,
                    client: "npm".to_owned(),
                    version: "11.5.1".to_owned(),
                    digest: fragment.clean_client.digest.clone(),
                }),
                destination_aliases: Vec::new(),
                conflict: None,
            });
            let evidence = evidence(&released, fragment);
            let source = source(&evidence);
            verify_release_observed(
                released.workspace.root(),
                "1.0.0",
                true,
                &source,
                Some(&observer),
            )
            .map(|verification| verification.report())
        };

        // A destination that admits no anonymous read keeps the mode it sealed,
        // and does not acquire a public path by being closed.
        let report = observed(
            CleanClientMode::AuthenticatedRegistry,
            CleanClientMode::AuthenticatedRegistry,
        )
        .expect("a registry without anonymous read verifies live under its own mode");
        assert!(
            report
                .iter()
                .any(|line| line.contains("live authenticated-registry retrieval of")),
            "the entry names the mode the check accepted: {report:?}"
        );
        let error = observed(
            CleanClientMode::AuthenticatedRegistry,
            CleanClientMode::Public,
        )
        .expect_err("a public claim at a registry without anonymous read is reported");
        assert!(
            error
                .to_string()
                .contains("was performed by a Public client")
                && error
                    .to_string()
                    .contains("retrieved by a AuthenticatedRegistry one"),
            "{error}"
        );

        // A draft-dependent destination's Release is published by now, so its
        // live retrieval is the public one its sealed mode could not be.
        observed(CleanClientMode::AuthenticatedDraft, CleanClientMode::Public)
            .expect("a draft-dependent publication verifies live through the public path");
        let error = observed(
            CleanClientMode::AuthenticatedDraft,
            CleanClientMode::AuthenticatedDraft,
        )
        .expect_err("a draft claim against a published Release is reported");
        assert!(
            error
                .to_string()
                .contains("was performed by a AuthenticatedDraft client"),
            "{error}"
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

    /// A present public observation of the released Homebrew publication.
    fn public_observation(digest: &str) -> String {
        format!(
            "$schema: {PUBLICATION_OBSERVATION_SCHEMA}
contract: {PUBLICATION_OBSERVATION_CONTRACT}
release-unit: component
publisher: homebrew
target: primary
state: present
subject:
  kind: homebrew-formula
  identity: example-tool
  version: 1.0.0
  digest: {digest}
packager:
  id: goreleaser
  version: 2.4.0
destination:
  identity: example-owner/homebrew-component
  version: 1.0.0
  digest: {digest}
retrieval:
  mode: public
  client: brew
  version: 4.3.0
  digest: {digest}
"
        )
    }

    // A draft-dependent publisher cannot claim public retrieval before closure,
    // so its public consumer check is deferred to live verification. This is
    // that check: the observation a public client wrote after closure, read
    // through the same document contract a repository-local readback uses.
    #[test]
    fn live_verification_proves_the_deferred_public_path_from_a_supplied_observation() {
        let (released, _, source) = scenario("verify-live-observed");
        let directory = released.workspace.root().join("observations");
        let observer = ObservedPublications::new(&directory);
        std::fs::create_dir_all(&directory).expect("observation directory");
        std::fs::write(
            observer.path("component/homebrew/primary"),
            public_observation(&digest_bytes(DELIVERABLE)),
        )
        .expect("observation written");

        let verification = verify_release_observed(
            released.workspace.root(),
            "1.0.0",
            true,
            &source,
            Some(&observer),
        )
        .expect("the deferred public path verifies");
        let report = verification.report().join("\n");
        assert!(
            report.contains("live public retrieval of component/homebrew/primary"),
            "{report}"
        );
    }

    // A publication with no supplied observation is reported rather than
    // silently passing. Live verification that shrugged at an absent document
    // would report a proved public path for a client that never ran.
    #[test]
    fn live_verification_reports_a_publication_no_observation_covers() {
        let (released, _, source) = scenario("verify-live-unobserved");
        let observer = ObservedPublications::new(released.workspace.root().join("observations"));
        let error = verify_release_observed(
            released.workspace.root(),
            "1.0.0",
            true,
            &source,
            Some(&observer),
        )
        .expect_err("an unobserved publication is reported");
        assert!(
            error
                .to_string()
                .contains("no post-closure observation of component/homebrew/primary"),
            "{error}"
        );
    }

    // Supplying observations asks for the consumer check in addition to the
    // Release readback, not instead of it. Withdrawing the readback would make
    // the richer invocation prove strictly less about the publication whose
    // deferred public path the observations exist to cover.
    #[test]
    fn live_verification_adds_the_consumer_check_to_the_release_readback() {
        let (released, _, source) = scenario("verify-live-additive");
        let directory = released.workspace.root().join("observations");
        let observer = ObservedPublications::new(&directory);
        std::fs::create_dir_all(&directory).expect("observation directory");
        std::fs::write(
            observer.path("component/homebrew/primary"),
            public_observation(&digest_bytes(DELIVERABLE)),
        )
        .expect("observation written");

        let verification = verify_release_observed(
            released.workspace.root(),
            "1.0.0",
            true,
            &source,
            Some(&observer),
        )
        .expect("both checks verify");
        let report = verification.report().join("\n");
        assert!(
            report.contains("authenticated readback of component/homebrew/primary"),
            "the closed-Release readback still ran: {report}"
        );
        assert!(
            report.contains("live public retrieval of component/homebrew/primary"),
            "the consumer check ran too: {report}"
        );
    }

    // Two identities that differ only in a separator must not resolve to one
    // file, because this observer's whole job is keeping one publication's
    // proved retrieval from standing in for another's.
    #[test]
    fn distinguishes_identities_that_a_lossy_name_would_collapse() {
        let observer = ObservedPublications::new(Path::new("observations"));
        assert_eq!(
            observer.path("component/homebrew/primary"),
            Path::new("observations/component%2Fhomebrew%2Fprimary.yml")
        );
        assert_ne!(
            observer.path("component/homebrew/primary"),
            observer.path("component-homebrew/primary"),
        );
        assert_ne!(
            observer.path("component%2Fhomebrew/primary"),
            observer.path("component/homebrew/primary"),
        );
    }

    // The file a publication's observation is read from is derived from its
    // identity. Reading whichever document a directory happened to contain
    // would let one publication's proved retrieval stand in for another's.
    #[test]
    fn reads_each_publication_observation_from_its_own_identity() {
        let (released, _, source) = scenario("verify-live-misfiled");
        let directory = released.workspace.root().join("observations");
        let observer = ObservedPublications::new(&directory);
        std::fs::create_dir_all(&directory).expect("observation directory");
        std::fs::write(
            observer.path("component/cargo/primary"),
            public_observation(&digest_bytes(DELIVERABLE)),
        )
        .expect("observation written");
        let error = verify_release_observed(
            released.workspace.root(),
            "1.0.0",
            true,
            &source,
            Some(&observer),
        )
        .expect_err("a misfiled observation does not cover another publication");
        assert!(
            error
                .to_string()
                .contains("no post-closure observation of component/homebrew/primary"),
            "{error}"
        );
    }

    #[test]
    fn rejects_a_release_commit_whose_sole_parent_is_not_the_source_commit() {
        let released = released_from(
            "verify-sole-parent",
            CONFIG,
            &["component"],
            "component@1.0.0",
            ReleaseShape::Interposed,
        );
        let fragment = fragment(&released);
        released.seal(&phase_evidence(&released, &fragment));
        let evidence = evidence(&released, fragment);
        let source = source(&evidence);
        let error = verify_release(released.workspace.root(), "1.0.0", false, &source)
            .expect_err("the interposed commit is refused");
        assert!(error.to_string().contains("as the sole parent"), "{error}");
    }

    #[test]
    fn reports_a_release_commit_the_repository_cannot_read_as_unreadable() {
        let (released, mut evidence, _) = scenario("verify-unreadable-commit");
        evidence.release.release_commit = "2222222222222222222222222222222222222222".to_owned();
        evidence.release.global_tag.target = evidence.release.release_commit.clone();
        let source = source(&evidence);
        let error = verify_release(released.workspace.root(), "1.0.0", false, &source)
            .expect_err("the unreadable commit is refused");
        assert!(
            error
                .to_string()
                .contains("repository cannot read release commit 2222"),
            "{error}"
        );
    }

    #[test]
    fn a_release_unit_phase_tag_intends_only_its_own_units_publications() {
        let released = released_from(
            "verify-two-units",
            TWO_UNIT_CONFIG,
            &["component", "library"],
            "release/1.0.0",
            ReleaseShape::SoleParent,
        );
        let component = fragment_for(
            &released,
            "component",
            PublisherKind::Homebrew,
            "release/1.0.0",
            DELIVERABLE,
        );
        let library = fragment_for(
            &released,
            "library",
            PublisherKind::Homebrew,
            "release/1.0.0",
            b"library deliverable bytes",
        );
        for fragment in [&component, &library] {
            released.seal_as(
                &format!("sealed/{}/1.0.0", fragment.release_unit),
                &intent(&released, "release/1.0.0", fragment),
            );
        }
        let evidence = evidence_for(&released, "release/1.0.0", vec![component, library]);
        let source = source_for(&evidence, "release/1.0.0");
        let verification = verify_release(released.workspace.root(), "1.0.0", false, &source)
            .expect("each unit's intent covers its own publications");
        let report = verification.report();
        for unit in ["component", "library"] {
            assert!(
                report
                    .iter()
                    .any(|line| line.contains(&format!("phase tag sealed/{unit}/1.0.0 agrees"))),
                "{report:?}"
            );
        }
    }

    #[test]
    fn reads_phase_evidence_only_from_the_tag_a_glob_template_names() {
        let released = released_from(
            "verify-glob-tag",
            GLOB_TAG_CONFIG,
            &["component"],
            "component@1.0.0",
            ReleaseShape::SoleParent,
        );
        let fragment = fragment(&released);
        released.seal(&phase_evidence(&released, &fragment));
        let evidence = evidence(&released, fragment);
        let source = source(&evidence);
        let error = verify_release(released.workspace.root(), "1.0.0", false, &source)
            .expect_err("another tag's sealed evidence is not borrowed");
        assert!(
            error.to_string().contains(&format!(
                "repository holds no phase tag published/*1.0.0 carrying a {PHASE_EVIDENCE_FIELD} record"
            )),
            "{error}"
        );
    }

    #[test]
    fn an_attestation_reports_the_workflow_repository_its_statement_carries() {
        let attestation = attestation_from_json(
            RELEASE_EVIDENCE_FILE,
            &attestation_statement(Some("https://github.com/other-owner/other-repository")),
        )
        .expect("the statement is read");
        assert_eq!(attestation.repository, "other-owner/other-repository");
        assert_eq!(attestation.run_id, 42);
    }

    #[test]
    fn an_attestation_carrying_no_workflow_repository_is_reported_rather_than_assumed() {
        let error = attestation_from_json(RELEASE_EVIDENCE_FILE, &attestation_statement(None))
            .expect_err("the missing repository is reported");
        assert!(
            error
                .to_string()
                .contains("carries no workflow repository at"),
            "{error}"
        );
    }

    /// One verified attestation statement, with or without its repository.
    fn attestation_statement(repository: Option<&str>) -> serde_json::Value {
        let mut workflow = serde_json::json!({ "path": ".github/workflows/publish.yml" });
        if let Some(repository) = repository {
            workflow["repository"] = serde_json::Value::String(repository.to_owned());
        }
        serde_json::json!({
            "verificationResult": {
                "statement": {
                    "subject": [{ "digest": { "sha256": "1111111111111111111111111111111111111111111111111111111111111111" } }],
                    "predicate": {
                        "buildDefinition": { "externalParameters": { "workflow": workflow } },
                        "runDetails": {
                            "metadata": {
                                "invocationId": "https://github.com/other-owner/other-repository/actions/runs/42"
                            }
                        }
                    }
                }
            }
        })
    }

    #[test]
    fn an_asset_name_that_is_not_flat_is_refused_before_it_is_staged() {
        let workspace = Workspace::new("attestation-flat-name");
        let error = GhReleaseSource::new(workspace.root())
            .attestation(REPOSITORY, "../escaped-evidence.yml", DELIVERABLE)
            .expect_err("the name is refused");
        assert!(
            error
                .to_string()
                .contains("\"../escaped-evidence.yml\" is not a flat name"),
            "{error}"
        );
    }
}
