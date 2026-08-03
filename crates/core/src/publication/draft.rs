// ---
// relationships:
//   implements: github-release-executor
// ---

//! Draft-Release asset handoff for publishers that consume GitHub-hosted deliverables before closure.
//!
//! A publisher whose normal consumer path resolves a GitHub Release asset
//! cannot perform public retrieval while that Release is a draft. It receives
//! this document instead: stable Release and asset identities bound to the
//! release identity that produced them, so it can download each asset through
//! authenticated GitHub access, prove the bytes, and record what it truthfully
//! did rather than a public claim it could not make.

use crate::config::Config;
use crate::error::{Error, Result};
use crate::evidence::assemble::CleanClientMode;
use crate::evidence::{digest_bytes, is_digest, is_flat_name, is_git_object};
use crate::executor::recipe::resolve_publications;
use crate::model::PublisherKind;
use crate::publication::release::ReleaseSource;
use crate::release::tag::verify_release_tag;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// Schema identity of one draft-Release asset handoff.
pub const DRAFT_HANDOFF_SCHEMA: &str =
    "https://intentional.foo/schemas/draft-release-asset-handoff/v1";
/// Contract identity of one draft-Release asset handoff.
pub const DRAFT_HANDOFF_CONTRACT: &str = "draft-release-asset-handoff-1";
/// File name one draft-Release asset handoff is transported as.
pub const DRAFT_HANDOFF_FILE: &str = "draft-release-asset-handoff.yml";

/// Publishers whose consumer path resolves a GitHub Release asset.
///
/// Only these adapters can be blocked by draft state, so only these can
/// legitimately claim authenticated draft retrieval instead of public
/// retrieval.
pub const DRAFT_DEPENDENT_PUBLISHERS: [PublisherKind; 4] = [
    PublisherKind::Homebrew,
    PublisherKind::Rpm,
    PublisherKind::Apt,
    PublisherKind::Aur,
];

/// Whether a publisher's consumer path depends on a GitHub Release asset.
pub fn is_draft_dependent(publisher: PublisherKind) -> bool {
    DRAFT_DEPENDENT_PUBLISHERS.contains(&publisher)
}

/// One GitHub Release asset a downstream publisher must consume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct HandoffAsset {
    /// Stable GitHub Release asset identifier.
    pub id: u64,
    /// Flat GitHub Release asset name.
    pub name: String,
    /// Declared asset size in bytes.
    pub size: u64,
    /// Declared asset media type.
    pub media_type: String,
    /// Declared asset digest.
    pub sha256: String,
}

/// The cross-repository document handed to one draft-dependent publisher.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DraftReleaseAssetHandoff {
    /// Handoff schema identity.
    #[serde(rename = "$schema")]
    pub schema: String,
    /// Handoff contract identity.
    pub contract: String,
    /// Owner and repository holding the draft Release.
    pub repository: String,
    /// Stable GitHub Release identifier of the draft.
    pub release_id: u64,
    /// Global release tag name.
    pub global_tag: String,
    /// Source commit S.
    pub source_commit: String,
    /// Release commit R.
    pub release_commit: String,
    /// Digest of the sealed release plan.
    pub plan_digest: String,
    /// Release unit whose deliverables are handed off.
    pub release_unit: String,
    /// Publisher adapter that consumes the assets.
    pub publisher: PublisherKind,
    /// Canonical target identity the publisher publishes to.
    pub target: String,
    /// Assets the publisher must retrieve, in stable order.
    pub assets: Vec<HandoffAsset>,
}

impl DraftReleaseAssetHandoff {
    /// Stable publication identity this handoff serves.
    pub fn identity(&self) -> String {
        format!("{}/{}/{}", self.release_unit, self.publisher, self.target)
    }

    /// Render the handoff as its transported YAML document.
    pub fn to_yaml(&self) -> Result<String> {
        Ok(serde_yaml::to_string(self)?)
    }

    /// Parse and mechanically validate one transported handoff document.
    pub fn from_yaml(text: &str) -> Result<Self> {
        let handoff: Self = serde_yaml::from_str(text)?;
        let findings = handoff.findings();
        if !findings.is_empty() {
            return Err(Error::Validation(findings.join("\n")));
        }
        Ok(handoff)
    }

    /// Report every mechanical problem this document carries.
    ///
    /// Every finding is collected rather than returned at the first failure so
    /// one run tells a publisher everything wrong with its handoff.
    fn findings(&self) -> Vec<String> {
        let mut findings = Vec::new();
        if self.schema != DRAFT_HANDOFF_SCHEMA {
            findings.push(format!(
                "draft handoff declares schema {:?} instead of {DRAFT_HANDOFF_SCHEMA}",
                self.schema
            ));
        }
        if self.contract != DRAFT_HANDOFF_CONTRACT {
            findings.push(format!(
                "draft handoff declares contract {:?} instead of {DRAFT_HANDOFF_CONTRACT}",
                self.contract
            ));
        }
        let (owner, name) = self.repository.split_once('/').unwrap_or(("", ""));
        let segment = |value: &str| {
            !value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
        };
        if !segment(owner) || !segment(name) {
            findings.push(format!(
                "draft handoff repository {:?} is not owner/name",
                self.repository
            ));
        }
        if self.release_id == 0 {
            findings.push("draft handoff records no GitHub Release identifier".to_owned());
        }
        if self.global_tag.is_empty() {
            findings.push("draft handoff records an empty global tag name".to_owned());
        }
        for (field, value) in [
            ("source-commit", &self.source_commit),
            ("release-commit", &self.release_commit),
        ] {
            if !is_git_object(value) {
                findings.push(format!(
                    "draft handoff records {field} {value:?}, which is not a complete Git object identifier"
                ));
            }
        }
        if !is_digest(&self.plan_digest) {
            findings.push(format!(
                "draft handoff records plan-digest {:?}, which is not a sha256 digest",
                self.plan_digest
            ));
        }
        if self.release_unit.is_empty() || self.target.is_empty() {
            findings.push("draft handoff records an empty release unit or target".to_owned());
        }
        if !is_draft_dependent(self.publisher) {
            findings.push(format!(
                "draft handoff names publisher {}, whose consumer path does not resolve a GitHub Release asset; expected one of {}",
                self.publisher,
                DRAFT_DEPENDENT_PUBLISHERS
                    .iter()
                    .map(PublisherKind::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if self.assets.is_empty() {
            findings.push(format!(
                "draft handoff for {} inventories no assets; a handoff exists to transfer at least one",
                self.identity()
            ));
        }
        let mut names = BTreeMap::new();
        for asset in &self.assets {
            if asset.id == 0 {
                findings.push(format!(
                    "draft handoff asset {:?} records no GitHub asset identifier",
                    asset.name
                ));
            }
            if !is_flat_name(&asset.name) {
                findings.push(format!(
                    "draft handoff asset name {:?} is not a flat GitHub Release asset name",
                    asset.name
                ));
            }
            if asset.media_type.is_empty() {
                findings.push(format!(
                    "draft handoff asset {:?} records an empty media type",
                    asset.name
                ));
            }
            if !is_digest(&asset.sha256) {
                findings.push(format!(
                    "draft handoff asset {:?} records sha256 {:?}, which is not a sha256 digest",
                    asset.name, asset.sha256
                ));
            }
            if let Some(previous) = names.insert(asset.name.clone(), asset.id) {
                findings.push(format!(
                    "draft handoff inventories asset name {:?} as both {previous} and {}",
                    asset.name, asset.id
                ));
            }
        }
        findings
    }
}

/// Inputs of one draft-Release asset handoff.
#[derive(Debug, Clone)]
pub struct HandoffRequest<'a> {
    /// Owner and repository holding the draft Release.
    pub repository: &'a str,
    /// Stable GitHub Release identifier of the draft.
    pub release_id: u64,
    /// Global release tag name.
    pub global_tag: &'a str,
    /// Source commit S.
    pub source_commit: &'a str,
    /// Release commit R.
    pub release_commit: &'a str,
    /// Digest of the sealed release plan.
    pub plan_digest: &'a str,
    /// Release unit whose deliverables are handed off.
    pub release_unit: &'a str,
    /// Publisher adapter that consumes the assets.
    pub publisher: PublisherKind,
    /// Canonical target identity the publisher publishes to.
    pub target: &'a str,
    /// Assets the publisher must retrieve.
    pub assets: &'a [HandoffAsset],
    /// File the document is written to.
    pub path: &'a Path,
}

/// Write one draft-Release asset handoff deterministically.
///
/// The inventory is ordered by asset name rather than by discovery order so a
/// rerun of the same release produces byte-identical bytes for a consumer to
/// compare.
pub fn write_handoff(request: &HandoffRequest<'_>) -> Result<DraftReleaseAssetHandoff> {
    let mut assets = request.assets.to_vec();
    assets.sort_by(|left, right| left.name.cmp(&right.name));
    let handoff = DraftReleaseAssetHandoff {
        schema: DRAFT_HANDOFF_SCHEMA.to_owned(),
        contract: DRAFT_HANDOFF_CONTRACT.to_owned(),
        repository: request.repository.to_owned(),
        release_id: request.release_id,
        global_tag: request.global_tag.to_owned(),
        source_commit: request.source_commit.to_owned(),
        release_commit: request.release_commit.to_owned(),
        plan_digest: request.plan_digest.to_owned(),
        release_unit: request.release_unit.to_owned(),
        publisher: request.publisher,
        target: request.target.to_owned(),
        assets,
    };
    let findings = handoff.findings();
    if !findings.is_empty() {
        return Err(Error::Validation(findings.join("\n")));
    }
    if let Some(parent) = request.path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
    }
    std::fs::write(request.path, handoff.to_yaml()?)
        .map_err(|error| Error::io(request.path, error))?;
    Ok(handoff)
}

/// One asset proved to carry exactly the bytes its handoff declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetrievedAsset {
    /// Stable GitHub Release asset identifier.
    pub id: u64,
    /// Flat GitHub Release asset name.
    pub name: String,
    /// Retrieved byte count.
    pub size: u64,
    /// Digest of exactly the retrieved bytes.
    pub sha256: String,
}

/// The result of retrieving draft assets through authenticated GitHub access.
///
/// The mode is a property of the type rather than a field, so a publisher that
/// consumed a draft Release cannot record a public clean-client claim it never
/// performed. There is no constructor outside this module and no way to set
/// the mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticatedDraftRetrieval {
    release_id: u64,
    assets: Vec<RetrievedAsset>,
}

impl AuthenticatedDraftRetrieval {
    /// The only retrieval mode this result can describe.
    pub const fn mode(&self) -> CleanClientMode {
        CleanClientMode::AuthenticatedDraft
    }

    /// Draft Release the assets were retrieved from.
    pub const fn release_id(&self) -> u64 {
        self.release_id
    }

    /// Retrieved assets in handoff order.
    pub fn assets(&self) -> &[RetrievedAsset] {
        &self.assets
    }
}

/// Download every inventoried asset and prove its declared size and digest.
///
/// Retrieval reads the draft's live inventory rather than trusting the handoff
/// alone, so an identifier that no longer belongs to that draft fails instead
/// of silently resolving to whatever the publisher's token can reach.
pub fn retrieve_assets(
    handoff: &DraftReleaseAssetHandoff,
    source: &dyn ReleaseSource,
) -> Result<AuthenticatedDraftRetrieval> {
    let mut findings = handoff.findings();
    let release = source.release(&handoff.repository, &handoff.global_tag)?;
    if release.id != handoff.release_id {
        findings.push(format!(
            "draft handoff names Release {} for tag {} but the repository resolves Release {}",
            handoff.release_id, handoff.global_tag, release.id
        ));
    }
    if !release.draft {
        findings.push(format!(
            "Release {} for tag {} is no longer a draft; authenticated draft retrieval applies before closure",
            release.id, handoff.global_tag
        ));
    }
    if !findings.is_empty() {
        return Err(Error::Validation(findings.join("\n")));
    }

    let inventory = source
        .assets(&handoff.repository, release.id)?
        .into_iter()
        .map(|asset| (asset.id, asset))
        .collect::<BTreeMap<_, _>>();
    let mut retrieved = Vec::new();
    for asset in &handoff.assets {
        let Some(present) = inventory.get(&asset.id) else {
            findings.push(format!(
                "draft handoff asset {} ({:?}) does not resolve within Release {}",
                asset.id, asset.name, release.id
            ));
            continue;
        };
        if present.name != asset.name {
            findings.push(format!(
                "draft handoff asset {} is named {:?} but Release {} holds it as {:?}",
                asset.id, asset.name, release.id, present.name
            ));
            continue;
        }
        let bytes = source.asset_bytes(&handoff.repository, asset.id)?;
        let size = bytes.len() as u64;
        if size != asset.size {
            findings.push(format!(
                "draft handoff asset {:?} declares {} bytes but retrieved {size}",
                asset.name, asset.size
            ));
            continue;
        }
        let digest = digest_bytes(&bytes);
        if digest != asset.sha256 {
            findings.push(format!(
                "draft handoff asset {:?} declares {} but retrieved {digest}",
                asset.name, asset.sha256
            ));
            continue;
        }
        retrieved.push(RetrievedAsset {
            id: asset.id,
            name: asset.name.clone(),
            size,
            sha256: digest,
        });
    }
    if !findings.is_empty() {
        return Err(Error::Validation(findings.join("\n")));
    }
    Ok(AuthenticatedDraftRetrieval {
        release_id: release.id,
        assets: retrieved,
    })
}

/// One handoff whose release identity, inventory, and bytes all held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedDraftHandoff {
    /// Publication identity the handoff serves.
    pub identity: String,
    /// Proved authenticated draft retrieval.
    pub retrieval: AuthenticatedDraftRetrieval,
}

/// Prove the handoff describes exactly the release this workspace published.
///
/// Independent verification of the checked-out release tag rebuilds the sealed
/// plan, the release commit, and the annotated global tag from the source
/// commit alone, so each identity the handoff declares is compared against a
/// proved value instead of being accepted in the shape it was written. Without
/// that comparison a document naming another repository's release under a
/// familiar tag name would hand its bytes to this publisher.
fn verify_release_identity(root: &Path, handoff: &DraftReleaseAssetHandoff) -> Result<()> {
    let repository = crate::publication::origin_identity(root)?;
    let verified = verify_release_tag(root)?;
    let mut findings = Vec::new();
    if handoff.repository != repository {
        findings.push(format!(
            "draft handoff names repository {:?}, but this workspace publishes {repository:?}",
            handoff.repository
        ));
    }
    for (field, declared, proved) in [
        ("global-tag", &handoff.global_tag, &verified.global_tag),
        ("release-commit", &handoff.release_commit, &verified.release),
        ("source-commit", &handoff.source_commit, &verified.source),
        ("plan-digest", &handoff.plan_digest, &verified.plan_digest),
    ] {
        if declared != proved {
            findings.push(format!(
                "draft handoff records {field} {declared:?}, but the verified release tag proves {proved:?}"
            ));
        }
    }
    if !findings.is_empty() {
        return Err(Error::Validation(findings.join("\n")));
    }
    Ok(())
}

/// Verify one draft-Release asset handoff against the repository at R.
///
/// The configured publication set is the authority on which publisher and
/// target may receive draft assets at all, so a handoff for a publication the
/// release does not select is refused before any byte is downloaded, and the
/// release identity the document declares is proved against the repository
/// before its inventory is trusted.
pub fn verify_draft_handoff(
    root: &Path,
    handoff: &DraftReleaseAssetHandoff,
    source: &dyn ReleaseSource,
) -> Result<VerifiedDraftHandoff> {
    let config = Config::load(root)?;
    let selection = resolve_publications(root, &config)?;
    let identity = handoff.identity();
    if !selection
        .selected
        .iter()
        .any(|publication| publication.identity() == identity)
    {
        let expected = selection
            .selected
            .iter()
            .map(|publication| publication.identity())
            .collect::<Vec<_>>();
        return Err(Error::Validation(format!(
            "draft handoff serves publication {identity}, which the release configuration does not select; configured publications are {}",
            if expected.is_empty() {
                "none".to_owned()
            } else {
                expected.join(", ")
            }
        )));
    }
    verify_release_identity(root, handoff)?;
    let retrieval = retrieve_assets(handoff, source)?;
    Ok(VerifiedDraftHandoff {
        identity,
        retrieval,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::fixture::Workspace;
    use crate::publication::release::tests::FakeReleaseSource;
    use crate::release::git::GitCommand;
    use std::path::PathBuf;

    const SOURCE: &str = "1111111111111111111111111111111111111111";
    const RELEASE: &str = "2222222222222222222222222222222222222222";
    const PLAN_DIGEST: &str =
        "sha256:4444444444444444444444444444444444444444444444444444444444444444";
    const GLOBAL_TAG: &str = "component@1.0.0";

    const CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-1
github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml }
release-units:
  component:
    path: component
    homebrew:
      repository: example-owner/homebrew-example
    tags:
      primary: { role: primary, template: '{id}@{version}' }
"#;

    /// A git workspace carrying one applied release and its published global tag.
    ///
    /// Handoff verification proves the declared release identity against the
    /// repository, so the fixture must be a real released repository with an
    /// origin remote rather than a bare directory of configuration files.
    struct ReleasedWorkspace {
        _temp: tempfile::TempDir,
        root: PathBuf,
        /// Accepted source commit S.
        source: String,
        /// Deterministic release commit R.
        release: String,
        /// Rendered name of the annotated global release tag.
        global_tag: String,
        /// Digest sealed inside the release plan.
        plan_digest: String,
    }

    impl ReleasedWorkspace {
        /// Author one intent, build the release, and publish its annotated global tag.
        fn new() -> Self {
            let temp = tempfile::tempdir().expect("temporary directory");
            let root = temp.path().join("workspace");
            std::fs::create_dir_all(&root).expect("create workspace");
            git(&root, &["init", "--quiet", "--initial-branch=main"]);
            git(&root, &["config", "user.name", "Fixture Author"]);
            git(&root, &["config", "user.email", "fixture@example.invalid"]);
            git(
                &root,
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://github.com/example-owner/example-repository.git",
                ],
            );
            write(&root, ".intentional/config.yml", CONFIG);
            write(
                &root,
                "component/go.mod",
                "module example.test/component\n\ngo 1.22\n",
            );
            write(
                &root,
                "component/main.go",
                "package main\n\nfunc main() {}\n",
            );
            write(&root, ".intentional/intents/.keep", "");
            git(&root, &["add", "-A"]);
            git(&root, &["commit", "--quiet", "-m", "Create the workspace"]);
            // The release unit carries no projection, so its baseline version
            // has no file to be read from and must be stated.
            let baseline = BTreeMap::from([(
                "component".to_owned(),
                semver::Version::parse("1.0.0").expect("baseline version"),
            )]);
            crate::tag::TagResult::build_baseline(&root, &baseline)
                .expect("baseline tag set")
                .apply(&root, false)
                .expect("record baseline tags");

            write(
                &root,
                ".intentional/intents/quiet-otter-0001.md",
                "---\ncomponent: minor\n---\n\nAdd a component capability\n",
            );
            git(&root, &["add", "-A"]);
            git(&root, &["commit", "--quiet", "-m", "Record release intent"]);

            let source = git(&root, &["rev-parse", "HEAD^{commit}"]);
            let built =
                crate::release::build::build_candidate(&root, &source).expect("release candidate");
            git(
                &root,
                &[
                    "update-ref",
                    &format!("refs/tags/{}", built.tag_name),
                    &built.tag_object,
                ],
            );
            git(
                &root,
                &["checkout", "--quiet", "--detach", &built.release_commit],
            );
            Self {
                _temp: temp,
                root,
                source,
                release: built.release_commit.clone(),
                global_tag: built.tag_name.clone(),
                plan_digest: built.plan.digest.clone(),
            }
        }

        /// A handoff declaring exactly the release identity this workspace published.
        fn handoff(&self, bytes: &[u8]) -> DraftReleaseAssetHandoff {
            DraftReleaseAssetHandoff {
                global_tag: self.global_tag.clone(),
                source_commit: self.source.clone(),
                release_commit: self.release.clone(),
                plan_digest: self.plan_digest.clone(),
                ..handoff(bytes)
            }
        }

        /// A release source serving this workspace's draft.
        fn source(&self, bytes: &[u8]) -> FakeReleaseSource {
            FakeReleaseSource::draft("example-owner/example-repository", &self.global_tag, 7)
                .with_asset(11, "component-1.0.0.tgz", "application/gzip", bytes)
        }
    }

    fn git(directory: &Path, arguments: &[&str]) -> String {
        GitCommand::new(directory)
            .args(arguments)
            .run()
            .unwrap_or_else(|error| panic!("git {arguments:?} failed: {error}"))
            .line()
            .expect("git output")
    }

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().expect("parent directory"))
            .expect("create parent directory");
        std::fs::write(path, contents).expect("write fixture file");
    }

    fn source(bytes: &[u8]) -> FakeReleaseSource {
        FakeReleaseSource::draft("example-owner/example-repository", GLOBAL_TAG, 7).with_asset(
            11,
            "component-1.0.0.tgz",
            "application/gzip",
            bytes,
        )
    }

    fn handoff(bytes: &[u8]) -> DraftReleaseAssetHandoff {
        DraftReleaseAssetHandoff {
            schema: DRAFT_HANDOFF_SCHEMA.to_owned(),
            contract: DRAFT_HANDOFF_CONTRACT.to_owned(),
            repository: "example-owner/example-repository".to_owned(),
            release_id: 7,
            global_tag: GLOBAL_TAG.to_owned(),
            source_commit: SOURCE.to_owned(),
            release_commit: RELEASE.to_owned(),
            plan_digest: PLAN_DIGEST.to_owned(),
            release_unit: "component".to_owned(),
            publisher: PublisherKind::Homebrew,
            target: "primary".to_owned(),
            assets: vec![HandoffAsset {
                id: 11,
                name: "component-1.0.0.tgz".to_owned(),
                size: bytes.len() as u64,
                media_type: "application/gzip".to_owned(),
                sha256: digest_bytes(bytes),
            }],
        }
    }

    #[test]
    fn writes_a_deterministic_handoff_ordered_by_asset_name() {
        let workspace = Workspace::new("handoff-write");
        let path = workspace.root().join("handoff/draft.yml");
        let assets = vec![
            HandoffAsset {
                id: 12,
                name: "component-1.0.0.tgz".to_owned(),
                size: 4,
                media_type: "application/gzip".to_owned(),
                sha256: digest_bytes(b"tgz!"),
            },
            HandoffAsset {
                id: 11,
                name: "component-1.0.0.deb".to_owned(),
                size: 4,
                media_type: "application/vnd.debian.binary-package".to_owned(),
                sha256: digest_bytes(b"deb!"),
            },
        ];
        let request = HandoffRequest {
            repository: "example-owner/example-repository",
            release_id: 7,
            global_tag: GLOBAL_TAG,
            source_commit: SOURCE,
            release_commit: RELEASE,
            plan_digest: PLAN_DIGEST,
            release_unit: "component",
            publisher: PublisherKind::Apt,
            target: "primary",
            assets: &assets,
            path: &path,
        };
        let written = write_handoff(&request).expect("handoff written");
        assert_eq!(
            written
                .assets
                .iter()
                .map(|asset| asset.name.as_str())
                .collect::<Vec<_>>(),
            ["component-1.0.0.deb", "component-1.0.0.tgz"]
        );

        let repeat = workspace.root().join("handoff/repeat.yml");
        write_handoff(&HandoffRequest {
            path: &repeat,
            ..request
        })
        .expect("handoff repeats");
        assert_eq!(
            std::fs::read_to_string(&path).expect("first"),
            std::fs::read_to_string(&repeat).expect("second"),
            "handoff writing is deterministic"
        );
        assert_eq!(
            DraftReleaseAssetHandoff::from_yaml(&std::fs::read_to_string(&path).expect("document"))
                .expect("handoff parses"),
            written
        );
    }

    #[test]
    fn refuses_a_handoff_for_a_publisher_that_does_not_consume_release_assets() {
        let workspace = Workspace::new("handoff-publisher");
        let path = workspace.root().join("draft.yml");
        let error = write_handoff(&HandoffRequest {
            repository: "example-owner/example-repository",
            release_id: 7,
            global_tag: GLOBAL_TAG,
            source_commit: SOURCE,
            release_commit: RELEASE,
            plan_digest: PLAN_DIGEST,
            release_unit: "component",
            publisher: PublisherKind::Npm,
            target: "primary",
            assets: &[HandoffAsset {
                id: 11,
                name: "component-1.0.0.tgz".to_owned(),
                size: 4,
                media_type: "application/gzip".to_owned(),
                sha256: digest_bytes(b"tgz!"),
            }],
            path: &path,
        })
        .expect_err("the publisher is refused");
        assert!(error.to_string().contains("npm"), "{error}");
        assert!(!path.exists(), "a refused handoff writes nothing");
    }

    #[test]
    fn retrieves_every_inventoried_asset_as_an_authenticated_draft() {
        let bytes = b"deliverable bytes";
        let retrieval = retrieve_assets(&handoff(bytes), &source(bytes)).expect("assets retrieved");
        assert_eq!(retrieval.mode(), CleanClientMode::AuthenticatedDraft);
        assert_eq!(retrieval.release_id(), 7);
        assert_eq!(
            retrieval.assets(),
            [RetrievedAsset {
                id: 11,
                name: "component-1.0.0.tgz".to_owned(),
                size: bytes.len() as u64,
                sha256: digest_bytes(bytes),
            }]
        );
    }

    #[test]
    fn rejects_an_asset_whose_bytes_do_not_match_its_declared_digest() {
        let mut document = handoff(b"deliverable bytes");
        document.assets[0].sha256 = digest_bytes(b"other bytes");
        let error = retrieve_assets(&document, &source(b"deliverable bytes"))
            .expect_err("the digest is refused");
        assert!(error.to_string().contains("component-1.0.0.tgz"), "{error}");
        assert!(error.to_string().contains("but retrieved"), "{error}");
    }

    #[test]
    fn rejects_an_asset_identifier_absent_from_the_draft() {
        let mut document = handoff(b"deliverable bytes");
        document.assets[0].id = 99;
        let error = retrieve_assets(&document, &source(b"deliverable bytes"))
            .expect_err("the identifier is refused");
        assert!(error.to_string().contains("does not resolve"), "{error}");
    }

    #[test]
    fn verifies_a_handoff_the_release_configuration_selects() {
        let workspace = ReleasedWorkspace::new();
        let bytes = b"deliverable bytes";
        let verified = verify_draft_handoff(
            &workspace.root,
            &workspace.handoff(bytes),
            &workspace.source(bytes),
        )
        .expect("the handoff verifies");
        assert_eq!(verified.identity, "component/homebrew/primary");
        assert_eq!(
            verified.retrieval.mode(),
            CleanClientMode::AuthenticatedDraft
        );
    }

    #[test]
    fn refuses_a_handoff_the_release_configuration_does_not_expect() {
        let workspace = ReleasedWorkspace::new();
        let bytes = b"deliverable bytes";
        let mut document = workspace.handoff(bytes);
        document.target = "sample-library".to_owned();
        let error = verify_draft_handoff(&workspace.root, &document, &workspace.source(bytes))
            .expect_err("the publication is refused");
        assert!(
            error
                .to_string()
                .contains("component/homebrew/sample-library"),
            "{error}"
        );
    }

    #[test]
    fn refuses_a_handoff_naming_a_repository_this_workspace_does_not_publish() {
        let workspace = ReleasedWorkspace::new();
        let bytes = b"deliverable bytes";
        let mut document = workspace.handoff(bytes);
        document.repository = "other-owner/other-repository".to_owned();
        let error = verify_draft_handoff(&workspace.root, &document, &workspace.source(bytes))
            .expect_err("the repository is refused");
        assert!(
            error.to_string().contains("other-owner/other-repository"),
            "{error}"
        );
        assert!(
            error
                .to_string()
                .contains("example-owner/example-repository"),
            "{error}"
        );
    }

    #[test]
    fn refuses_a_handoff_whose_release_identity_the_tag_does_not_prove() {
        let workspace = ReleasedWorkspace::new();
        let bytes = b"deliverable bytes";
        let mut document = workspace.handoff(bytes);
        document.source_commit = RELEASE.to_owned();
        document.plan_digest = PLAN_DIGEST.to_owned();
        let error = verify_draft_handoff(&workspace.root, &document, &workspace.source(bytes))
            .expect_err("the release identity is refused");
        assert!(error.to_string().contains("source-commit"), "{error}");
        assert!(error.to_string().contains("plan-digest"), "{error}");
        assert!(
            error.to_string().contains(&workspace.plan_digest),
            "{error}"
        );
    }

    #[test]
    fn refuses_a_release_that_is_no_longer_a_draft() {
        let bytes = b"deliverable bytes";
        let published = source(bytes).published();
        let error = retrieve_assets(&handoff(bytes), &published).expect_err("closure is refused");
        assert!(error.to_string().contains("no longer a draft"), "{error}");
    }
}
