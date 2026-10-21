// ---
// relationships:
//   implements: release-planning-and-records
// ---

//! Verification of a baseline tag record.

use super::{
    check_identity_tags, is_release_tag, producing_version, read_tag, Agreement, Identity,
    TagState, Tagger, Verification,
};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::materialize::Checkout;
use crate::tag::{baseline_versions, render_tag, tag_candidates, verify_version_projections};
use crate::tag_record::{baseline_digest, TagRecord};
use std::collections::BTreeMap;

/// Verify a baseline tag record: recompute the baseline set at the commit it
/// tags, check the digest of its payload, and check the tag record of every
/// tag that establishing that set creates.
pub(super) fn verify(
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
    // Every tag is read in the checkout, which holds only the tags whose
    // target the baseline commit reaches: a tag that marks later history
    // cannot stand in for one the baseline established.
    let history = gix::discover(checkout.root())
        .map_err(|error| Error::Git(format!("failed to open the checkout: {error}")))?;
    let reconstructed = Config::load(checkout.root()).and_then(|config| {
        let versions = baseline_versions(
            checkout.root(),
            &config,
            &BTreeMap::new(),
            &|name, id, version| {
                established_before(
                    repository,
                    &history,
                    target,
                    &config.contract,
                    name,
                    id,
                    version,
                )
            },
        )?;
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
    let producing = producing_version(record);
    match baseline_digest(&config.contract, producing, &versions)
        .map(|digest| Agreement::of(digest == record.plan_digest, producing))
    {
        Ok(Agreement::Equal) => {
            verification.guarantees.push("baseline-reconstruction");
        }
        Ok(Agreement::Differs) => verification.error(
            "baseline-digest-mismatch",
            format!(
                "the baseline set recomputed at {target} does not have the recorded digest {}",
                record.plan_digest
            ),
        ),
        Ok(Agreement::DiffersUnderAnotherVersion) => verification.advisory(
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
    check_projections(
        &history,
        checkout.root(),
        target,
        &config,
        &versions,
        verification,
    )?;
    let candidates = tag_candidates(&config, &versions);
    let identities = candidates
        .iter()
        .map(|(id, candidate)| Identity {
            id,
            name: &candidate.name,
            version: &candidate.version,
        })
        .collect::<Vec<_>>();
    check_identity_tags(&history, tag, target, record, &identities, verification)
}

/// Whether tag `name`, the current tag of one stream, establishes that stream
/// outside the baseline, as `tag --baseline` decides, read through the same
/// reader as every identity tag. The tag must carry a tag record under the
/// fixed tagger whose tag ID, version, and contract agree with the tag it
/// occupies, and be one of:
///
/// - a tag record on an earlier commit;
/// - a non-baseline tag record at the baseline commit that is an identity tag
///   of a Release reconstructing there.
///
/// A baseline tag record at the baseline commit is a tag of this baseline.
/// Any other tag leaves the stream in the baseline set, where the identity
/// check judges it.
fn established_before(
    repository: &gix::Repository,
    history: &gix::Repository,
    baseline_commit: gix::ObjectId,
    contract: &str,
    name: &str,
    id: &str,
    version: &str,
) -> Result<bool> {
    let TagState::Recorded {
        record,
        target,
        tagger,
    } = read_tag(history, name)?
    else {
        return Ok(false);
    };
    let agrees = tagger == Tagger::Fixed
        && record.tag_id == id
        && record.version == version
        && record.contract == contract;
    Ok(agrees
        && (target != baseline_commit
            || (!record.baseline && is_release_tag(repository, target, &record, name))))
}

/// Apply the projection rule of `tag --baseline` at the baseline commit: a
/// Release Unit whose primary tag there carries a baseline tag record has
/// every version-bearing projection at its baseline version. A unit whose
/// version a tag without a tag record supplies is skipped, as baseline
/// creation does not read its projections.
fn check_projections(
    history: &gix::Repository,
    root: &std::path::Path,
    baseline_commit: gix::ObjectId,
    config: &Config,
    versions: &BTreeMap<String, String>,
    verification: &mut Verification,
) -> Result<()> {
    let mut created = BTreeMap::new();
    for (id, version) in versions {
        let Ok((_, primary)) = config.primary_tag(id) else {
            continue;
        };
        if let TagState::Recorded { record, target, .. } =
            read_tag(history, &render_tag(&primary.template, id, version))?
        {
            if record.baseline && target == baseline_commit {
                created.insert(id.clone(), version.clone());
            }
        }
    }
    if let Err(error) = verify_version_projections(root, config, &created) {
        verification.error(
            "baseline-projection-mismatch",
            format!("the baseline commit {baseline_commit} disagrees with its baseline version: {error}"),
        );
    }
    Ok(())
}
