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
//!
//! A baseline tag record names the commit the baseline was established at.
//! Verification recomputes the baseline set there and checks its digest and
//! the tag records of every tag that establishing the baseline creates.

use crate::apply::ApplyResult;
use crate::config::Config;
use crate::error::{Error, Result};
use crate::materialize::Checkout;
use crate::tag::{
    baseline_versions, materialized_release_parent, recover_deleted_intents, render_tag,
    tag_candidates,
};
use crate::tag_record::baseline_digest;
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
#[non_exhaustive]
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
#[non_exhaustive]
pub struct Verification {
    /// Tag that was verified.
    pub tag: String,
    /// Whether every verification passed.
    pub verified: bool,
    /// Whether the tag record establishes a baseline instead of a Release.
    pub baseline: bool,
    /// Release commit the tag names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Plan digest the tag record carries.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_digest: Option<String>,
    /// Producing Intentional the tag record names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
    /// Identity tags of the Release or baseline whose tag records verified.
    pub identity_tags: Vec<String>,
    /// Checks that passed: `tag-record`, `plan-reconstruction`,
    /// `materialized-source`, and `identity-tags` for a Release;
    /// `tag-record`, `baseline-reconstruction`, and `identity-tags` for a
    /// baseline.
    pub guarantees: Vec<&'static str>,
    /// Findings in the order they were made.
    pub diagnostics: Vec<VerifyDiagnostic>,
}

impl Verification {
    fn new(tag: &str) -> Self {
        Self {
            tag: tag.to_owned(),
            verified: false,
            baseline: false,
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

    /// Verified only when no finding is an error and every check of its kind
    /// reported its own pass. A reconstruction that differs under another
    /// producing version passes as an advisory.
    fn finish(mut self) -> Self {
        let has = |code: &str| self.guarantees.contains(&code);
        let advised = self
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "producing-version-differs");
        let reconstruction = if self.baseline {
            "baseline-reconstruction"
        } else {
            "plan-reconstruction"
        };
        self.verified = !self
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == Severity::Error)
            && has("tag-record")
            && (has(reconstruction) || advised)
            && (self.baseline || has("materialized-source"))
            && has("identity-tags");
        self
    }

    /// Serialize as compact JSON.
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self)
            .map_err(|error| Error::Validation(format!("JSON serialization failed: {error}")))
    }
}

/// Verify the Release or baseline that annotated tag `tag` identifies.
/// Findings are diagnostics; an error means the repository itself could not be
/// read.
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
    verification.baseline = record.baseline;
    verification.guarantees.push("tag-record");
    if record.baseline {
        verify_baseline(&repository, tag, target, &record, &mut verification)?;
        return Ok(verification.finish());
    }

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
    let identities = release
        .apply
        .plan
        .tags
        .iter()
        .map(|planned| Identity {
            id: &planned.id,
            name: &planned.name,
            version: &planned.version,
        })
        .collect::<Vec<_>>();
    check_identity_tags(
        &repository,
        tag,
        target,
        &record,
        &identities,
        &mut verification,
    )?;
    Ok(verification.finish())
}

/// Verify a baseline tag record: recompute the baseline set at the commit it
/// tags, check the digest of its payload, and check the tag record of every
/// tag that establishing that set creates.
fn verify_baseline(
    repository: &gix::Repository,
    tag: &str,
    target: gix::ObjectId,
    record: &TagRecord,
    verification: &mut Verification,
) -> Result<()> {
    let checkout = match Checkout::of(repository, target) {
        Ok(checkout) => checkout,
        Err(error) => {
            verification.error(
                "reconstruction-failed",
                format!("the baseline of {tag} cannot be reconstructed: {error}"),
            );
            return Ok(());
        }
    };
    let reconstructed = Config::load(checkout.root()).and_then(|config| {
        let versions = baseline_versions(checkout.root(), &config, &BTreeMap::new())?;
        Ok((config, versions))
    });
    let (config, versions) = match reconstructed {
        Ok(reconstructed) => reconstructed,
        Err(error) => {
            verification.error(
                "reconstruction-failed",
                format!("the baseline of {tag} cannot be reconstructed: {error}"),
            );
            return Ok(());
        }
    };
    let producing = record
        .generator
        .strip_prefix("intentional ")
        .unwrap_or(&record.generator);
    match baseline_digest(&config.contract, producing, &versions) {
        Ok(digest) if digest == record.plan_digest => {
            verification.guarantees.push("baseline-reconstruction");
        }
        Ok(_) if producing == crate::VERSION => verification.error(
            "baseline-digest-mismatch",
            format!(
                "the baseline set recomputed at {target} does not have the recorded digest {}",
                record.plan_digest
            ),
        ),
        Ok(_) => verification.advisory(
            "producing-version-differs",
            format!(
                "produced by intentional {producing}; the baseline set recomputed with intentional {} differs from the recorded digest {}",
                crate::VERSION,
                record.plan_digest
            ),
        ),
        Err(error) => verification.error(
            "reconstruction-failed",
            format!("the baseline digest of {tag} cannot be recomputed: {error}"),
        ),
    }
    let candidates = tag_candidates(&config, &versions);
    let identities = candidates
        .iter()
        .map(|(id, candidate)| Identity {
            id,
            name: &candidate.name,
            version: &candidate.version,
        })
        .collect::<Vec<_>>();
    // Tags are read in the checkout, which holds only the tags whose target
    // the baseline commit reaches: a tag that marks later history cannot
    // stand in for one the baseline established.
    let history = gix::discover(checkout.root())
        .map_err(|error| Error::Git(format!("failed to open the checkout: {error}")))?;
    check_identity_tags(&history, tag, target, record, &identities, verification)
}

/// Whether a tag object is tagged the way Intentional tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tagger {
    /// The fixed identity at the committer time and offset of the target.
    Fixed,
    /// No tagger header.
    Absent,
    /// Another identity, time, or offset.
    Other,
}

/// What one tag ref holds, read by the same rules for every identity tag.
enum TagState {
    /// No such tag.
    Missing,
    /// Symbolic or lightweight; the detail says which. A lightweight tag
    /// names the commit it tags.
    NotAnnotated {
        kind: &'static str,
        target: Option<gix::ObjectId>,
    },
    /// Annotated without a tag record, and the commit it tags.
    Unclaimed { target: gix::ObjectId },
    /// A tag record that the codec or its target refuses.
    Invalid(String),
    /// A decoded tag record, the commit it tags, and how it is tagged.
    Recorded {
        record: TagRecord,
        target: gix::ObjectId,
        tagger: Tagger,
    },
}

fn read_tag(repository: &gix::Repository, name: &str) -> Result<TagState> {
    let Some(mut reference) = repository
        .try_find_reference(format!("refs/tags/{name}").as_str())
        .map_err(|error| Error::Git(format!("failed to inspect tag {name}: {error}")))?
    else {
        return Ok(TagState::Missing);
    };
    let Some(object_id) = reference.try_id().map(|id| id.detach()) else {
        return Ok(TagState::NotAnnotated {
            kind: "symbolic",
            target: None,
        });
    };
    let target = reference
        .peel_to_id()
        .map_err(|error| Error::Git(format!("failed to peel tag {name}: {error}")))?
        .detach();
    let object = repository
        .find_object(object_id)
        .map_err(|error| Error::Git(format!("failed to read tag {name}: {error}")))?;
    let Ok(tag_object) = object.try_into_tag() else {
        return Ok(TagState::NotAnnotated {
            kind: "lightweight",
            target: Some(target),
        });
    };
    let decoded = tag_object
        .decode()
        .map_err(|error| Error::Git(format!("failed to decode tag {name}: {error}")))?;
    let message = decoded.message.to_string();
    if !TagRecord::is_claimed_by(&message) {
        return Ok(TagState::Unclaimed { target });
    }
    let record = match TagRecord::decode(&message) {
        Ok(record) => record,
        Err(error) => {
            return Ok(TagState::Invalid(format!(
                "annotated tag {name}: {}",
                error.in_document(name.to_owned())
            )))
        }
    };
    if record
        .target
        .as_ref()
        .is_some_and(|recorded| gix::ObjectId::from_hex(recorded.as_bytes()).ok() != Some(target))
    {
        return Ok(TagState::Invalid(format!(
            "annotated tag {name} records a target other than the commit it tags"
        )));
    }
    let tagger = match decoded
        .tagger()
        .map_err(|error| Error::Git(format!("failed to decode the tagger of {name}: {error}")))?
    {
        None => Tagger::Absent,
        Some(tagger) => {
            let commit = repository
                .find_commit(target)
                .map_err(|error| Error::Git(format!("failed to read commit {target}: {error}")))?;
            let committer = commit
                .committer()
                .map_err(|error| Error::Git(format!("failed to read the committer: {error}")))?;
            if tagger.name == TAGGER_NAME
                && tagger.email == TAGGER_EMAIL
                && tagger.time == committer.time
            {
                Tagger::Fixed
            } else {
                Tagger::Other
            }
        }
    };
    Ok(TagState::Recorded {
        record,
        target,
        tagger,
    })
}

/// Read the tag record of `tag` and the commit it tags, or record why not.
fn selected_record(
    repository: &gix::Repository,
    tag: &str,
    verification: &mut Verification,
) -> Result<Option<(TagRecord, gix::ObjectId)>> {
    match read_tag(repository, tag)? {
        TagState::Missing => verification.error("tag-missing", format!("tag {tag} does not exist")),
        TagState::NotAnnotated { kind, .. } => verification.error(
            "tag-not-annotated",
            format!("tag {tag} is {kind}; a Release tag is an annotated tag"),
        ),
        TagState::Unclaimed { .. } => verification.error(
            "tag-record-missing",
            format!("annotated tag {tag} carries no tag record"),
        ),
        TagState::Invalid(detail) => verification.error("tag-record-invalid", detail),
        TagState::Recorded {
            tagger: Tagger::Absent,
            ..
        } => verification.error(
            "tagger-mismatch",
            format!("tag {tag} has no tagger; Intentional tags as {TAGGER_NAME} <{TAGGER_EMAIL}>"),
        ),
        TagState::Recorded {
            tagger: Tagger::Other,
            ..
        } => verification.error(
            "tagger-mismatch",
            format!(
                "tag {tag} is not tagged by {TAGGER_NAME} <{TAGGER_EMAIL}> at the time of the commit it tags"
            ),
        ),
        TagState::Recorded {
            record,
            target,
            tagger: Tagger::Fixed,
        } => return Ok(Some((record, target))),
    }
    Ok(None)
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
            let versions = release_unit_versions(repository, &config, target, &record.plan_digest)?;
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
/// A record counts only when its tag ID names a configured Release Unit tag
/// whose template renders the tag's own name at the recorded version, and
/// when Intentional tagged it; any other tag at the commit is ignored.
fn release_unit_versions(
    repository: &gix::Repository,
    config: &Config,
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
        let TagState::Recorded {
            record,
            tagger: Tagger::Fixed,
            ..
        } = read_tag(repository, &name)?
        else {
            continue;
        };
        if record.baseline || record.plan_digest != digest {
            continue;
        }
        let Some((release_unit, tag_id)) = record
            .tag_id
            .strip_prefix("release-unit/")
            .and_then(|rest| rest.rsplit_once('/'))
        else {
            continue;
        };
        let configured = config
            .release_units
            .get(release_unit)
            .and_then(|unit| unit.tags.get(tag_id));
        if configured
            .is_some_and(|tag| render_tag(&tag.template, release_unit, &record.version) == name)
        {
            versions.insert(release_unit.to_owned(), record.version.clone());
        }
    }
    if versions.is_empty() {
        return Err(Error::Validation(format!(
            "no configured Release Unit tag at {target} carries plan digest {digest}"
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

/// One tag that the verified Release or baseline names.
struct Identity<'a> {
    id: &'a str,
    name: &'a str,
    version: &'a str,
}

/// Check that `tag` is one of `identities` and that every identity tag carries
/// a tag record that agrees with `record`, at `target`, under the fixed
/// tagger. For a baseline, a tag without a tag record on an earlier commit is
/// pre-Intentional history that supplies its version, which establishing the
/// baseline leaves in place.
fn check_identity_tags(
    repository: &gix::Repository,
    tag: &str,
    target: gix::ObjectId,
    record: &TagRecord,
    identities: &[Identity<'_>],
    verification: &mut Verification,
) -> Result<()> {
    let kind = if record.baseline {
        "baseline"
    } else {
        "Release"
    };
    let mut failed = false;
    if !identities
        .iter()
        .any(|identity| identity.name == tag && identity.id == record.tag_id)
    {
        failed = true;
        verification.error(
            "identity-tag-mismatch",
            format!(
                "{tag} is not tag {} of the reconstructed {kind}",
                record.tag_id
            ),
        );
    }
    for expected in identities {
        let (identity, identity_target, tagger) = match read_tag(repository, expected.name)? {
            TagState::Recorded {
                record,
                target,
                tagger,
            } => (record, target, tagger),
            TagState::Missing => {
                failed = true;
                verification.error(
                    "identity-tag-missing",
                    if record.baseline {
                        format!(
                            "identity tag {} does not exist at {target} or in its history",
                            expected.name
                        )
                    } else {
                        format!("identity tag {} does not exist", expected.name)
                    },
                );
                continue;
            }
            TagState::NotAnnotated {
                target: Some(history),
                ..
            }
            | TagState::Unclaimed { target: history }
                if record.baseline && history != target =>
            {
                continue;
            }
            TagState::NotAnnotated { .. } | TagState::Unclaimed { .. } => {
                failed = true;
                verification.error(
                    "identity-tag-mismatch",
                    format!("identity tag {} carries no tag record", expected.name),
                );
                continue;
            }
            TagState::Invalid(detail) => {
                failed = true;
                verification.error("identity-tag-mismatch", detail);
                continue;
            }
        };
        let disagreements = [
            ("target", identity_target == target),
            ("tagger", tagger == Tagger::Fixed),
            ("tag-id", identity.tag_id == expected.id),
            ("version", identity.version == expected.version),
            ("contract", identity.contract == record.contract),
            ("plan-digest", identity.plan_digest == record.plan_digest),
            ("baseline", identity.baseline == record.baseline),
        ]
        .into_iter()
        .filter(|(_, agrees)| !agrees)
        .map(|(field, _)| field)
        .collect::<Vec<_>>();
        if disagreements.is_empty() {
            verification.identity_tags.push(expected.name.to_owned());
        } else {
            failed = true;
            verification.error(
                "identity-tag-mismatch",
                format!(
                    "identity tag {} has unexpected {}",
                    expected.name,
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

#[cfg(test)]
mod tests {
    use super::Verification;
    use std::collections::BTreeSet;

    #[test]
    fn a_verification_without_every_check_passed_is_not_verified() {
        const ALL: [&str; 4] = [
            "tag-record",
            "plan-reconstruction",
            "materialized-source",
            "identity-tags",
        ];
        assert!(!Verification::new("sample").finish().verified);
        let mut complete = Verification::new("sample");
        complete.guarantees = ALL.to_vec();
        assert!(complete.finish().verified);
        for missing in ALL {
            let mut partial = Verification::new("sample");
            partial.guarantees = ALL.into_iter().filter(|check| *check != missing).collect();
            assert!(!partial.finish().verified, "{missing}");
        }
        let mut advised = Verification::new("sample");
        advised.guarantees = ALL
            .into_iter()
            .filter(|check| *check != "plan-reconstruction")
            .collect();
        advised.advisory("producing-version-differs", "sample");
        assert!(advised.finish().verified);
    }

    #[test]
    fn a_baseline_verification_without_every_baseline_check_passed_is_not_verified() {
        const ALL: [&str; 3] = ["tag-record", "baseline-reconstruction", "identity-tags"];
        let baseline = |guarantees: Vec<&'static str>| {
            let mut verification = Verification::new("sample");
            verification.baseline = true;
            verification.guarantees = guarantees;
            verification
        };
        assert!(baseline(ALL.to_vec()).finish().verified);
        for missing in ALL {
            let partial = baseline(ALL.into_iter().filter(|check| *check != missing).collect());
            assert!(!partial.finish().verified, "{missing}");
        }
        let release = baseline(vec!["tag-record", "plan-reconstruction", "identity-tags"]);
        assert!(!release.finish().verified);
        let mut advised = baseline(vec!["tag-record", "identity-tags"]);
        advised.advisory("producing-version-differs", "sample");
        assert!(advised.finish().verified);
    }

    /// Diagnostic codes this module passes to `error` or `advisory`.
    fn emitted_codes() -> BTreeSet<String> {
        let source = include_str!("verify.rs");
        let source = &source[..source.find("#[cfg(test)]").expect("test module")];
        let mut codes = BTreeSet::new();
        for call in [".error(", ".advisory("] {
            for (index, _) in source.match_indices(call) {
                let rest = source[index + call.len()..].trim_start();
                if let Some(code) = rest
                    .strip_prefix('"')
                    .and_then(|rest| rest.split_once('"'))
                    .map(|(code, _)| code)
                {
                    codes.insert(code.to_owned());
                }
            }
        }
        codes
    }

    #[test]
    fn the_cli_specification_names_every_diagnostic_code() {
        let specification: serde_yaml::Value = serde_yaml::from_str(include_str!(
            "../../../docs/specifications/command-line-interface.spec.yml"
        ))
        .expect("CLI specification parses");
        let verify = specification["commands"]
            .as_sequence()
            .expect("commands")
            .iter()
            .find(|command| command["name"] == "verify")
            .expect("verify command");
        let notes = verify["notes"]
            .as_sequence()
            .expect("notes")
            .iter()
            .filter_map(serde_yaml::Value::as_str)
            .collect::<Vec<_>>()
            .join(" ");
        let named = notes
            .split('`')
            .skip(1)
            .step_by(2)
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let emitted = emitted_codes();
        assert!(emitted.len() >= 12, "{emitted:?}");
        assert_eq!(named, emitted);
    }
}
