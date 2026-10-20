// ---
// relationships:
//   implements: release-planning-and-records
// ---

//! Verification of a baseline tag record.

use super::{check_identity_tags, read_tag, Identity, TagState, Tagger, Verification};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::materialize::Checkout;
use crate::tag::{baseline_versions, tag_candidates};
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
                established_before(&history, target, &config.contract, name, id, version)
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
    check_identity_tags(&history, tag, target, record, &identities, verification)
}

/// Whether tag `name`, the current tag of one stream, establishes that stream
/// before the baseline commit, read through the same reader as every
/// identity tag: a tag record on an earlier commit, under the fixed tagger,
/// whose tag ID, version, and contract agree with the tag it occupies. A tag
/// record at the baseline commit never establishes its stream, so that every
/// tag record there is checked as a baseline tag. Any other tag leaves the
/// stream in the baseline set, where the identity check judges it.
fn established_before(
    history: &gix::Repository,
    baseline_commit: gix::ObjectId,
    contract: &str,
    name: &str,
    id: &str,
    version: &str,
) -> Result<bool> {
    Ok(match read_tag(history, name)? {
        TagState::Recorded {
            record,
            target,
            tagger,
        } => {
            target != baseline_commit
                && tagger == Tagger::Fixed
                && record.tag_id == id
                && record.version == version
                && record.contract == contract
        }
        _ => false,
    })
}
