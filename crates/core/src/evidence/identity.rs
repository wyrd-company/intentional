// ---
// relationships:
//   implements: github-release-executor
// ---

//! The release identity final assembly binds its evidence to.
//!
//! Assembly proves the checkout it was given rather than trusting it or being
//! handed a document about it. `verify release-tag` resolves the annotated
//! global tag the checked-out commit publishes, rebuilds the release from the
//! accepted source commit in an isolated local clone, and refuses the checkout
//! unless the rebuilt plan digest, tree, commit, tag object and tag name all
//! match what the published tag carries.
//!
//! That is what makes the checkout usable evidence. The rule assembly is held
//! to was never "do not read the checkout", it was "do not infer identity from
//! the checkout without proving it is R", and this proves it. Every identity
//! and the whole sealed plan then come from one derivation performed in the job
//! that consumes them, with no credential, no network access, and no document
//! transported from another run.

use crate::error::Result;
use crate::evidence::assemble::{ReleaseIdentity, TagIdentity};
use crate::plan::ReleasePlan;
use crate::release::tag::verify_release_tag;
use std::path::Path;

/// What the proved checkout tells assembly about the release it is closing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvedRelease {
    /// Release identity every accepted document must agree with.
    pub identity: ReleaseIdentity,
    /// Sealed release plan the reproduction rebuilt and the tag seals.
    pub plan: ReleasePlan,
}

/// Prove the checkout is the release commit and read the release from it.
///
/// The tag's target is not read separately: reproduction refuses a global tag
/// that targets anything but the released commit, so the target is R and
/// recording it as anything else would be recording a value nothing proved.
pub fn proved_release(root: &Path) -> Result<ProvedRelease> {
    let verified = verify_release_tag(root)?;
    Ok(ProvedRelease {
        identity: ReleaseIdentity {
            source_commit: verified.source,
            release_commit: verified.release.clone(),
            global_tag: TagIdentity {
                name: verified.global_tag,
                object: verified.global_tag_object,
                target: verified.release,
            },
            plan_digest: verified.plan_digest,
        },
        plan: verified.plan,
    })
}

/// Report each named component two documents spell differently.
///
/// The comparison is component-wise rather than whole-struct so a document that
/// carries the right commits and the wrong tag object names the tag object, and
/// so each component can fail on its own.
fn disagreements(label: &str, components: &[(&str, &str, &str)]) -> Vec<String> {
    components
        .iter()
        .filter(|(_, expected, observed)| expected != observed)
        .map(|(field, expected, observed)| {
            format!(
                "{label} records {field} {observed:?}, but the proved release identifies \
                 the release by {field} {expected:?}"
            )
        })
        .collect()
}

/// Every identity component one publisher fragment carries.
pub(crate) fn fragment_disagreements(
    label: &str,
    expected: &ReleaseIdentity,
    observed: &ReleaseIdentity,
) -> Vec<String> {
    disagreements(
        label,
        &[
            (
                "source-commit",
                &expected.source_commit,
                &observed.source_commit,
            ),
            (
                "release-commit",
                &expected.release_commit,
                &observed.release_commit,
            ),
            (
                "global-tag.name",
                &expected.global_tag.name,
                &observed.global_tag.name,
            ),
            (
                "global-tag.object",
                &expected.global_tag.object,
                &observed.global_tag.object,
            ),
            (
                "global-tag.target",
                &expected.global_tag.target,
                &observed.global_tag.target,
            ),
            ("plan-digest", &expected.plan_digest, &observed.plan_digest),
        ],
    )
}

/// Every identity component one phase-tag document carries.
///
/// A phase document records the global tag by name and never carries the tag
/// object or its target, so four components are compared rather than six. The
/// two it cannot carry are bound to this identity by the publisher fragments,
/// not here and not by the sealed plan, which seals a tag's name and not the
/// annotated object a later push creates. The caller's prose must not claim
/// otherwise.
pub(crate) fn phase_disagreements(
    label: &str,
    expected: &ReleaseIdentity,
    source_commit: &str,
    release_commit: &str,
    global_tag: &str,
    plan_digest: &str,
) -> Vec<String> {
    disagreements(
        label,
        &[
            ("source-commit", &expected.source_commit, source_commit),
            ("release-commit", &expected.release_commit, release_commit),
            ("global-tag.name", &expected.global_tag.name, global_tag),
            ("plan-digest", &expected.plan_digest, plan_digest),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release::tag::tests::ReleasedWorkspace;

    /// The one release every evidence test binds its documents to.
    ///
    /// Assembly, the fragments it accepts, and the phase documents it compares
    /// all have to spell one release the same way. Every evidence test
    /// therefore reads its identities from here rather than from a constant of
    /// its own, so a fixture cannot agree with itself while disagreeing with
    /// the released checkout assembly proves.
    fn identity(workspace: &ReleasedWorkspace) -> ReleaseIdentity {
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
    }

    #[test]
    fn reads_the_release_from_the_checkout_it_proves() {
        let workspace = ReleasedWorkspace::new();
        let proved = proved_release(&workspace.root).expect("the checkout is the release");
        assert_eq!(proved.identity, identity(&workspace));
        assert_eq!(proved.plan.digest, workspace.plan_digest);
    }

    /// A checkout that is not the released commit is refused, not read.
    ///
    /// This is the state the whole binding exists for: a job checked out at a
    /// ref that is not R. Assembly refuses it rather than deriving an
    /// expectation from it.
    #[test]
    fn refuses_a_checkout_that_is_not_the_released_commit() {
        let workspace = ReleasedWorkspace::new();
        let error = proved_release(&workspace.root.join(".intentional"))
            .expect_err("a directory that is not the released checkout is refused");
        assert!(!error.to_string().is_empty(), "{error}");
    }

    /// Each identity component is compared, and each is compared on its own.
    ///
    /// The battery corrupts one component at a time, so a comparison that
    /// stopped reading any single component fails here rather than passing
    /// because the components beside it still disagreed.
    #[test]
    fn names_each_identity_component_a_fragment_spells_differently() {
        let expected = identity(&ReleasedWorkspace::new());
        let elsewhere = "9".repeat(40);
        /// One named component of an identity, and how to spell it wrongly.
        type Corruption = (&'static str, fn(&mut ReleaseIdentity, &str));
        let corruptions: [Corruption; 6] = [
            ("source-commit", |identity, value| {
                identity.source_commit = value.to_owned();
            }),
            ("release-commit", |identity, value| {
                identity.release_commit = value.to_owned();
            }),
            ("global-tag.name", |identity, _| {
                identity.global_tag.name = "release/9.9.9".to_owned();
            }),
            ("global-tag.object", |identity, value| {
                identity.global_tag.object = value.to_owned();
            }),
            ("global-tag.target", |identity, value| {
                identity.global_tag.target = value.to_owned();
            }),
            ("plan-digest", |identity, _| {
                identity.plan_digest = format!("sha256:{}", "8".repeat(64));
            }),
        ];
        for (field, corrupt) in corruptions {
            let mut observed = expected.clone();
            corrupt(&mut observed, &elsewhere);
            let findings = fragment_disagreements("fragment.yml", &expected, &observed);
            assert_eq!(
                findings.len(),
                1,
                "corrupting {field} alone reports {field} alone: {findings:?}"
            );
            assert!(
                findings[0].contains(field),
                "the {field} finding names {field}: {findings:?}"
            );
        }
        assert!(
            fragment_disagreements("fragment.yml", &expected, &expected).is_empty(),
            "an agreeing document reports nothing"
        );
    }

    /// The same battery over the four components a phase document carries.
    ///
    /// A phase document is compared by a shorter list than a fragment, so its
    /// components need a battery of their own: a comparison that stopped
    /// reading one of them would still be caught by the fragment battery on the
    /// components the two lists share, and never on the ones they do not.
    #[test]
    fn names_each_identity_component_a_phase_document_spells_differently() {
        let expected = identity(&ReleasedWorkspace::new());
        /// One named component of a phase document, and how to spell it wrongly.
        type Corruption = (&'static str, fn(&mut [String; 4]));
        let corruptions: [Corruption; 4] = [
            ("source-commit", |observed| {
                observed[0] = "9".repeat(40);
            }),
            ("release-commit", |observed| {
                observed[1] = "9".repeat(40);
            }),
            ("global-tag.name", |observed| {
                observed[2] = "release/9.9.9".to_owned();
            }),
            ("plan-digest", |observed| {
                observed[3] = format!("sha256:{}", "8".repeat(64));
            }),
        ];
        let agreeing = [
            expected.source_commit.clone(),
            expected.release_commit.clone(),
            expected.global_tag.name.clone(),
            expected.plan_digest.clone(),
        ];
        for (field, corrupt) in corruptions {
            let mut observed = agreeing.clone();
            corrupt(&mut observed);
            let findings = phase_disagreements(
                "phase.yml",
                &expected,
                &observed[0],
                &observed[1],
                &observed[2],
                &observed[3],
            );
            assert_eq!(
                findings.len(),
                1,
                "corrupting {field} alone reports {field} alone: {findings:?}"
            );
            assert!(
                findings[0].contains(field),
                "the {field} finding names {field}: {findings:?}"
            );
        }
        assert!(
            phase_disagreements(
                "phase.yml",
                &expected,
                &agreeing[0],
                &agreeing[1],
                &agreeing[2],
                &agreeing[3],
            )
            .is_empty(),
            "an agreeing document reports nothing"
        );
    }
}
