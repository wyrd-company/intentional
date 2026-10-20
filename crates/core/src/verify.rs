// ---
// relationships:
//   implements: release-planning-and-records
// ---

//! Verification of one established Release from its annotated tag, with Git
//! history only.
//!
//! The tag record names the release commit and the plan digest. Verification
//! finds the commit the release was planned from, recomputes the plan and the
//! materialization there, and checks the digest, the materialized files of the
//! release commit, and the tag records of every identity tag in the plan.

use crate::apply::ApplyResult;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::materialize::Checkout;
use crate::tag::{materialized_release_parent, read_tag_record, recover_deleted_intents};
use crate::tag_record::TagRecord;
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::Path;

/// Tagger name of every Intentional tag object.
const TAGGER_NAME: &str = "Intentional";

/// Tagger email of every Intentional tag object.
const TAGGER_EMAIL: &str = "intentional@wyrd.company";

/// Weight of one verification finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Verification failed.
    Error,
    /// Reported without failing verification.
    Advisory,
}

/// One stable verification finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifyDiagnostic {
    /// Stable diagnostic code.
    pub code: &'static str,
    /// Whether the finding fails verification.
    pub severity: Severity,
    /// Human-readable detail.
    pub message: String,
}

/// The result of verifying one tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Verification {
    /// Tag that was verified.
    pub tag: String,
    /// Whether every verification passed.
    pub verified: bool,
    /// Release commit the tag names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Plan digest the tag record carries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_digest: Option<String>,
    /// Producing Intentional the tag record names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
    /// Identity tags of the Release whose tag records verified.
    pub identity_tags: Vec<String>,
    /// Checks that passed: `tag-record`, `plan-reconstruction`,
    /// `materialized-source`, `identity-tags`.
    pub guarantees: Vec<&'static str>,
    /// Findings in the order they were made.
    pub diagnostics: Vec<VerifyDiagnostic>,
}

impl Verification {
    fn new(tag: &str) -> Self {
        Self {
            tag: tag.to_owned(),
            verified: false,
            target: None,
            plan_digest: None,
            generator: None,
            identity_tags: Vec::new(),
            guarantees: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn error(&mut self, code: &'static str, message: impl Into<String>) {
        self.diagnostics.push(VerifyDiagnostic {
            code,
            severity: Severity::Error,
            message: message.into(),
        });
    }

    fn advisory(&mut self, code: &'static str, message: impl Into<String>) {
        self.diagnostics.push(VerifyDiagnostic {
            code,
            severity: Severity::Advisory,
            message: message.into(),
        });
    }

    /// Verified only when no finding is an error and every check reported its
    /// own pass. A plan reconstruction that differs under another producing
    /// version passes as an advisory.
    fn finish(mut self) -> Self {
        let has = |code: &str| self.guarantees.contains(&code);
        let advised = self
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "producing-version-differs");
        self.verified = !self
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
            && has("tag-record")
            && (has("plan-reconstruction") || advised)
            && has("materialized-source")
            && has("identity-tags");
        self
    }

    /// Serialize as compact JSON.
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|error| Error::Validation(format!("JSON serialization failed: {error}")))
    }
}

/// Verify the Release that annotated tag `tag` identifies. Findings are
/// diagnostics; an error means the repository itself could not be read.
pub fn verify_release(root: &Path, tag: &str) -> Result<Verification> {
    let repository = gix::discover(root)
        .map_err(|error| Error::Git(format!("failed to discover repository: {error}")))?;
    let mut verification = Verification::new(tag);
    let Some((record, target)) = selected_record(&repository, tag, &mut verification)? else {
        return Ok(verification.finish());
    };
    verification.target = Some(target.to_string());
    verification.plan_digest = Some(record.plan_digest.clone());
    verification.generator = Some(record.generator.clone());
    if record.baseline {
        verification.error(
            "baseline-record",
            format!("{tag} establishes a baseline, not a Release; verify takes a release tag"),
        );
        return Ok(verification.finish());
    }
    verification.guarantees.push("tag-record");

    let release = match reconstruct(&repository, target, &record) {
        Ok(release) => release,
        Err(error) => {
            verification.error(
                "reconstruction-failed",
                format!("the release of {tag} cannot be reconstructed: {error}"),
            );
            return Ok(verification.finish());
        }
    };
    check_plan_digest(&release, &record, &mut verification)?;
    check_materialized_tree(&repository, target, &release, &mut verification)?;
    check_identity_tags(
        &repository,
        tag,
        target,
        &record,
        &release,
        &mut verification,
    )?;
    Ok(verification.finish())
}

/// Read the tag record of `tag` and the commit it tags, or record why not.
fn selected_record(
    repository: &gix::Repository,
    tag: &str,
    verification: &mut Verification,
) -> Result<Option<(TagRecord, gix::ObjectId)>> {
    let Some(mut reference) = repository
        .try_find_reference(format!("refs/tags/{tag}").as_str())
        .map_err(|error| Error::Git(format!("failed to inspect tag {tag}: {error}")))?
    else {
        verification.error("tag-missing", format!("tag {tag} does not exist"));
        return Ok(None);
    };
    let Some(object_id) = reference.try_id().map(|id| id.detach()) else {
        verification.error("tag-not-annotated", format!("tag {tag} is symbolic"));
        return Ok(None);
    };
    let target = reference
        .peel_to_id()
        .map_err(|error| Error::Git(format!("failed to peel tag {tag}: {error}")))?
        .detach();
    let object = repository
        .find_object(object_id)
        .map_err(|error| Error::Git(format!("failed to read tag {tag}: {error}")))?;
    let Ok(tag_object) = object.try_into_tag() else {
        verification.error(
            "tag-not-annotated",
            format!("tag {tag} is lightweight; a Release tag is an annotated tag"),
        );
        return Ok(None);
    };
    let decoded = tag_object
        .decode()
        .map_err(|error| Error::Git(format!("failed to decode tag {tag}: {error}")))?;
    let message = decoded.message.to_string();
    if !TagRecord::is_claimed_by(&message) {
        verification.error(
            "tag-record-missing",
            format!("annotated tag {tag} carries no tag record"),
        );
        return Ok(None);
    }
    let record = match TagRecord::decode(&message) {
        Ok(record) => record,
        Err(error) => {
            verification.error(
                "tag-record-invalid",
                format!("annotated tag {tag}: {}", error.in_document(tag.to_owned())),
            );
            return Ok(None);
        }
    };
    if record
        .target
        .as_ref()
        .is_some_and(|recorded| gix::ObjectId::from_hex(recorded.as_bytes()).ok() != Some(target))
    {
        verification.error(
            "tag-record-invalid",
            format!("annotated tag {tag} records a target other than the commit it tags"),
        );
        return Ok(None);
    }
    let tagger = decoded
        .tagger()
        .map_err(|error| Error::Git(format!("failed to decode the tagger of {tag}: {error}")))?;
    if let Some(tagger) = tagger {
        let commit = repository
            .find_commit(target)
            .map_err(|error| Error::Git(format!("failed to read commit {target}: {error}")))?;
        let committer = commit
            .committer()
            .map_err(|error| Error::Git(format!("failed to read the committer: {error}")))?;
        if tagger.name != TAGGER_NAME
            || tagger.email != TAGGER_EMAIL
            || tagger.time != committer.time
        {
            verification.error(
                "tagger-mismatch",
                format!(
                    "tag {tag} is not tagged by {TAGGER_NAME} <{TAGGER_EMAIL}> at the time of the commit it tags"
                ),
            );
            return Ok(None);
        }
    }
    Ok(Some((record, target)))
}

/// The plan and materialization recomputed at the commit the release was
/// planned from.
struct Reconstruction {
    apply: ApplyResult,
    source: gix::ObjectId,
}

fn reconstruct(
    repository: &gix::Repository,
    target: gix::ObjectId,
    record: &TagRecord,
) -> Result<Reconstruction> {
    let release = Checkout::of(repository, target)?;
    let config = Config::load(release.root())?;
    let channel = channel_of(&record.version);
    let source = match channel {
        Some(_) => {
            let versions = release_unit_versions(repository, target, &record.plan_digest)?;
            let release_repository = gix::discover(release.root())
                .map_err(|error| Error::Git(format!("failed to open the checkout: {error}")))?;
            materialized_release_parent(&release_repository, &config, &versions)?.ok_or_else(
                || Error::Validation("the commit that materialized it has no parent".to_owned()),
            )?
        }
        None => recover_deleted_intents(release.root(), &config)?.1,
    };
    let planned = Checkout::of(repository, source)?;
    let apply = ApplyResult::build(planned.root(), channel)?;
    Ok(Reconstruction { apply, source })
}

/// The channel name of a channel version, `<channel>.<iteration>`.
fn channel_of(version: &str) -> Option<&str> {
    let (_, prerelease) = version.split_once('-')?;
    let prerelease = prerelease.split('+').next()?;
    prerelease.split_once('.').map(|(channel, _)| channel)
}

/// Release Unit versions from the tag records at `target` that carry `digest`.
fn release_unit_versions(
    repository: &gix::Repository,
    target: gix::ObjectId,
    digest: &str,
) -> Result<BTreeMap<String, String>> {
    let references = repository
        .references()
        .map_err(|error| Error::Git(format!("failed to read references: {error}")))?;
    let tags = references
        .tags()
        .map_err(|error| Error::Git(format!("failed to read tags: {error}")))?;
    let mut versions = BTreeMap::new();
    for mut reference in tags.flatten() {
        let Ok(peeled) = reference.peel_to_id() else {
            continue;
        };
        if peeled.detach() != target {
            continue;
        }
        let name = reference.name().shorten().to_string();
        let Ok(Some(parsed)) = read_tag_record(repository, &name) else {
            continue;
        };
        let record = parsed.record;
        if record.baseline || record.plan_digest != digest {
            continue;
        }
        if let Some((release_unit, _)) = record
            .tag_id
            .strip_prefix("release-unit/")
            .and_then(|rest| rest.rsplit_once('/'))
        {
            versions.insert(release_unit.to_owned(), record.version.clone());
        }
    }
    if versions.is_empty() {
        return Err(Error::Validation(format!(
            "no Release Unit tag record at {target} carries plan digest {digest}"
        )));
    }
    Ok(versions)
}

fn check_plan_digest(
    release: &Reconstruction,
    record: &TagRecord,
    verification: &mut Verification,
) -> Result<()> {
    let producing = record
        .generator
        .strip_prefix("intentional ")
        .unwrap_or(&record.generator);
    let digests = release.apply.plan.candidate_digests(producing)?;
    if digests.contains(&record.plan_digest) {
        verification.guarantees.push("plan-reconstruction");
    } else if producing == crate::VERSION {
        verification.error(
            "plan-digest-mismatch",
            format!(
                "the plan recomputed at {} does not have the recorded digest {}",
                release.source, record.plan_digest
            ),
        );
    } else {
        verification.advisory(
            "producing-version-differs",
            format!(
                "produced by intentional {producing}; reconstruction with intentional {} differs from the recorded digest {}",
                crate::VERSION,
                record.plan_digest
            ),
        );
    }
    Ok(())
}

fn check_materialized_tree(
    repository: &gix::Repository,
    target: gix::ObjectId,
    release: &Reconstruction,
    verification: &mut Verification,
) -> Result<()> {
    let tree = repository
        .find_commit(target)
        .map_err(|error| Error::Git(format!("failed to read commit {target}: {error}")))?
        .tree()
        .map_err(|error| Error::Git(format!("failed to read the tree of {target}: {error}")))?;
    let mut differing = Vec::new();
    for write in &release.apply.writes {
        if blob_at(&tree, &write.path)?.as_deref() != Some(write.contents.as_bytes()) {
            differing.push(write.path.display().to_string());
        }
    }
    for path in &release.apply.deletes {
        if blob_at(&tree, path)?.is_some() {
            differing.push(path.display().to_string());
        }
    }
    if differing.is_empty() {
        verification.guarantees.push("materialized-source");
    } else {
        verification.error(
            "materialized-tree-mismatch",
            format!(
                "release commit {target} differs from the materialization of its plan at {}",
                differing.join(", ")
            ),
        );
    }
    Ok(())
}

fn blob_at(tree: &gix::Tree<'_>, path: &Path) -> Result<Option<Vec<u8>>> {
    let Some(entry) = tree
        .lookup_entry_by_path(path)
        .map_err(|error| Error::Git(format!("failed to look up {}: {error}", path.display())))?
    else {
        return Ok(None);
    };
    let object = entry
        .object()
        .map_err(|error| Error::Git(format!("failed to read {}: {error}", path.display())))?;
    Ok(Some(object.data.clone()))
}

fn check_identity_tags(
    repository: &gix::Repository,
    tag: &str,
    target: gix::ObjectId,
    record: &TagRecord,
    release: &Reconstruction,
    verification: &mut Verification,
) -> Result<()> {
    let tags = &release.apply.plan.tags;
    let mut failed = false;
    if !tags
        .iter()
        .any(|planned| planned.name == tag && planned.id == record.tag_id)
    {
        failed = true;
        verification.error(
            "identity-tag-mismatch",
            format!(
                "{tag} is not tag {} of the reconstructed Release",
                record.tag_id
            ),
        );
    }
    for planned in tags {
        let parsed = match read_tag_record(repository, &planned.name) {
            Ok(Some(parsed)) => parsed,
            Ok(None) => {
                failed = true;
                let exists = repository
                    .try_find_reference(format!("refs/tags/{}", planned.name).as_str())
                    .map_err(|error| {
                        Error::Git(format!("failed to inspect tag {}: {error}", planned.name))
                    })?
                    .is_some();
                if exists {
                    verification.error(
                        "identity-tag-mismatch",
                        format!("identity tag {} carries no tag record", planned.name),
                    );
                } else {
                    verification.error(
                        "identity-tag-missing",
                        format!("identity tag {} does not exist", planned.name),
                    );
                }
                continue;
            }
            Err(error) => {
                failed = true;
                verification.error("identity-tag-mismatch", error.to_string());
                continue;
            }
        };
        let identity = &parsed.record;
        let disagreements = [
            ("target", parsed.target == target),
            ("tag-id", identity.tag_id == planned.id),
            ("version", identity.version == planned.version),
            ("contract", identity.contract == record.contract),
            ("plan-digest", identity.plan_digest == record.plan_digest),
            ("baseline", !identity.baseline),
        ]
        .into_iter()
        .filter(|(_, agrees)| !agrees)
        .map(|(field, _)| field)
        .collect::<Vec<_>>();
        if disagreements.is_empty() {
            verification.identity_tags.push(planned.name.clone());
        } else {
            failed = true;
            verification.error(
                "identity-tag-mismatch",
                format!(
                    "identity tag {} has unexpected {}",
                    planned.name,
                    disagreements.join(", ")
                ),
            );
        }
    }
    if !failed {
        verification.guarantees.push("identity-tags");
    }
    Ok(())
}
