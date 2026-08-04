// ---
// relationships:
//   implements: github-release-executor
// ---

//! The release identity final assembly binds its evidence to.
//!
//! Assembly is handed the prepared release-candidate handoff and reads the
//! release identity from it. The handoff is the artifact the privileged
//! publication job verified before it pushed the release commit and the
//! annotated global tag, so it is the one input to assembly that states which
//! release is being closed without asking the assembling job's own checkout.

use crate::error::{Error, Result};
use crate::evidence::assemble::{ReleaseIdentity, TagIdentity};
use crate::evidence::digest_bytes;
use crate::plan::ReleasePlan;
use crate::release::candidate::{ReleaseCandidate, RELEASE_CANDIDATE_MANIFEST};
use std::path::Path;

/// What one prepared release-candidate handoff tells assembly about its release.
///
/// Named for the handoff rather than for the release, because preparation has
/// its own `PreparedRelease` describing what it wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedHandoff {
    /// Release identity every accepted document must agree with.
    pub identity: ReleaseIdentity,
    /// Sealed release plan the handoff transports.
    pub plan: ReleasePlan,
}

/// Read the release the prepared candidate handoff was prepared for.
///
/// The manifest is parsed through its own closed validation rather than picked
/// apart field by field, so a handoff that is internally inconsistent is
/// refused here instead of supplying assembly with half an identity.
///
/// The manifest's claims about the release are then held to the plan the
/// handoff actually transports, in steps that fail independently: the
/// transported bytes are the ones the manifest inventoried; the plan's own seal
/// still recomputes over its payload; the digest that recomputation produces is
/// the one the manifest claims; and the global tag the manifest names is a tag
/// that plan seals, under the name it seals it by.
///
/// The digest comparison reads `payload_digest` rather than the seal the plan
/// carries, so it is a derivation set against a claim at the point of use. That
/// keeps it non-circular on its own rather than by standing after the seal
/// check, which a later edit could move or drop without anything noticing.
pub fn prepared_release(directory: &Path) -> Result<PreparedHandoff> {
    let manifest = directory.join(RELEASE_CANDIDATE_MANIFEST);
    let text = std::fs::read_to_string(&manifest).map_err(|error| Error::io(&manifest, error))?;
    let candidate = ReleaseCandidate::from_yaml(&text).map_err(|error| {
        Error::Validation(format!(
            "release candidate {} is not a usable handoff: {error}",
            manifest.display()
        ))
    })?;

    let plan_path = directory.join(&candidate.plan.file);
    let bytes = std::fs::read(&plan_path).map_err(|error| Error::io(&plan_path, error))?;
    let transported = digest_bytes(&bytes);
    if transported != candidate.plan.sha256 {
        return Err(Error::Validation(format!(
            "the handoff transports {} as {transported}, which its manifest inventories as {}",
            candidate.plan.file, candidate.plan.sha256
        )));
    }
    let plan: ReleasePlan = serde_json::from_slice(&bytes).map_err(|error| {
        Error::Validation(format!(
            "the handoff's sealed release plan {} is not a release plan: {error}",
            plan_path.display()
        ))
    })?;
    plan.verify_digest()?;
    let sealed = plan.payload_digest()?;
    if sealed != candidate.plan.digest {
        return Err(Error::Validation(format!(
            "the release candidate identifies the release by plan-digest {}, but the sealed \
             release plan it transports seals {sealed}",
            candidate.plan.digest
        )));
    }

    // The plan seals the tags the release creates, and the manifest names one
    // of them as the global release tag. Reading the plan's own entry is what
    // stops a manifest and a unanimous set of fragments from agreeing about a
    // tag the release never sealed.
    let tag = plan
        .tags
        .iter()
        .find(|tag| tag.id == candidate.global_tag.id)
        .ok_or_else(|| {
            Error::Validation(format!(
                "the release candidate names global release tag {}, which the sealed release \
                 plan does not seal; it seals {}",
                candidate.global_tag.id,
                plan.tags
                    .iter()
                    .map(|tag| tag.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
    if tag.name != candidate.global_tag.name {
        return Err(Error::Validation(format!(
            "the release candidate renders global release tag {} as {}, and the sealed release \
             plan seals it as {}",
            candidate.global_tag.id, candidate.global_tag.name, tag.name
        )));
    }

    Ok(PreparedHandoff {
        identity: ReleaseIdentity {
            source_commit: candidate.source.commit,
            release_commit: candidate.release.commit,
            global_tag: TagIdentity {
                name: candidate.global_tag.name,
                object: candidate.global_tag.object,
                target: candidate.global_tag.target,
            },
            plan_digest: candidate.plan.digest,
        },
        plan,
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
                "{label} records {field} {observed:?}, but the prepared release candidate \
                 identifies the release by {field} {expected:?}"
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
/// two it cannot carry are bound to this identity by the publisher fragments
/// and by the sealed plan, not here, and the caller's prose must not claim
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

/// The one prepared handoff every evidence test binds its documents to.
///
/// Assembly, the fragments it accepts, and the phase documents it compares all
/// have to spell one release the same way. Every evidence test therefore reads
/// its identities from here rather than from a constant of its own, so a
/// fixture cannot agree with itself while disagreeing with the handoff.
#[cfg(test)]
pub(crate) mod test_support {
    use crate::evidence::digest_bytes;
    use crate::executor::fixture::Workspace;
    use crate::model::{Bump, TagPhase, TagRole};
    use crate::plan::{Generator, PlanReleaseUnit, PlanTag, ReleasePlan};
    use crate::release::candidate::{
        RELEASE_BUNDLE_FILE, RELEASE_CANDIDATE_CONTRACT, RELEASE_CANDIDATE_MANIFEST,
        RELEASE_CANDIDATE_SCHEMA, RELEASE_PLAN_FILE,
    };
    use std::path::PathBuf;

    /// Source commit S.
    pub(crate) const SOURCE: &str = "1111111111111111111111111111111111111111";
    /// Release commit R.
    pub(crate) const RELEASE: &str = "2222222222222222222222222222222222222222";
    /// Annotated global tag object.
    pub(crate) const TAG_OBJECT: &str = "3333333333333333333333333333333333333333";
    /// Rendered global tag name.
    pub(crate) const TAG_NAME: &str = "release/1.0.0";
    /// Release unit the fixture releases and publishes.
    pub(crate) const RELEASE_UNIT: &str = "component";

    /// The sealed release plan the fixture handoff transports.
    ///
    /// Assembly reads the plan's contract and the release units it releases.
    /// The tags are carried because a real plan carries them: exactly one tag
    /// without a required phase, which is the global release tag the manifest
    /// names.
    pub(crate) fn sealed_plan() -> ReleasePlan {
        let mut plan = ReleasePlan {
            digest: String::new(),
            contract: "contract-1".to_owned(),
            generator: Generator {
                tool: "intentional".to_owned(),
                version: "0.0.0".to_owned(),
            },
            channel: None,
            release_units: vec![PlanReleaseUnit {
                id: RELEASE_UNIT.to_owned(),
                old_version: "0.9.0".to_owned(),
                new_version: "1.0.0".to_owned(),
                bump: Bump::Minor,
                contributing_intent_ids: Vec::new(),
                tag_ids: vec![format!("release-unit/{RELEASE_UNIT}/primary")],
                release_notes: "## 1.0.0\n".to_owned(),
            }],
            tags: vec![
                PlanTag {
                    id: format!("release-unit/{RELEASE_UNIT}/primary"),
                    name: format!("{RELEASE_UNIT}@1.0.0"),
                    version: "1.0.0".to_owned(),
                    release_unit: Some(RELEASE_UNIT.to_owned()),
                    role: Some(TagRole::Primary),
                    require_phase: Some(TagPhase::BeforePublication),
                    tag_after: Vec::new(),
                },
                PlanTag {
                    id: "workspace/release".to_owned(),
                    name: TAG_NAME.to_owned(),
                    version: "1.0.0".to_owned(),
                    release_unit: None,
                    role: None,
                    require_phase: None,
                    tag_after: Vec::new(),
                },
            ],
            tag_order: vec![
                format!("release-unit/{RELEASE_UNIT}/primary"),
                "workspace/release".to_owned(),
            ],
        };
        plan.digest = plan.payload_digest().expect("the fixture plan seals");
        plan
    }

    /// The exact bytes the handoff transports as its sealed plan.
    pub(crate) fn sealed_plan_bytes() -> String {
        // Preparation writes the canonical JSON followed by a newline, so the
        // fixture transports exactly what a real handoff transports.
        format!(
            "{}\n",
            sealed_plan()
                .to_canonical_json()
                .expect("the fixture plan serializes")
        )
    }

    /// Digest sealed inside the release plan the handoff transports.
    ///
    /// Derived from the plan rather than written down, so a fixture cannot
    /// state a digest the plan it ships does not have.
    pub(crate) fn plan_digest() -> String {
        sealed_plan().digest
    }

    /// Render the handoff manifest of the one prepared release under test.
    pub(crate) fn candidate_manifest() -> String {
        let plan_digest = plan_digest();
        let plan_bytes = sealed_plan_bytes();
        let plan_sha256 = digest_bytes(plan_bytes.as_bytes());
        let plan_size = plan_bytes.len();
        format!(
            r#"$schema: {RELEASE_CANDIDATE_SCHEMA}
contract: {RELEASE_CANDIDATE_CONTRACT}
source:
  commit: {SOURCE}
release:
  commit: {RELEASE}
  parent: {SOURCE}
  tree: 6666666666666666666666666666666666666666
plan:
  file: {RELEASE_PLAN_FILE}
  digest: {plan_digest}
  sha256: {plan_sha256}
global-tag:
  id: workspace/release
  name: {TAG_NAME}
  object: {TAG_OBJECT}
  target: {RELEASE}
changed-tree:
  - path: {RELEASE_UNIT}/package.json
    status: modified
    digest: sha256:8888888888888888888888888888888888888888888888888888888888888888
git-bundle:
  file: {RELEASE_BUNDLE_FILE}
  sha256: sha256:9999999999999999999999999999999999999999999999999999999999999999
  heads:
    - refs/heads/intentional-release
    - refs/tags/intentional-global-release
files:
  - path: {RELEASE_PLAN_FILE}
    sha256: {plan_sha256}
    size: {plan_size}
  - path: {RELEASE_BUNDLE_FILE}
    sha256: sha256:9999999999999999999999999999999999999999999999999999999999999999
    size: 512
"#
        )
    }

    /// Write that handoff beneath a workspace and return its directory.
    pub(crate) fn candidate(workspace: &Workspace) -> PathBuf {
        workspace
            .write(
                &format!("handoff/{RELEASE_CANDIDATE_MANIFEST}"),
                &candidate_manifest(),
            )
            .write(
                &format!("handoff/{RELEASE_PLAN_FILE}"),
                &sealed_plan_bytes(),
            );
        workspace.root().join("handoff")
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{
        candidate_manifest, plan_digest, RELEASE, SOURCE, TAG_NAME, TAG_OBJECT,
    };
    use super::*;
    use crate::executor::fixture::Workspace;

    fn identity() -> ReleaseIdentity {
        ReleaseIdentity {
            source_commit: SOURCE.to_owned(),
            release_commit: RELEASE.to_owned(),
            global_tag: TagIdentity {
                name: TAG_NAME.to_owned(),
                object: TAG_OBJECT.to_owned(),
                target: RELEASE.to_owned(),
            },
            plan_digest: plan_digest(),
        }
    }

    #[test]
    fn reads_the_release_identity_the_handoff_declares() {
        let workspace = Workspace::new("candidate-identity");
        let directory = super::test_support::candidate(&workspace);
        assert_eq!(
            prepared_release(&directory).expect("identity").identity,
            identity()
        );
    }

    #[test]
    fn refuses_a_handoff_whose_manifest_contradicts_itself() {
        let workspace = Workspace::new("candidate-identity-broken");
        // The global tag must target R. A manifest that targets S instead is
        // internally inconsistent, and assembly must not take the half of it
        // that still parses.
        workspace.write(
            &format!("handoff/{RELEASE_CANDIDATE_MANIFEST}"),
            &candidate_manifest().replace(
                &format!("  target: {RELEASE}"),
                &format!("  target: {SOURCE}"),
            ),
        );
        let error = prepared_release(&workspace.root().join("handoff"))
            .expect_err("an inconsistent handoff is refused");
        assert!(
            error
                .to_string()
                .contains("global tag must target release commit"),
            "{error}"
        );
    }

    #[test]
    fn refuses_a_handoff_that_is_not_there() {
        let workspace = Workspace::new("candidate-identity-absent");
        let error = prepared_release(workspace.root()).expect_err("an absent handoff is refused");
        assert!(
            error.to_string().contains(RELEASE_CANDIDATE_MANIFEST),
            "{error}"
        );
    }

    /// Each identity component is compared, and each is compared on its own.
    ///
    /// The battery corrupts one component at a time, so a comparison that
    /// stopped reading any single component fails here rather than passing
    /// because the components beside it still disagreed.
    #[test]
    fn names_each_identity_component_a_fragment_spells_differently() {
        let expected = identity();
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
}
