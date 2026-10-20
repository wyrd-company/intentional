// ---
// relationships:
//   implements: release-planning-and-records
// ---

//! Verification of a baseline tag record.

use super::{check_identity_tags, Identity, Verification};
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
