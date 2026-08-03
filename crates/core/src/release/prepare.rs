// ---
// relationships:
//   implements: github-release-executor
// ---

//! Deterministic release preparation and release-candidate handoff construction.
//!
//! Preparation performs no network mutation. It proves that the remote default
//! branch still identifies the accepted source commit, seals one release plan,
//! constructs the deterministic release commit and its annotated global tag as
//! local objects, and writes the schema-backed handoff that a later privileged
//! job verifies before pushing.

use crate::error::{Error, Result};
use crate::release::build::build_candidate;
use crate::release::candidate::{
    digest_bytes, BundleInventory, CandidateFile, CandidateReleaseIdentity, GlobalTag,
    PlanInventory, ReleaseCandidate, SourceIdentity, BUNDLE_RELEASE_HEAD, BUNDLE_TAG_HEAD,
    CANDIDATE_TREE_DIRECTORY, MAX_BUNDLE_BYTES, MAX_CANDIDATE_FILES, MAX_CANDIDATE_FILE_BYTES,
    RELEASE_BUNDLE_FILE, RELEASE_CANDIDATE_CONTRACT, RELEASE_CANDIDATE_MANIFEST,
    RELEASE_CANDIDATE_SCHEMA, RELEASE_PLAN_FILE,
};
use crate::release::git::{self, GitCommand};
use std::path::{Path, PathBuf};

/// Remote whose default branch supplies and re-proves the accepted source commit.
const SOURCE_REMOTE: &str = "origin";

/// A prepared release candidate and the handoff it wrote.
#[derive(Debug, Clone)]
pub struct PreparedRelease {
    /// Directory holding the complete handoff.
    pub directory: PathBuf,
    /// Manifest file inside that directory.
    pub path: PathBuf,
    /// The authoritative release-candidate manifest.
    pub candidate: ReleaseCandidate,
}

impl PreparedRelease {
    /// Stable identity lines the credential-free prepare Action projects.
    pub fn projections(&self) -> Vec<String> {
        vec![
            format!("candidate-path: {}", self.directory.display()),
            format!("source-sha: {}", self.candidate.source.commit),
            format!("release-sha: {}", self.candidate.release.commit),
            format!("global-tag: {}", self.candidate.global_tag.name),
            format!("plan-digest: {}", self.candidate.plan.digest),
        ]
    }
}

/// Resolve the source commit, seal one plan, and write the release-candidate handoff.
pub fn prepare_release(root: &Path, output: &Path) -> Result<PreparedRelease> {
    // Dropping `.` components keeps the projected candidate path readable for
    // the workflow steps that consume it as an artifact location.
    let directory: PathBuf = if output.is_absolute() {
        output.components().collect()
    } else {
        root.join(output).components().collect()
    };
    require_empty_directory(&directory)?;

    let source = git::resolve(root, "HEAD^{commit}")?;
    require_clean_worktree(root, &directory)?;
    require_remote_default_branch(root, &source)?;

    let built = build_candidate(root, &source)?;

    create_tag_reference(root, &built.tag_name, &built.tag_object)?;
    let bundle = directory.join(RELEASE_BUNDLE_FILE);
    write_bundle(
        root,
        &source,
        &built.release_commit,
        &built.tag_object,
        &built.tag_name,
        &bundle,
    )?;

    let plan_bytes = format!("{}\n", built.plan_json).into_bytes();
    write_file(&directory.join(RELEASE_PLAN_FILE), &plan_bytes)?;
    for (path, contents) in &built.materialized {
        write_file(
            &directory.join(CANDIDATE_TREE_DIRECTORY).join(path),
            contents.as_bytes(),
        )?;
    }

    let files = inventory(&directory)?;
    let bundle_digest = files
        .iter()
        .find(|file| file.path == RELEASE_BUNDLE_FILE)
        .map(|file| file.sha256.clone())
        .ok_or_else(|| Error::Validation("the git bundle was not written".to_owned()))?;

    let candidate = ReleaseCandidate {
        schema: RELEASE_CANDIDATE_SCHEMA.to_owned(),
        contract: RELEASE_CANDIDATE_CONTRACT.to_owned(),
        source: SourceIdentity {
            commit: source.clone(),
        },
        release: CandidateReleaseIdentity {
            commit: built.release_commit.clone(),
            parent: source,
            tree: built.release_tree.clone(),
        },
        plan: PlanInventory {
            file: RELEASE_PLAN_FILE.to_owned(),
            digest: built.plan.digest.clone(),
            sha256: digest_bytes(&plan_bytes),
        },
        global_tag: GlobalTag {
            id: built.tag_id.clone(),
            name: built.tag_name.clone(),
            object: built.tag_object.clone(),
            target: built.release_commit.clone(),
        },
        changed_tree: built.changed_tree.clone(),
        git_bundle: BundleInventory {
            file: RELEASE_BUNDLE_FILE.to_owned(),
            sha256: bundle_digest,
            heads: vec![BUNDLE_RELEASE_HEAD.to_owned(), BUNDLE_TAG_HEAD.to_owned()],
        },
        files,
    };
    let path = directory.join(RELEASE_CANDIDATE_MANIFEST);
    write_file(&path, candidate.to_yaml()?.as_bytes())?;
    Ok(PreparedRelease {
        directory,
        path,
        candidate,
    })
}

/// Refuse to write a handoff into an existing non-empty directory.
fn require_empty_directory(directory: &Path) -> Result<()> {
    match std::fs::read_dir(directory) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Err(Error::Validation(format!(
                    "release candidate output {} already contains files",
                    directory.display()
                )));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(directory).map_err(|error| Error::io(directory, error))
        }
        Err(error) => Err(Error::io(directory, error)),
    }
}

/// Refuse to prepare a release from a workspace that is not exactly the source commit.
fn require_clean_worktree(root: &Path, output: &Path) -> Result<()> {
    let root_canonical = root
        .canonicalize()
        .map_err(|error| Error::io(root, error))?;
    let output_canonical = output
        .canonicalize()
        .map_err(|error| Error::io(output, error))?;
    let status = GitCommand::new(root)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .run()?;
    let text = status.text()?;
    let mut records = text.split('\0').filter(|value| !value.is_empty());
    let mut dirty = Vec::new();
    while let Some(record) = records.next() {
        let Some(path) = record.get(3..) else {
            return Err(Error::Git(format!(
                "git status produced an unreadable record: {record}"
            )));
        };
        // A rename record is followed by its original path in the same stream.
        if record.starts_with(['R', 'C']) || record[1..].starts_with(['R', 'C']) {
            records.next();
        }
        if root_canonical.join(path).starts_with(&output_canonical) {
            continue;
        }
        dirty.push(path.to_owned());
    }
    if dirty.is_empty() {
        return Ok(());
    }
    dirty.sort();
    dirty.truncate(10);
    Err(Error::Validation(format!(
        "release preparation requires a workspace that matches the accepted source commit; found {}",
        dirty.join(", ")
    )))
}

/// Prove that the remote default branch still identifies the accepted source commit.
fn require_remote_default_branch(root: &Path, source: &str) -> Result<()> {
    let output = GitCommand::new(root)
        .args(["ls-remote", "--symref", SOURCE_REMOTE, "HEAD"])
        .run()?;
    let text = output.text()?;
    let mut default_branch = None;
    let mut head = None;
    for line in text.lines() {
        if let Some(value) = line.strip_prefix("ref: ") {
            if let Some((reference, name)) = value.split_once('\t') {
                if name.trim() == "HEAD" {
                    default_branch = Some(reference.to_owned());
                }
            }
            continue;
        }
        if let Some((object, name)) = line.split_once('\t') {
            if name.trim() == "HEAD" {
                head = Some(object.to_owned());
            }
        }
    }
    let head = head.ok_or_else(|| {
        Error::Git(format!(
            "remote {SOURCE_REMOTE} did not report a default branch head"
        ))
    })?;
    if head != source {
        return Err(Error::Validation(format!(
            "remote {SOURCE_REMOTE} default branch {} identifies {head}, not the accepted source commit {source}",
            default_branch.as_deref().unwrap_or("HEAD")
        )));
    }
    Ok(())
}

/// Create the local annotated global tag, accepting an identical existing record.
fn create_tag_reference(root: &Path, name: &str, object: &str) -> Result<()> {
    let reference = format!("refs/tags/{name}");
    let existing = GitCommand::new(root)
        .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
        .arg(&reference)
        .output()?;
    if existing.succeeded() {
        let current = existing.line()?;
        if current == object {
            return Ok(());
        }
        return Err(Error::Validation(format!(
            "tag {name} already exists as {current} and does not match the prepared release record {object}"
        )));
    }
    GitCommand::new(root)
        .args(["update-ref", &reference, object])
        .run()?;
    Ok(())
}

/// Write the thin Git bundle carrying only the release commit and its tag object.
fn write_bundle(
    root: &Path,
    source: &str,
    release: &str,
    tag_object: &str,
    tag_name: &str,
    bundle: &Path,
) -> Result<()> {
    let transport_tag_is_release_tag = format!("refs/tags/{tag_name}") == BUNDLE_TAG_HEAD;
    GitCommand::new(root)
        .args(["update-ref", BUNDLE_RELEASE_HEAD, release])
        .run()?;
    if !transport_tag_is_release_tag {
        GitCommand::new(root)
            .args(["update-ref", BUNDLE_TAG_HEAD, tag_object])
            .run()?;
    }
    let path = bundle
        .to_str()
        .ok_or_else(|| Error::Validation("the bundle path is not valid UTF-8".to_owned()))?;
    let created = GitCommand::new(root)
        .args(["bundle", "create", path, &format!("^{source}")])
        .args([BUNDLE_RELEASE_HEAD, BUNDLE_TAG_HEAD])
        .output()?;
    GitCommand::new(root)
        .args(["update-ref", "-d", BUNDLE_RELEASE_HEAD])
        .run()?;
    if !transport_tag_is_release_tag {
        GitCommand::new(root)
            .args(["update-ref", "-d", BUNDLE_TAG_HEAD])
            .run()?;
    }
    if !created.succeeded() {
        return Err(Error::Git(format!(
            "failed to create the release bundle: {}",
            created.diagnostic()
        )));
    }
    let size = std::fs::metadata(bundle)
        .map_err(|error| Error::io(bundle, error))?
        .len();
    if size > MAX_BUNDLE_BYTES {
        return Err(Error::Validation(format!(
            "the release bundle is {size} bytes and exceeds the {MAX_BUNDLE_BYTES} byte handoff bound"
        )));
    }
    Ok(())
}

/// Write one handoff file, creating its parent directories.
fn write_file(path: &Path, contents: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
    }
    std::fs::write(path, contents).map_err(|error| Error::io(path, error))
}

/// Inventory every transported file except the manifest itself.
fn inventory(directory: &Path) -> Result<Vec<CandidateFile>> {
    let mut files = Vec::new();
    collect(directory, directory, &mut files)?;
    files.sort();
    if files.len() > MAX_CANDIDATE_FILES {
        return Err(Error::Validation(format!(
            "the release candidate transports {} files and exceeds the {MAX_CANDIDATE_FILES} file bound",
            files.len()
        )));
    }
    Ok(files)
}

fn collect(root: &Path, directory: &Path, files: &mut Vec<CandidateFile>) -> Result<()> {
    let entries = std::fs::read_dir(directory).map_err(|error| Error::io(directory, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| Error::io(directory, error))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|error| Error::io(&path, error))?;
        if kind.is_dir() {
            collect(root, &path, files)?;
            continue;
        }
        if !kind.is_file() {
            return Err(Error::Validation(format!(
                "release candidate entry {} is not a regular file",
                path.display()
            )));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| {
                Error::Validation(format!(
                    "release candidate entry {} left the handoff directory",
                    path.display()
                ))
            })?
            .to_str()
            .ok_or_else(|| {
                Error::Validation(format!(
                    "release candidate entry {} is not valid UTF-8",
                    path.display()
                ))
            })?
            .replace('\\', "/");
        if relative == RELEASE_CANDIDATE_MANIFEST {
            continue;
        }
        let contents = std::fs::read(&path).map_err(|error| Error::io(&path, error))?;
        if contents.len() as u64 > MAX_CANDIDATE_FILE_BYTES {
            return Err(Error::Validation(format!(
                "release candidate file {relative} exceeds the {MAX_CANDIDATE_FILE_BYTES} byte bound"
            )));
        }
        files.push(CandidateFile {
            path: relative,
            sha256: digest_bytes(&contents),
            size: contents.len() as u64,
        });
    }
    Ok(())
}
