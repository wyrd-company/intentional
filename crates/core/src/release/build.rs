// ---
// relationships:
//   implements: github-release-executor
// ---

//! Deterministic construction of the release commit and its annotated global tag.
//!
//! Preparation and independent handoff verification run exactly this code. A
//! candidate is a pure function of the accepted source commit and the
//! repository state that commit contains, so verification can rebuild it in a
//! fresh clone and compare identities rather than trust transported values.

use crate::apply::ApplyResult;
use crate::error::{Error, Result};
use crate::plan::{PlanTag, ReleasePlan};
use crate::release::candidate::{digest_bytes, validate_path, ChangeStatus, ChangedPath};
use crate::release::git::{self, GitCommand};
use crate::tag::release_tag_message;
use std::collections::BTreeMap;
use std::path::Path;

/// Author and committer name recorded on every deterministic release commit.
///
/// A release commit must be reproducible from its accepted source commit
/// alone, so its Git identity is a fixed protocol value rather than the
/// identity of whichever runner or operator constructed it.
pub const RELEASE_IDENTITY_NAME: &str = "Intentional";
/// Author and committer address recorded on every deterministic release commit.
pub const RELEASE_IDENTITY_EMAIL: &str = "releases@intentional.foo";

/// Regular file modes a release may rewrite.
const REGULAR_FILE_MODE: &str = "100644";
const EXECUTABLE_FILE_MODE: &str = "100755";
const ABSENT_OBJECT: &str = "0000000000000000000000000000000000000000";

/// A deterministic release candidate rebuilt from one accepted source commit.
pub(crate) struct BuiltCandidate {
    /// Sealed release plan.
    pub(crate) plan: ReleasePlan,
    /// Canonical JSON bytes of the sealed plan.
    pub(crate) plan_json: String,
    /// Release commit R.
    pub(crate) release_commit: String,
    /// Tree of R.
    pub(crate) release_tree: String,
    /// Annotated global tag object.
    pub(crate) tag_object: String,
    /// Canonical configured id of the global tag.
    pub(crate) tag_id: String,
    /// Rendered name of the global tag.
    pub(crate) tag_name: String,
    /// Changed-tree evidence in path order.
    pub(crate) changed_tree: Vec<ChangedPath>,
    /// Materialized release content for every added or modified path.
    pub(crate) materialized: BTreeMap<String, String>,
}

/// Rebuild the deterministic release candidate rooted at `directory` from `source`.
pub(crate) fn build_candidate(directory: &Path, source: &str) -> Result<BuiltCandidate> {
    validate_object_identity(source)?;
    let applied = ApplyResult::build(directory, None)?;
    if applied.writes.is_empty() && applied.deletes.is_empty() {
        return Err(Error::Validation(
            "the release plan changes no files, so there is no release candidate to prepare"
                .to_owned(),
        ));
    }
    let plan = applied.plan.clone();
    let plan_json = plan.to_canonical_json()?;
    let (tag_id, tag_name) = global_tag(&plan)?;

    let mut writes = BTreeMap::new();
    for write in &applied.writes {
        let path = relative_path("release path", &write.path)?;
        writes.insert(path, write.contents.clone());
    }
    let mut deletes = Vec::new();
    for delete in &applied.deletes {
        let path = relative_path("consumed intent path", delete)?;
        if writes.contains_key(&path) {
            return Err(Error::Validation(format!(
                "release path {path} is both written and deleted"
            )));
        }
        deletes.push(path);
    }
    deletes.sort();

    let mut addressed = writes.keys().cloned().collect::<Vec<_>>();
    addressed.extend(deletes.iter().cloned());
    let source_modes = source_tree_modes(directory, source, &addressed)?;

    let index = tempfile::Builder::new()
        .prefix("intentional-release-index")
        .tempdir()
        .map_err(|error| Error::Git(format!("failed to create a release index: {error}")))?;
    let index_file = index.path().join("index");
    let index_file = index_file
        .to_str()
        .ok_or_else(|| Error::Git("the release index path is not valid UTF-8".to_owned()))?;

    GitCommand::new(directory)
        .args(["read-tree", source])
        .env("GIT_INDEX_FILE", index_file)
        .run()?;

    let mut entries = String::new();
    for (path, contents) in &writes {
        let mode = match source_modes.get(path.as_str()) {
            None => REGULAR_FILE_MODE,
            Some(mode) if mode == REGULAR_FILE_MODE || mode == EXECUTABLE_FILE_MODE => mode,
            Some(mode) => {
                return Err(Error::Validation(format!(
                    "release path {path} is not a regular file in the accepted source commit; got mode {mode}"
                )))
            }
        };
        let blob = GitCommand::new(directory)
            .args(["hash-object", "-w", "--stdin", "--path", path])
            .stdin(contents.clone().into_bytes())
            .run()?
            .line()?;
        entries.push_str(&format!("{mode} {blob}\t{path}\n"));
    }
    for path in &deletes {
        if !source_modes.contains_key(path.as_str()) {
            return Err(Error::Validation(format!(
                "consumed intent {path} is not tracked by the accepted source commit"
            )));
        }
        entries.push_str(&format!("0 {ABSENT_OBJECT}\t{path}\n"));
    }
    GitCommand::new(directory)
        .args(["update-index", "--index-info"])
        .env("GIT_INDEX_FILE", index_file)
        .stdin(entries.into_bytes())
        .run()?;
    let release_tree = GitCommand::new(directory)
        .arg("write-tree")
        .env("GIT_INDEX_FILE", index_file)
        .run()?
        .line()?;

    let source_tree = git::resolve(directory, &format!("{source}^{{tree}}"))?;
    if release_tree == source_tree {
        return Err(Error::Validation(
            "the release candidate reproduces the accepted source tree, so no release commit exists"
                .to_owned(),
        ));
    }

    let timestamp = GitCommand::new(directory)
        .args(["show", "--no-patch", "--format=%ct", source])
        .run()?
        .line()?;
    if timestamp.is_empty() || !timestamp.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::Git(format!(
            "the accepted source commit has no usable commit timestamp; got {timestamp}"
        )));
    }
    let date = format!("{timestamp} +0000");
    let release_commit = GitCommand::new(directory)
        .args(["commit-tree", &release_tree, "-p", source])
        .env("GIT_AUTHOR_NAME", RELEASE_IDENTITY_NAME)
        .env("GIT_AUTHOR_EMAIL", RELEASE_IDENTITY_EMAIL)
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_NAME", RELEASE_IDENTITY_NAME)
        .env("GIT_COMMITTER_EMAIL", RELEASE_IDENTITY_EMAIL)
        .env("GIT_COMMITTER_DATE", &date)
        .stdin(release_commit_message(&plan).into_bytes())
        .run()?
        .line()?;

    let version = plan
        .tags
        .iter()
        .find(|tag| tag.id == tag_id)
        .map(|tag| tag.version.clone())
        .ok_or_else(|| Error::Validation(format!("release plan does not contain tag {tag_id}")))?;
    let message = release_tag_message(&plan.contract, &plan.digest, &tag_id, &version);
    let tag_object = GitCommand::new(directory)
        .arg("mktag")
        .stdin(
            format!(
                "object {release_commit}\ntype commit\ntag {tag_name}\ntagger {RELEASE_IDENTITY_NAME} <{RELEASE_IDENTITY_EMAIL}> {date}\n\n{message}"
            )
            .into_bytes(),
        )
        .run()?
        .line()?;

    let mut changed_tree = Vec::new();
    for (path, contents) in &writes {
        changed_tree.push(ChangedPath {
            path: path.clone(),
            status: if source_modes.contains_key(path.as_str()) {
                ChangeStatus::Modified
            } else {
                ChangeStatus::Added
            },
            digest: Some(digest_bytes(contents.as_bytes())),
        });
    }
    for path in &deletes {
        changed_tree.push(ChangedPath {
            path: path.clone(),
            status: ChangeStatus::Deleted,
            digest: None,
        });
    }
    changed_tree.sort_by(|left, right| left.path.cmp(&right.path));

    Ok(BuiltCandidate {
        plan,
        plan_json,
        release_commit,
        release_tree,
        tag_object,
        tag_id,
        tag_name,
        changed_tree,
        materialized: writes,
    })
}

/// Reject any revision that is not already a complete hexadecimal object identity.
fn validate_object_identity(value: &str) -> Result<()> {
    let valid = matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if valid {
        return Ok(());
    }
    Err(Error::Git(format!(
        "a release candidate must be built from a complete object identity; got {value}"
    )))
}

/// The single unphased release tag the executor publishes with the release commit.
fn global_tag(plan: &ReleasePlan) -> Result<(String, String)> {
    let unphased = plan
        .tags
        .iter()
        .filter(|tag| tag.require_phase.is_none())
        .collect::<Vec<&PlanTag>>();
    match unphased.as_slice() {
        [tag] => Ok((tag.id.clone(), tag.name.clone())),
        [] => Err(Error::Validation(
            "the GitHub executor requires one unphased release tag to publish as the global release tag; the release plan has none"
                .to_owned(),
        )),
        tags => Err(Error::Validation(format!(
            "the GitHub executor requires exactly one unphased release tag as the global release tag; the release plan has {}",
            tags.iter()
                .map(|tag| tag.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// The canonical release commit message bound to the sealed plan.
fn release_commit_message(plan: &ReleasePlan) -> String {
    let mut message = match plan.release_units.as_slice() {
        [only] => format!("chore(release): apply {}\n\n", only.new_version),
        units => format!("chore(release): apply {} release units\n\n", units.len()),
    };
    for release_unit in &plan.release_units {
        message.push_str(&format!(
            "- {} {}\n",
            release_unit.id, release_unit.new_version
        ));
    }
    message.push_str(&format!("\nRelease-Plan: {}\n", plan.digest));
    message
}

/// File modes of the addressed paths in the accepted source tree.
fn source_tree_modes(
    directory: &Path,
    source: &str,
    paths: &[String],
) -> Result<BTreeMap<String, String>> {
    let mut modes = BTreeMap::new();
    if paths.is_empty() {
        return Ok(modes);
    }
    let output = GitCommand::new(directory)
        .args(["ls-tree", "-r", "-z", "--full-tree", source, "--"])
        .args(paths.iter().map(|path| format!(":(literal){path}")))
        .run()?;
    for record in output.text()?.split('\0').filter(|value| !value.is_empty()) {
        let (metadata, path) = record.split_once('\t').ok_or_else(|| {
            Error::Git(format!(
                "git ls-tree produced an unreadable record: {record}"
            ))
        })?;
        let mode = metadata.split(' ').next().unwrap_or_default();
        modes.insert(path.to_owned(), mode.to_owned());
    }
    Ok(modes)
}

/// Convert a workspace-relative path into a validated forward-slash string.
fn relative_path(label: &str, path: &Path) -> Result<String> {
    let mut segments = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(value) => segments.push(
                value
                    .to_str()
                    .ok_or_else(|| {
                        Error::Validation(format!("{label} {} is not valid UTF-8", path.display()))
                    })?
                    .to_owned(),
            ),
            _ => {
                return Err(Error::Validation(format!(
                    "{label} {} must be a contained relative path",
                    path.display()
                )))
            }
        }
    }
    let value = segments.join("/");
    validate_path(label, &value)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Bump;
    use crate::plan::{Generator, PlanReleaseUnit};

    fn plan(release_units: Vec<PlanReleaseUnit>) -> ReleasePlan {
        ReleasePlan {
            digest: format!("sha256:{}", "1".repeat(64)),
            contract: "contract-1".to_owned(),
            generator: Generator {
                tool: "intentional".to_owned(),
                version: "0.0.0".to_owned(),
            },
            channel: None,
            release_units,
            tags: Vec::new(),
            tag_order: Vec::new(),
        }
    }

    fn release_unit(id: &str, version: &str) -> PlanReleaseUnit {
        PlanReleaseUnit {
            id: id.to_owned(),
            old_version: "0.0.1".to_owned(),
            new_version: version.to_owned(),
            bump: Bump::Patch,
            contributing_intent_ids: Vec::new(),
            tag_ids: Vec::new(),
            release_notes: String::new(),
        }
    }

    #[test]
    fn names_a_single_release_unit_version_in_the_commit_subject() {
        let message = release_commit_message(&plan(vec![release_unit("alpha", "1.2.3")]));
        assert!(message.starts_with("chore(release): apply 1.2.3\n\n"));
        assert!(message.contains("\n- alpha 1.2.3\n"));
        assert!(message.ends_with(&format!("\nRelease-Plan: sha256:{}\n", "1".repeat(64))));
    }

    #[test]
    fn counts_release_units_in_a_multiple_release_unit_subject() {
        let message = release_commit_message(&plan(vec![
            release_unit("alpha", "1.2.3"),
            release_unit("beta", "2.0.0"),
        ]));
        assert!(message.starts_with("chore(release): apply 2 release units\n\n"));
        assert!(message.contains("\n- alpha 1.2.3\n- beta 2.0.0\n"));
    }

    #[test]
    fn requires_exactly_one_unphased_release_tag() {
        let mut value = plan(vec![release_unit("alpha", "1.2.3")]);
        assert!(global_tag(&value).is_err());
        value.tags.push(PlanTag {
            id: "alpha/primary".to_owned(),
            name: "1.2.3".to_owned(),
            version: "1.2.3".to_owned(),
            release_unit: Some("alpha".to_owned()),
            role: Some(crate::model::TagRole::Primary),
            require_phase: None,
            tag_after: Vec::new(),
        });
        assert_eq!(
            global_tag(&value).expect("one unphased tag"),
            ("alpha/primary".to_owned(), "1.2.3".to_owned())
        );
        value.tags.push(PlanTag {
            id: "beta/primary".to_owned(),
            name: "beta-1.2.3".to_owned(),
            version: "1.2.3".to_owned(),
            release_unit: Some("beta".to_owned()),
            role: Some(crate::model::TagRole::Primary),
            require_phase: None,
            tag_after: Vec::new(),
        });
        assert!(global_tag(&value).is_err());
    }

    #[test]
    fn rejects_paths_that_leave_the_workspace() {
        assert!(relative_path("release path", Path::new("../escape")).is_err());
        assert!(relative_path("release path", Path::new("/etc/passwd")).is_err());
        assert_eq!(
            relative_path("release path", Path::new("crates/cli/Cargo.toml")).expect("contained"),
            "crates/cli/Cargo.toml"
        );
    }
}
