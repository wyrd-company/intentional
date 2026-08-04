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
use crate::release::candidate::{ReleaseCandidate, RELEASE_CANDIDATE_MANIFEST};
use std::path::Path;

/// Read the release identity the prepared candidate handoff declares.
///
/// The manifest is parsed through its own closed validation rather than picked
/// apart field by field, so a handoff that is internally inconsistent is
/// refused here instead of supplying assembly with half an identity.
pub fn candidate_identity(directory: &Path) -> Result<ReleaseIdentity> {
    let manifest = directory.join(RELEASE_CANDIDATE_MANIFEST);
    let text = std::fs::read_to_string(&manifest).map_err(|error| Error::io(&manifest, error))?;
    let candidate = ReleaseCandidate::from_yaml(&text).map_err(|error| {
        Error::Validation(format!(
            "release candidate {} is not a usable handoff: {error}",
            manifest.display()
        ))
    })?;
    Ok(ReleaseIdentity {
        source_commit: candidate.source.commit,
        release_commit: candidate.release.commit,
        global_tag: TagIdentity {
            name: candidate.global_tag.name,
            object: candidate.global_tag.object,
            target: candidate.global_tag.target,
        },
        plan_digest: candidate.plan.digest,
    })
}

/// Every identity component one document must spell the same way, in order.
///
/// The comparison is component-wise rather than whole-struct so a fragment that
/// carries the right commits and the wrong tag object names the tag object, and
/// so each component can fail on its own.
pub(crate) fn identity_disagreements(
    label: &str,
    expected: &ReleaseIdentity,
    observed: &ReleaseIdentity,
) -> Vec<String> {
    [
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
    ]
    .into_iter()
    .filter(|(_, expected, observed)| expected != observed)
    .map(|(field, expected, observed)| {
        format!(
            "{label} records {field} {observed:?}, but the prepared release candidate identifies \
             the release by {field} {expected:?}"
        )
    })
    .collect()
}

/// The one prepared handoff every evidence test binds its documents to.
///
/// Assembly, the fragments it accepts, and the phase documents it compares all
/// have to spell one release the same way. Every evidence test therefore reads
/// its identities from here rather than from a constant of its own, so a
/// fixture cannot agree with itself while disagreeing with the handoff.
#[cfg(test)]
pub(crate) mod test_support {
    use crate::executor::fixture::Workspace;
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
    /// Digest sealed inside the release plan.
    pub(crate) const PLAN_DIGEST: &str =
        "sha256:4444444444444444444444444444444444444444444444444444444444444444";

    /// Render the handoff manifest of the one prepared release under test.
    pub(crate) fn candidate_manifest() -> String {
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
  digest: {PLAN_DIGEST}
  sha256: sha256:7777777777777777777777777777777777777777777777777777777777777777
global-tag:
  id: workspace/release
  name: {TAG_NAME}
  object: {TAG_OBJECT}
  target: {RELEASE}
changed-tree:
  - path: component/package.json
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
    sha256: sha256:7777777777777777777777777777777777777777777777777777777777777777
    size: 256
  - path: {RELEASE_BUNDLE_FILE}
    sha256: sha256:9999999999999999999999999999999999999999999999999999999999999999
    size: 512
"#
        )
    }

    /// Write that handoff beneath a workspace and return its directory.
    pub(crate) fn candidate(workspace: &Workspace) -> PathBuf {
        workspace.write(
            &format!("handoff/{RELEASE_CANDIDATE_MANIFEST}"),
            &candidate_manifest(),
        );
        workspace.root().join("handoff")
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{
        candidate_manifest, PLAN_DIGEST, RELEASE, SOURCE, TAG_NAME, TAG_OBJECT,
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
            plan_digest: PLAN_DIGEST.to_owned(),
        }
    }

    #[test]
    fn reads_the_release_identity_the_handoff_declares() {
        let workspace = Workspace::new("candidate-identity");
        let directory = super::test_support::candidate(&workspace);
        assert_eq!(
            candidate_identity(&directory).expect("identity"),
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
        let error = candidate_identity(&workspace.root().join("handoff"))
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
        let error = candidate_identity(workspace.root()).expect_err("an absent handoff is refused");
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
    fn names_each_identity_component_a_document_spells_differently() {
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
            let findings = identity_disagreements("fragment.yml", &expected, &observed);
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
            identity_disagreements("fragment.yml", &expected, &expected).is_empty(),
            "an agreeing document reports nothing"
        );
    }
}
