// ---
// relationships:
//   implements: github-release-executor
// ---

//! Independent verification of a prepared release-candidate handoff.
//!
//! The handoff arrives from an unprivileged job as a workflow artifact, so
//! every value in it is untrusted. Verification proves the transported files,
//! validates the Git bundle before importing it, materializes the declared
//! objects in an isolated clone, rebuilds the candidate from the accepted
//! source commit alone, and only then imports the pushable identities into the
//! repository the privileged job will push from.

use crate::error::{Error, Result};
use crate::plan::ReleasePlan;
use crate::release::build::build_candidate;
use crate::release::candidate::{
    digest_bytes, ReleaseCandidate, BUNDLE_RELEASE_HEAD, BUNDLE_TAG_HEAD, CANDIDATE_TREE_DIRECTORY,
    IMPORTED_RELEASE_REF, MAX_BUNDLE_BYTES, MAX_CANDIDATE_DEPTH, MAX_CANDIDATE_FILES,
    MAX_CANDIDATE_FILE_BYTES, RELEASE_CANDIDATE_MANIFEST,
};
use crate::release::git::{self, GitCommand};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Refs the isolated import clone uses while proving untrusted objects.
const STAGED_RELEASE_REF: &str = "refs/intentional/verify/release";
const STAGED_TAG_REF: &str = "refs/intentional/verify/global-tag";

/// A release handoff that reproduced every candidate invariant.
#[derive(Debug, Clone)]
pub struct VerifiedHandoff {
    /// Verified handoff directory.
    pub directory: PathBuf,
    /// Accepted source commit S.
    pub source: String,
    /// Deterministic release commit R.
    pub release: String,
    /// Rendered name of the annotated global release tag.
    pub global_tag: String,
    /// Digest sealed inside the release plan.
    pub plan_digest: String,
}

impl VerifiedHandoff {
    /// Stable identity lines the credential-free verify Action projects.
    pub fn projections(&self) -> Vec<String> {
        vec![
            format!("source-sha: {}", self.source),
            format!("release-sha: {}", self.release),
            format!("global-tag: {}", self.global_tag),
            format!("plan-digest: {}", self.plan_digest),
        ]
    }
}

/// Verify a release-candidate handoff and import its pushable identities.
pub fn verify_handoff(root: &Path, handoff: &Path) -> Result<VerifiedHandoff> {
    let directory = if handoff.is_absolute() {
        handoff.to_path_buf()
    } else {
        root.join(handoff)
    };
    let directory = directory
        .canonicalize()
        .map_err(|error| Error::io(&directory, error))?;
    let manifest_path = directory.join(RELEASE_CANDIDATE_MANIFEST);
    let manifest_text = std::fs::read_to_string(&manifest_path)
        .map_err(|error| Error::io(&manifest_path, error))?;
    let candidate = ReleaseCandidate::from_yaml(&manifest_text)?;

    // A conflicting local ref is reported before any untrusted object is read,
    // so an operator sees the real obstacle rather than a reproduction failure.
    require_importable_refs(root, &candidate)?;
    verify_transported_files(&directory, &candidate)?;
    let plan = verify_sealed_plan(&directory, &candidate)?;

    let bundle = directory.join(&candidate.git_bundle.file);
    let import_root = tempfile::Builder::new()
        .prefix("intentional-handoff-import")
        .tempdir()
        .map_err(|error| Error::Git(format!("failed to create an import clone: {error}")))?;
    let import = isolated_clone(root, import_root.path(), &candidate.source.commit)?;
    verify_bundle(&import, &bundle, &candidate)?;
    verify_imported_objects(&import, &candidate)?;

    let reproduction_root = tempfile::Builder::new()
        .prefix("intentional-handoff-reproduce")
        .tempdir()
        .map_err(|error| Error::Git(format!("failed to create a reproduction clone: {error}")))?;
    let reproduction = isolated_clone(root, reproduction_root.path(), &candidate.source.commit)?;
    reproduce_candidate(&reproduction, &directory, &candidate, &plan)?;
    drop(reproduction_root);

    // The pushable objects are taken from the clone that already proved them,
    // never from a second read of the untrusted handoff directory, so the
    // bundle cannot be swapped between verification and import.
    import_pushable_identities(root, &import, &candidate)?;
    drop(import_root);

    Ok(VerifiedHandoff {
        directory,
        source: candidate.source.commit.clone(),
        release: candidate.release.commit.clone(),
        global_tag: candidate.global_tag.name.clone(),
        plan_digest: candidate.plan.digest,
    })
}

/// Prove the handoff directory contains exactly its inventoried files.
fn verify_transported_files(directory: &Path, candidate: &ReleaseCandidate) -> Result<()> {
    let mut present = BTreeSet::new();
    collect(directory, directory, 0, &mut 0, &mut present)?;
    let inventoried = candidate
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    let unexpected = present
        .difference(&inventoried)
        .cloned()
        .collect::<Vec<_>>();
    if !unexpected.is_empty() {
        return Err(Error::Validation(format!(
            "the release handoff transports uninventoried files: {}",
            unexpected.join(", ")
        )));
    }
    verify_bundle_bound(directory, candidate)?;
    for file in &candidate.files {
        let path = directory.join(&file.path);
        // Bound the read by what is on disk rather than by what the manifest
        // declares, so an oversized transported file cannot be read into memory
        // on the strength of an understated inventory entry.
        let present = std::fs::metadata(&path)
            .map_err(|error| Error::io(&path, error))?
            .len();
        if present > MAX_CANDIDATE_FILE_BYTES {
            return Err(Error::Validation(format!(
                "release handoff file {} is {present} bytes and exceeds the {MAX_CANDIDATE_FILE_BYTES} byte bound",
                file.path
            )));
        }
        let contents = std::fs::read(&path).map_err(|error| Error::io(&path, error))?;
        if contents.len() as u64 != file.size {
            return Err(Error::Validation(format!(
                "release handoff file {} is {} bytes and does not match its inventoried size {}",
                file.path,
                contents.len(),
                file.size
            )));
        }
        let digest = digest_bytes(&contents);
        if digest != file.sha256 {
            return Err(Error::Validation(format!(
                "release handoff file {} does not match its inventoried digest",
                file.path
            )));
        }
        if file.path == candidate.git_bundle.file && digest != candidate.git_bundle.sha256 {
            return Err(Error::Validation(
                "the inventoried git bundle digest and the declared git bundle digest disagree"
                    .to_owned(),
            ));
        }
    }
    Ok(())
}

/// Bound the transported bundle by both its declared and its observed size.
///
/// The bundle bound is checked before any transported file is read, so an
/// oversized bundle is refused rather than digested.
fn verify_bundle_bound(directory: &Path, candidate: &ReleaseCandidate) -> Result<()> {
    let declared = candidate.bundle_size().ok_or_else(|| {
        Error::Validation("the git bundle is missing from the file inventory".to_owned())
    })?;
    let path = directory.join(&candidate.git_bundle.file);
    let observed = std::fs::metadata(&path)
        .map_err(|error| Error::io(&path, error))?
        .len();
    for size in [declared, observed] {
        if size > MAX_BUNDLE_BYTES {
            return Err(Error::Validation(format!(
                "the release bundle is {size} bytes and exceeds the {MAX_BUNDLE_BYTES} byte handoff bound"
            )));
        }
    }
    Ok(())
}

/// Prove the transported plan seals the digest the manifest binds.
fn verify_sealed_plan(directory: &Path, candidate: &ReleaseCandidate) -> Result<ReleasePlan> {
    let path = directory.join(&candidate.plan.file);
    let contents = std::fs::read(&path).map_err(|error| Error::io(&path, error))?;
    if digest_bytes(&contents) != candidate.plan.sha256 {
        return Err(Error::Validation(
            "the transported release plan does not match its inventoried digest".to_owned(),
        ));
    }
    let text = std::str::from_utf8(&contents).map_err(|error| {
        Error::Validation(format!("the release plan is not valid UTF-8: {error}"))
    })?;
    let plan: ReleasePlan = serde_json::from_str(text)
        .map_err(|error| Error::Validation(format!("invalid release plan: {error}")))?;
    plan.verify_digest()?;
    if plan.digest != candidate.plan.digest {
        return Err(Error::Validation(format!(
            "the release candidate binds plan digest {} but the transported plan seals {}",
            candidate.plan.digest, plan.digest
        )));
    }
    Ok(plan)
}

/// Create an isolated clone that already contains the accepted source commit.
fn isolated_clone(root: &Path, into: &Path, source: &str) -> Result<PathBuf> {
    let target = into.join("repository");
    let target_argument = target
        .to_str()
        .ok_or_else(|| Error::Git("the clone path is not valid UTF-8".to_owned()))?
        .to_owned();
    let origin = root
        .canonicalize()
        .map_err(|error| Error::io(root, error))?;
    let origin_argument = origin
        .to_str()
        .ok_or_else(|| Error::Git("the repository path is not valid UTF-8".to_owned()))?
        .to_owned();
    // Tags carry the version authority the release plan is derived from, so an
    // isolated clone must keep them to reproduce a candidate faithfully.
    GitCommand::new(into)
        .args([
            "clone",
            "--quiet",
            "--no-checkout",
            &origin_argument,
            &target_argument,
        ])
        .run()?;
    if !git::has_object(&target, source)? {
        GitCommand::new(&target)
            .args([
                "fetch",
                "--quiet",
                "--no-tags",
                &origin_argument,
                "+refs/*:refs/intentional/source/*",
            ])
            .run()?;
    }
    if !git::has_object(&target, source)? {
        return Err(Error::Validation(format!(
            "the verifying repository does not contain the accepted source commit {source}"
        )));
    }
    Ok(target)
}

/// Validate the bundle as untrusted input, then import only its declared refs.
fn verify_bundle(clone: &Path, bundle: &Path, candidate: &ReleaseCandidate) -> Result<()> {
    let bundle_argument = bundle
        .to_str()
        .ok_or_else(|| Error::Git("the bundle path is not valid UTF-8".to_owned()))?
        .to_owned();
    let verified = GitCommand::new(clone)
        .args(["bundle", "verify", &bundle_argument])
        .output()?;
    if !verified.succeeded() {
        return Err(Error::Validation(format!(
            "the release bundle did not verify against the accepted source commit: {}",
            verified.diagnostic()
        )));
    }
    let listed = GitCommand::new(clone)
        .args(["bundle", "list-heads", &bundle_argument])
        .run()?;
    let mut heads = Vec::new();
    for line in listed.text()?.lines().filter(|line| !line.is_empty()) {
        let (object, reference) = line.split_once(' ').ok_or_else(|| {
            Error::Validation(format!(
                "the release bundle declares an unreadable ref: {line}"
            ))
        })?;
        heads.push((reference.to_owned(), object.to_owned()));
    }
    let expected = vec![
        (
            BUNDLE_RELEASE_HEAD.to_owned(),
            candidate.release.commit.clone(),
        ),
        (
            BUNDLE_TAG_HEAD.to_owned(),
            candidate.global_tag.object.clone(),
        ),
    ];
    heads.sort();
    let mut expected_sorted = expected.clone();
    expected_sorted.sort();
    if heads != expected_sorted {
        return Err(Error::Validation(format!(
            "the release bundle declares {:?} rather than the verified release and tag identities",
            heads
        )));
    }
    GitCommand::new(clone)
        .args([
            "-c",
            "fetch.fsckObjects=true",
            "-c",
            "transfer.fsckObjects=true",
            "fetch",
            "--quiet",
            "--no-tags",
            &bundle_argument,
            &format!("{BUNDLE_RELEASE_HEAD}:{STAGED_RELEASE_REF}"),
            &format!("{BUNDLE_TAG_HEAD}:{STAGED_TAG_REF}"),
        ])
        .run()?;
    Ok(())
}

/// Prove the imported objects reproduce every identity the manifest declares.
fn verify_imported_objects(clone: &Path, candidate: &ReleaseCandidate) -> Result<()> {
    let release = git::resolve(clone, STAGED_RELEASE_REF)?;
    if release != candidate.release.commit {
        return Err(Error::Validation(format!(
            "the imported release ref identifies {release}, not the declared release commit {}",
            candidate.release.commit
        )));
    }
    let tag_object = GitCommand::new(clone)
        .args(["rev-parse", "--verify", STAGED_TAG_REF])
        .run()?
        .line()?;
    if tag_object != candidate.global_tag.object {
        return Err(Error::Validation(format!(
            "the imported tag ref identifies {tag_object}, not the declared tag object {}",
            candidate.global_tag.object
        )));
    }

    let commit = GitCommand::new(clone)
        .args(["show", "--no-patch", "--format=%T%n%P", &release])
        .run()?;
    let mut lines = commit.text()?.lines();
    let tree = lines.next().unwrap_or_default().trim().to_owned();
    let parents = lines
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if tree != candidate.release.tree {
        return Err(Error::Validation(format!(
            "the imported release commit carries tree {tree}, not the declared tree {}",
            candidate.release.tree
        )));
    }
    if parents.as_slice() != [candidate.source.commit.clone()] {
        return Err(Error::Validation(format!(
            "the imported release commit must have the accepted source commit {} as its sole parent; found {}",
            candidate.source.commit,
            if parents.is_empty() {
                "no parent".to_owned()
            } else {
                parents.join(", ")
            }
        )));
    }

    let kind = GitCommand::new(clone)
        .args(["cat-file", "-t", &tag_object])
        .run()?
        .line()?;
    if kind != "tag" {
        return Err(Error::Validation(format!(
            "the imported global release tag is a {kind} rather than an annotated tag object"
        )));
    }
    let tag = GitCommand::new(clone)
        .args(["cat-file", "tag", &tag_object])
        .run()?;
    let header = tag.text()?;
    let expected = format!(
        "object {}\ntype commit\ntag {}\n",
        candidate.release.commit, candidate.global_tag.name
    );
    if !header.starts_with(&expected) {
        return Err(Error::Validation(format!(
            "the imported global release tag does not name {} targeting the declared release commit",
            candidate.global_tag.name
        )));
    }

    let changed = GitCommand::new(clone)
        .args([
            "diff-tree",
            "-r",
            "-z",
            "--no-renames",
            "--name-status",
            &candidate.source.commit,
            &release,
        ])
        .run()?;
    let mut records = changed
        .text()?
        .split('\0')
        .filter(|value| !value.is_empty());
    let mut observed = Vec::new();
    while let Some(status) = records.next() {
        let path = records.next().ok_or_else(|| {
            Error::Git("git diff-tree produced an unpaired change record".to_owned())
        })?;
        observed.push((path.to_owned(), status.to_owned()));
    }
    observed.sort();
    let mut declared = candidate
        .changed_tree
        .iter()
        .map(|entry| {
            (
                entry.path.clone(),
                match entry.status {
                    crate::release::candidate::ChangeStatus::Added => "A".to_owned(),
                    crate::release::candidate::ChangeStatus::Modified => "M".to_owned(),
                    crate::release::candidate::ChangeStatus::Deleted => "D".to_owned(),
                },
            )
        })
        .collect::<Vec<_>>();
    declared.sort();
    if observed != declared {
        return Err(Error::Validation(
            "the declared changed-tree evidence does not match the release commit".to_owned(),
        ));
    }
    Ok(())
}

/// Rebuild the candidate from the accepted source commit alone and compare it.
fn reproduce_candidate(
    clone: &Path,
    directory: &Path,
    candidate: &ReleaseCandidate,
    plan: &ReleasePlan,
) -> Result<()> {
    GitCommand::new(clone)
        .args([
            "checkout",
            "--quiet",
            "--force",
            "--detach",
            &candidate.source.commit,
        ])
        .run()?;
    discard_previously_imported_tag(clone, candidate)?;
    let built = build_candidate(clone, &candidate.source.commit)?;
    if &built.plan != plan {
        return Err(Error::Validation(format!(
            "the release candidate disagrees with the sealed plan: rebuilding it from the accepted source commit seals {} rather than {}; the verifying repository must carry the release tags that hold version authority",
            built.plan.digest, plan.digest
        )));
    }
    if built.release_tree != candidate.release.tree {
        return Err(Error::Validation(format!(
            "the reproduced release tree {} does not match the declared tree {}",
            built.release_tree, candidate.release.tree
        )));
    }
    if built.release_commit != candidate.release.commit {
        return Err(Error::Validation(format!(
            "the reproduced release commit {} does not match the declared release commit {}",
            built.release_commit, candidate.release.commit
        )));
    }
    if built.tag_object != candidate.global_tag.object
        || built.tag_name != candidate.global_tag.name
        || built.tag_id != candidate.global_tag.id
    {
        return Err(Error::Validation(
            "the reproduced annotated global release tag does not match the declared tag"
                .to_owned(),
        ));
    }
    if built.changed_tree != candidate.changed_tree {
        return Err(Error::Validation(
            "the reproduced changed-tree evidence does not match the declared evidence".to_owned(),
        ));
    }

    let mut expected = BTreeSet::new();
    expected.insert(candidate.plan.file.clone());
    expected.insert(candidate.git_bundle.file.clone());
    for (path, contents) in &built.materialized {
        let relative = format!("{CANDIDATE_TREE_DIRECTORY}/{path}");
        let materialized = directory.join(&relative);
        let transported =
            std::fs::read(&materialized).map_err(|error| Error::io(&materialized, error))?;
        if transported != contents.as_bytes() {
            return Err(Error::Validation(format!(
                "the materialized release candidate for {path} does not reproduce the release content"
            )));
        }
        expected.insert(relative);
    }
    let inventoried = candidate
        .files
        .iter()
        .map(|file| file.path.clone())
        .collect::<BTreeSet<_>>();
    if inventoried != expected {
        let extra = inventoried
            .difference(&expected)
            .cloned()
            .collect::<Vec<_>>();
        let missing = expected
            .difference(&inventoried)
            .cloned()
            .collect::<Vec<_>>();
        return Err(Error::Validation(format!(
            "the release handoff transports an unexpected file set; unexpected: [{}], missing: [{}]",
            extra.join(", "),
            missing.join(", ")
        )));
    }
    Ok(())
}

/// Remove a global release tag left by an earlier verification of this exact handoff.
///
/// Verification imports the annotated global tag into the repository it runs
/// in, so a retry would otherwise reproduce the candidate against a repository
/// that already records the release. Only a tag that already identifies the
/// exact object under verification is removed, so the reproduction environment
/// can never be steered by a manifest value.
fn discard_previously_imported_tag(clone: &Path, candidate: &ReleaseCandidate) -> Result<()> {
    let reference = format!("refs/tags/{}", candidate.global_tag.name);
    let existing = GitCommand::new(clone)
        .args(["rev-parse", "--verify", "--quiet", &reference])
        .output()?;
    if !existing.succeeded() || existing.line()? != candidate.global_tag.object {
        return Ok(());
    }
    GitCommand::new(clone)
        .args(["update-ref", "-d", &reference, &candidate.global_tag.object])
        .run()?;
    Ok(())
}

/// Import the verified release commit and annotated tag so the push step can use them.
fn import_pushable_identities(
    root: &Path,
    proven: &Path,
    candidate: &ReleaseCandidate,
) -> Result<()> {
    let tag_reference = format!("refs/tags/{}", candidate.global_tag.name);
    require_importable_refs(root, candidate)?;
    let source_argument = proven
        .to_str()
        .ok_or_else(|| Error::Git("the verified clone path is not valid UTF-8".to_owned()))?
        .to_owned();
    GitCommand::new(root)
        .args([
            "-c",
            "fetch.fsckObjects=true",
            "-c",
            "transfer.fsckObjects=true",
            "fetch",
            "--quiet",
            "--no-tags",
            &source_argument,
            &format!("{STAGED_RELEASE_REF}:{IMPORTED_RELEASE_REF}"),
            &format!("{STAGED_TAG_REF}:{tag_reference}"),
        ])
        .run()?;
    // Both imported refs are re-read from the repository the privileged push
    // step will use, so neither identity is taken on the strength of the fetch
    // having exited successfully.
    let imported = git::resolve(root, IMPORTED_RELEASE_REF)?;
    if imported != candidate.release.commit {
        return Err(Error::Validation(format!(
            "importing the verified release commit produced {imported}"
        )));
    }
    let imported_tag = GitCommand::new(root)
        .args(["rev-parse", "--verify", &tag_reference])
        .run()?
        .line()?;
    if imported_tag != candidate.global_tag.object {
        return Err(Error::Validation(format!(
            "importing the verified global release tag produced {imported_tag}"
        )));
    }
    Ok(())
}

/// Refuse to verify a handoff whose identities conflict with existing local refs.
fn require_importable_refs(root: &Path, candidate: &ReleaseCandidate) -> Result<()> {
    require_absent_or_identical(
        root,
        &format!("refs/tags/{}", candidate.global_tag.name),
        &candidate.global_tag.object,
    )?;
    require_absent_or_identical(root, IMPORTED_RELEASE_REF, &candidate.release.commit)
}

/// Accept an identical existing ref and reject a conflicting one.
fn require_absent_or_identical(root: &Path, reference: &str, object: &str) -> Result<()> {
    let existing = GitCommand::new(root)
        .args(["rev-parse", "--verify", "--quiet", reference])
        .output()?;
    if !existing.succeeded() {
        return Ok(());
    }
    let current = existing.line()?;
    if current == object {
        return Ok(());
    }
    Err(Error::Validation(format!(
        "{reference} already identifies {current} and conflicts with the verified handoff identity {object}"
    )))
}

/// Collect every handoff-relative regular file path beneath the handoff directory.
fn collect(
    root: &Path,
    directory: &Path,
    depth: usize,
    visited: &mut usize,
    files: &mut BTreeSet<String>,
) -> Result<()> {
    if depth > MAX_CANDIDATE_DEPTH {
        return Err(Error::Validation(format!(
            "the release handoff nests directories more than {MAX_CANDIDATE_DEPTH} deep"
        )));
    }
    let entries = std::fs::read_dir(directory).map_err(|error| Error::io(directory, error))?;
    for entry in entries {
        let entry = entry.map_err(|error| Error::io(directory, error))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|error| Error::io(&path, error))?;
        if kind.is_symlink() {
            return Err(Error::Validation(format!(
                "release handoff entry {} is a symbolic link",
                path.display()
            )));
        }
        // Directories are counted alongside files, so a handoff cannot present
        // an unbounded walk built entirely out of empty directories.
        *visited += 1;
        if *visited > MAX_CANDIDATE_FILES {
            return Err(Error::Validation(format!(
                "the release handoff contains more than {MAX_CANDIDATE_FILES} entries"
            )));
        }
        if kind.is_dir() {
            collect(root, &path, depth + 1, visited, files)?;
            continue;
        }
        if !kind.is_file() {
            return Err(Error::Validation(format!(
                "release handoff entry {} is not a regular file",
                path.display()
            )));
        }
        let relative = path
            .strip_prefix(root)
            .map_err(|_| {
                Error::Validation(format!(
                    "release handoff entry {} left the handoff directory",
                    path.display()
                ))
            })?
            .to_str()
            .ok_or_else(|| {
                Error::Validation(format!(
                    "release handoff entry {} is not valid UTF-8",
                    path.display()
                ))
            })?
            .replace('\\', "/");
        if relative == RELEASE_CANDIDATE_MANIFEST {
            continue;
        }
        files.insert(relative);
    }
    Ok(())
}
