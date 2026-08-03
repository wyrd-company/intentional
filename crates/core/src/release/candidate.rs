// ---
// relationships:
//   implements: github-release-executor
// ---

//! The release-candidate handoff manifest and its closed validation rules.
//!
//! The manifest is the authority for a prepared release. Verification treats
//! every value in it as untrusted input, so the structural rules published in
//! `release-candidate.json-schema.yml` are enforced here rather than assumed.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt;

/// Published identity of the release-candidate handoff schema.
pub const RELEASE_CANDIDATE_SCHEMA: &str = "https://intentional.foo/schemas/release-candidate/v1";
/// Interpretation contract carried by every release-candidate handoff.
pub const RELEASE_CANDIDATE_CONTRACT: &str = "github-release-candidate-1";
/// Manifest file name inside the handoff directory.
pub const RELEASE_CANDIDATE_MANIFEST: &str = "release-candidate.yml";
/// Sealed release-plan file name inside the handoff directory.
pub const RELEASE_PLAN_FILE: &str = "release-plan.json";
/// Git bundle file name inside the handoff directory.
pub const RELEASE_BUNDLE_FILE: &str = "release.bundle";
/// Directory holding the materialized release candidate inside the handoff.
pub const CANDIDATE_TREE_DIRECTORY: &str = "candidate";
/// Transport ref carrying the release commit inside the Git bundle.
pub const BUNDLE_RELEASE_HEAD: &str = "refs/heads/intentional-release";
/// Transport ref carrying the annotated global tag object inside the Git bundle.
pub const BUNDLE_TAG_HEAD: &str = "refs/tags/intentional-global-release";
/// Local ref under which verification imports the verified release commit.
pub const IMPORTED_RELEASE_REF: &str = "refs/intentional/handoff/release";

/// Largest Git bundle a handoff may transport.
///
/// The bundle is thin: it carries only the objects introduced by the release
/// commit and its annotated tag, with the accepted source commit declared as a
/// prerequisite. A bound keeps an untrusted bundle from becoming an unbounded
/// decompression and object-ingestion surface in the privileged job.
pub const MAX_BUNDLE_BYTES: u64 = 64 * 1024 * 1024;

/// Largest single file a handoff may transport.
pub const MAX_CANDIDATE_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// Largest number of files a handoff may inventory.
pub const MAX_CANDIDATE_FILES: usize = 4096;

/// How one path changed between the accepted source tree and the release tree.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum ChangeStatus {
    /// The path does not exist in the source tree.
    Added,
    /// The path exists in both trees with different content.
    Modified,
    /// The path exists only in the source tree.
    Deleted,
}

impl fmt::Display for ChangeStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
        })
    }
}

/// The accepted default-branch source commit.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SourceIdentity {
    /// Source commit S.
    pub commit: String,
}

/// The deterministic release commit and its bound identities.
///
/// Named for the candidate it belongs to, because release evidence carries its
/// own, unrelated release identity.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CandidateReleaseIdentity {
    /// Release commit R.
    pub commit: String,
    /// Sole parent of R, which is always S.
    pub parent: String,
    /// Tree of R.
    pub tree: String,
}

/// The sealed release plan transported by the handoff.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct PlanInventory {
    /// Handoff-relative plan file.
    pub file: String,
    /// Digest sealed inside the plan.
    pub digest: String,
    /// Digest of the transported plan file bytes.
    pub sha256: String,
}

/// The annotated global release tag created by preparation.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct GlobalTag {
    /// Canonical configured tag id.
    pub id: String,
    /// Rendered Git tag name.
    pub name: String,
    /// Annotated tag object identity.
    pub object: String,
    /// Commit the annotated tag targets, which is always R.
    pub target: String,
}

/// One path changed by the release.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ChangedPath {
    /// Workspace-relative path.
    pub path: String,
    /// How the path changed.
    pub status: ChangeStatus,
    /// Digest of the release content, absent for a deleted path.
    pub digest: Option<String>,
}

/// The Git bundle transporting the unpushed release objects.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct BundleInventory {
    /// Handoff-relative bundle file.
    pub file: String,
    /// Digest of the transported bundle bytes.
    pub sha256: String,
    /// Refs the bundle declares.
    pub heads: Vec<String>,
}

/// One inventoried file transported by the handoff.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CandidateFile {
    /// Handoff-relative path.
    pub path: String,
    /// Digest of the file bytes.
    pub sha256: String,
    /// Size of the file in bytes.
    pub size: u64,
}

/// The schema-backed release-candidate handoff manifest.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ReleaseCandidate {
    /// Published schema identity.
    #[serde(rename = "$schema")]
    pub schema: String,
    /// Interpretation contract.
    pub contract: String,
    /// Accepted source commit.
    pub source: SourceIdentity,
    /// Deterministic release commit identities.
    pub release: CandidateReleaseIdentity,
    /// Sealed release plan.
    pub plan: PlanInventory,
    /// Annotated global release tag.
    pub global_tag: GlobalTag,
    /// Changed-tree evidence in path order.
    pub changed_tree: Vec<ChangedPath>,
    /// Git bundle transporting the release objects.
    pub git_bundle: BundleInventory,
    /// Every file transported by the handoff, in path order.
    pub files: Vec<CandidateFile>,
}

impl ReleaseCandidate {
    /// Parse a manifest and enforce its closed structural rules.
    pub fn from_yaml(text: &str) -> Result<Self> {
        let candidate: Self = serde_yaml::from_str(text)?;
        candidate.validate()?;
        Ok(candidate)
    }

    /// Serialize the manifest deterministically.
    pub fn to_yaml(&self) -> Result<String> {
        self.validate()?;
        Ok(serde_yaml::to_string(self)?)
    }

    /// Enforce every structural rule published by the handoff schema.
    pub fn validate(&self) -> Result<()> {
        if self.schema != RELEASE_CANDIDATE_SCHEMA {
            return Err(Error::Validation(format!(
                "release candidate schema must be {RELEASE_CANDIDATE_SCHEMA}; got {}",
                self.schema
            )));
        }
        if self.contract != RELEASE_CANDIDATE_CONTRACT {
            return Err(Error::Validation(format!(
                "release candidate contract must be {RELEASE_CANDIDATE_CONTRACT}; got {}",
                self.contract
            )));
        }
        validate_object("source commit", &self.source.commit)?;
        validate_object("release commit", &self.release.commit)?;
        validate_object("release parent", &self.release.parent)?;
        validate_object("release tree", &self.release.tree)?;
        validate_object("global tag object", &self.global_tag.object)?;
        validate_object("global tag target", &self.global_tag.target)?;
        validate_digest("plan digest", &self.plan.digest)?;
        validate_digest("plan file digest", &self.plan.sha256)?;
        validate_digest("git bundle digest", &self.git_bundle.sha256)?;
        validate_path("plan file", &self.plan.file)?;
        validate_path("git bundle file", &self.git_bundle.file)?;
        if self.global_tag.id.is_empty() {
            return Err(Error::Validation(
                "global tag id must not be empty".to_owned(),
            ));
        }
        validate_tag_name(&self.global_tag.name)?;

        if self.release.parent != self.source.commit {
            return Err(Error::Validation(format!(
                "release parent {} must be the accepted source commit {}",
                self.release.parent, self.source.commit
            )));
        }
        if self.global_tag.target != self.release.commit {
            return Err(Error::Validation(format!(
                "global tag must target release commit {}; got {}",
                self.release.commit, self.global_tag.target
            )));
        }
        if self.release.commit == self.source.commit {
            return Err(Error::Validation(
                "release commit must differ from the accepted source commit".to_owned(),
            ));
        }

        let mut heads = BTreeSet::new();
        for head in &self.git_bundle.heads {
            if head != BUNDLE_RELEASE_HEAD && head != BUNDLE_TAG_HEAD {
                return Err(Error::Validation(format!(
                    "git bundle head {head} is not a release handoff transport ref"
                )));
            }
            if !heads.insert(head.as_str()) {
                return Err(Error::Validation(format!(
                    "git bundle head {head} is declared more than once"
                )));
            }
        }
        if !heads.contains(BUNDLE_RELEASE_HEAD) || !heads.contains(BUNDLE_TAG_HEAD) {
            return Err(Error::Validation(format!(
                "git bundle must declare {BUNDLE_RELEASE_HEAD} and {BUNDLE_TAG_HEAD}"
            )));
        }

        let mut changed = BTreeSet::new();
        let mut previous: Option<&str> = None;
        for entry in &self.changed_tree {
            validate_path("changed path", &entry.path)?;
            if !changed.insert(entry.path.as_str()) {
                return Err(Error::Validation(format!(
                    "changed path {} is inventoried more than once",
                    entry.path
                )));
            }
            if previous.is_some_and(|last| last >= entry.path.as_str()) {
                return Err(Error::Validation(
                    "changed-tree evidence must be ordered by path".to_owned(),
                ));
            }
            previous = Some(&entry.path);
            match (entry.status, entry.digest.as_deref()) {
                (ChangeStatus::Deleted, None) => {}
                (ChangeStatus::Deleted, Some(_)) => {
                    return Err(Error::Validation(format!(
                        "deleted path {} must not carry a content digest",
                        entry.path
                    )))
                }
                (_, Some(digest)) => validate_digest("changed path digest", digest)?,
                (status, None) => {
                    return Err(Error::Validation(format!(
                        "{status} path {} requires a content digest",
                        entry.path
                    )))
                }
            }
        }

        if self.files.is_empty() {
            return Err(Error::Validation(
                "a release candidate must inventory at least one file".to_owned(),
            ));
        }
        if self.files.len() > MAX_CANDIDATE_FILES {
            return Err(Error::Validation(format!(
                "a release candidate must inventory at most {MAX_CANDIDATE_FILES} files; got {}",
                self.files.len()
            )));
        }
        let mut inventoried = BTreeSet::new();
        let mut previous: Option<&str> = None;
        for file in &self.files {
            validate_path("inventoried file", &file.path)?;
            if file.path == RELEASE_CANDIDATE_MANIFEST {
                return Err(Error::Validation(
                    "the manifest must not inventory itself".to_owned(),
                ));
            }
            validate_digest("inventoried file digest", &file.sha256)?;
            if file.size > MAX_CANDIDATE_FILE_BYTES {
                return Err(Error::Validation(format!(
                    "inventoried file {} exceeds the {MAX_CANDIDATE_FILE_BYTES} byte bound",
                    file.path
                )));
            }
            if !inventoried.insert(file.path.as_str()) {
                return Err(Error::Validation(format!(
                    "file {} is inventoried more than once",
                    file.path
                )));
            }
            if previous.is_some_and(|last| last >= file.path.as_str()) {
                return Err(Error::Validation(
                    "the file inventory must be ordered by path".to_owned(),
                ));
            }
            previous = Some(&file.path);
        }
        for required in [&self.plan.file, &self.git_bundle.file] {
            if !inventoried.contains(required.as_str()) {
                return Err(Error::Validation(format!(
                    "handoff file {required} is missing from the file inventory"
                )));
            }
        }
        if self.plan.file == self.git_bundle.file {
            return Err(Error::Validation(
                "the sealed plan and the git bundle must be distinct files".to_owned(),
            ));
        }
        Ok(())
    }

    /// The bundle's inventoried size, when it is inventoried.
    pub fn bundle_size(&self) -> Option<u64> {
        self.files
            .iter()
            .find(|file| file.path == self.git_bundle.file)
            .map(|file| file.size)
    }
}

/// Digest one byte sequence in the canonical `sha256:` form.
pub fn digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// Whether a value is a complete lowercase hexadecimal Git object identity.
fn validate_object(label: &str, value: &str) -> Result<()> {
    let valid = matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if valid {
        return Ok(());
    }
    Err(Error::Validation(format!(
        "{label} must be a complete lowercase Git object identity; got {value}"
    )))
}

/// Whether a value is a canonical `sha256:` digest.
fn validate_digest(label: &str, value: &str) -> Result<()> {
    let valid = value
        .strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
        && value.to_ascii_lowercase() == value;
    if valid {
        return Ok(());
    }
    Err(Error::Validation(format!(
        "{label} must be a canonical sha256 digest; got {value}"
    )))
}

/// Whether a value is a contained relative path safe to join beneath a root.
pub(crate) fn validate_path(label: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(Error::Validation(format!("{label} must not be empty")));
    }
    if value.starts_with('/') {
        return Err(Error::Validation(format!(
            "{label} must be relative; got {value}"
        )));
    }
    if value.contains('\\') {
        return Err(Error::Validation(format!(
            "{label} must not contain a backslash; got {value}"
        )));
    }
    if value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(Error::Validation(format!(
            "{label} must not contain control characters"
        )));
    }
    for segment in value.split('/') {
        if segment.is_empty() {
            return Err(Error::Validation(format!(
                "{label} must not contain an empty path segment; got {value}"
            )));
        }
        if segment == "." || segment == ".." {
            return Err(Error::Validation(format!(
                "{label} must not traverse directories; got {value}"
            )));
        }
        if segment.eq_ignore_ascii_case(".git") {
            return Err(Error::Validation(format!(
                "{label} must not address Git metadata; got {value}"
            )));
        }
    }
    Ok(())
}

/// Whether a value is usable as a Git tag name in a ref and on a command line.
fn validate_tag_name(value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(Error::Validation(
            "global tag name must not be empty".to_owned(),
        ));
    }
    if value.starts_with('-') {
        return Err(Error::Validation(format!(
            "global tag name must not start with a hyphen; got {value}"
        )));
    }
    let rejected = [' ', '~', '^', ':', '?', '*', '[', '\\', '\x7f'];
    if value.bytes().any(|byte| byte.is_ascii_control())
        || value.chars().any(|value| rejected.contains(&value))
        || value.contains("..")
        || value.contains("@{")
        || value.ends_with('.')
        || value.ends_with(".lock")
    {
        return Err(Error::Validation(format!(
            "global tag name is not a valid Git tag name; got {value}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate() -> ReleaseCandidate {
        ReleaseCandidate {
            schema: RELEASE_CANDIDATE_SCHEMA.to_owned(),
            contract: RELEASE_CANDIDATE_CONTRACT.to_owned(),
            source: SourceIdentity {
                commit: "a".repeat(40),
            },
            release: CandidateReleaseIdentity {
                commit: "b".repeat(40),
                parent: "a".repeat(40),
                tree: "c".repeat(40),
            },
            plan: PlanInventory {
                file: RELEASE_PLAN_FILE.to_owned(),
                digest: format!("sha256:{}", "1".repeat(64)),
                sha256: format!("sha256:{}", "2".repeat(64)),
            },
            global_tag: GlobalTag {
                id: "intentional/primary".to_owned(),
                name: "1.2.3".to_owned(),
                object: "d".repeat(40),
                target: "b".repeat(40),
            },
            changed_tree: vec![ChangedPath {
                path: "CHANGELOG.md".to_owned(),
                status: ChangeStatus::Modified,
                digest: Some(format!("sha256:{}", "3".repeat(64))),
            }],
            git_bundle: BundleInventory {
                file: RELEASE_BUNDLE_FILE.to_owned(),
                sha256: format!("sha256:{}", "4".repeat(64)),
                heads: vec![BUNDLE_RELEASE_HEAD.to_owned(), BUNDLE_TAG_HEAD.to_owned()],
            },
            files: vec![
                CandidateFile {
                    path: RELEASE_PLAN_FILE.to_owned(),
                    sha256: format!("sha256:{}", "2".repeat(64)),
                    size: 256,
                },
                CandidateFile {
                    path: RELEASE_BUNDLE_FILE.to_owned(),
                    sha256: format!("sha256:{}", "4".repeat(64)),
                    size: 512,
                },
            ],
        }
    }

    #[test]
    fn accepts_a_complete_candidate() {
        candidate().validate().expect("candidate is valid");
    }

    #[test]
    fn round_trips_through_yaml() {
        let text = candidate().to_yaml().expect("serialize");
        assert!(text.contains("$schema:"));
        assert!(text.contains("global-tag:"));
        assert!(text.contains("changed-tree:"));
        assert!(text.contains("git-bundle:"));
        assert_eq!(
            ReleaseCandidate::from_yaml(&text).expect("parse"),
            candidate()
        );
    }

    #[test]
    fn rejects_an_unknown_schema() {
        let mut value = candidate();
        value.schema = "https://example.invalid/schema".to_owned();
        assert!(value.validate().is_err());
    }

    #[test]
    fn rejects_a_parent_that_is_not_the_source() {
        let mut value = candidate();
        value.release.parent = "e".repeat(40);
        assert!(value.validate().is_err());
    }

    #[test]
    fn rejects_a_tag_that_does_not_target_the_release_commit() {
        let mut value = candidate();
        value.global_tag.target = "e".repeat(40);
        assert!(value.validate().is_err());
    }

    #[test]
    fn rejects_unexpected_bundle_heads() {
        let mut value = candidate();
        value.git_bundle.heads = vec![BUNDLE_RELEASE_HEAD.to_owned(), "refs/heads/main".to_owned()];
        assert!(value.validate().is_err());
    }

    #[test]
    fn rejects_an_incomplete_bundle_head_set() {
        let mut value = candidate();
        value.git_bundle.heads = vec![BUNDLE_RELEASE_HEAD.to_owned()];
        assert!(value.validate().is_err());
    }

    #[test]
    fn rejects_uninventoried_transport_files() {
        let mut value = candidate();
        value.files.retain(|file| file.path != RELEASE_BUNDLE_FILE);
        assert!(value.validate().is_err());
    }

    #[test]
    fn rejects_unordered_inventories() {
        let mut value = candidate();
        value.files.reverse();
        assert!(value.validate().is_err());
    }

    #[test]
    fn rejects_a_deleted_path_with_a_digest() {
        let mut value = candidate();
        value.changed_tree[0].status = ChangeStatus::Deleted;
        assert!(value.validate().is_err());
    }

    #[test]
    fn rejects_traversal_and_metadata_paths() {
        for path in [
            "../escape",
            "/absolute",
            "candidate//double",
            "candidate/./here",
            ".git/config",
            "candidate/.GIT/hooks",
        ] {
            assert!(
                validate_path("path", path).is_err(),
                "expected {path} to be rejected"
            );
        }
        validate_path("path", "candidate/crates/cli/Cargo.toml").expect("contained path");
    }

    #[test]
    fn rejects_unsafe_tag_names() {
        for name in [
            "", "-tag", "bad name", "a..b", "tag.lock", "ref^", "tag@{0}",
        ] {
            assert!(
                validate_tag_name(name).is_err(),
                "expected {name} to be rejected"
            );
        }
        validate_tag_name("1.2.3").expect("plain version tag");
        validate_tag_name("intentional/1.2.3").expect("namespaced tag");
    }
}
