// ---
// relationships:
//   tests: github-release-executor
// ---

use assert_cmd::Command;
use intentional_core::{
    CandidateFile, ChangeStatus, ChangedPath, ReleaseCandidate, RELEASE_CANDIDATE_MANIFEST,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use tempfile::TempDir;

/// A source repository, its remote, and a privileged clone that verifies handoffs.
struct ReleaseFixture {
    _temp: TempDir,
    /// Unprivileged workspace that prepares release candidates.
    source: PathBuf,
    /// Bare remote whose default branch supplies the accepted source commit.
    remote: PathBuf,
    /// Directory in which handoffs are written.
    handoff: PathBuf,
}

impl ReleaseFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory");
        let remote = temp.path().join("remote.git");
        let source = temp.path().join("source");
        let handoff = temp.path().join("release-candidate");
        git(
            temp.path(),
            &[
                "init",
                "--quiet",
                "--bare",
                "--initial-branch=main",
                remote.to_str().expect("remote path"),
            ],
        );
        git(
            temp.path(),
            &[
                "init",
                "--quiet",
                "--initial-branch=main",
                source.to_str().expect("source path"),
            ],
        );
        git(&source, &["config", "user.name", "Fixture Author"]);
        git(
            &source,
            &["config", "user.email", "fixture@example.invalid"],
        );

        let fixture = Self {
            _temp: temp,
            source,
            remote,
            handoff,
        };
        fixture.write(
            ".intentional/config.yml",
            "$schema: https://intentional.foo/schemas/config.yml\ncontract: contract-1\nrelease-units:\n  widget:\n    path: .\n    projections:\n      - adapter: json\n        file: package.json\n        pointer: /version\n        mode: committed\n    tags:\n      primary:\n        role: primary\n        template: '{version}'\n",
        );
        fixture.write("package.json", "{\n  \"version\": \"1.0.0\"\n}\n");
        fixture.write(".intentional/intents/.keep", "");
        git(&fixture.source, &["add", "-A"]);
        git(
            &fixture.source,
            &["commit", "--quiet", "-m", "Create the fixture workspace"],
        );
        fixture.cli().args(["tag", "--baseline"]).assert().success();
        git(
            &fixture.source,
            &[
                "remote",
                "add",
                "origin",
                fixture.remote.to_str().expect("remote path"),
            ],
        );
        git(
            &fixture.source,
            &["push", "--quiet", "origin", "main", "--tags"],
        );
        fixture
    }

    /// Author one intent and publish the resulting source commit.
    fn author_release(&self) {
        self.cli()
            .args([
                "add",
                "--release-unit",
                "widget:minor",
                "--message",
                "Add a widget capability",
            ])
            .assert()
            .success();
        git(&self.source, &["add", "-A"]);
        git(
            &self.source,
            &["commit", "--quiet", "-m", "Record release intent"],
        );
        git(&self.source, &["push", "--quiet", "origin", "main"]);
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.source.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent");
        }
        fs::write(path, contents).expect("fixture write");
    }

    fn cli(&self) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("intentional"));
        command.arg("-C").arg(&self.source);
        command
    }

    /// Prepare a release candidate into the fixture handoff directory.
    fn prepare(&self) -> ReleaseCandidate {
        self.cli()
            .args(["release", "prepare", "--output"])
            .arg(&self.handoff)
            .assert()
            .success();
        self.manifest()
    }

    fn manifest(&self) -> ReleaseCandidate {
        ReleaseCandidate::from_yaml(
            &fs::read_to_string(self.handoff.join(RELEASE_CANDIDATE_MANIFEST))
                .expect("release candidate manifest"),
        )
        .expect("valid release candidate manifest")
    }

    /// Overwrite the manifest without revalidating it, as a hostile producer would.
    fn write_manifest(&self, text: &str) {
        fs::write(self.handoff.join(RELEASE_CANDIDATE_MANIFEST), text).expect("write manifest");
    }

    /// Create the fresh privileged clone that performs handoff verification.
    fn privileged_clone(&self, name: &str) -> PathBuf {
        let target = self._temp.path().join(name);
        git(
            self._temp.path(),
            &[
                "clone",
                "--quiet",
                self.remote.to_str().expect("remote path"),
                target.to_str().expect("clone path"),
            ],
        );
        target
    }

    /// Rewrite the prepared handoff into a self-consistent forgery.
    ///
    /// The forged release tree carries one extra file. Its commit, annotated
    /// tag, Git bundle, changed-tree evidence, and file inventory are all
    /// regenerated to agree with each other, so nothing short of rebuilding the
    /// candidate from the accepted source commit can detect it.
    fn forge_extra_file(
        &self,
        path: &str,
        contents: &str,
        candidate: &ReleaseCandidate,
    ) -> ReleaseCandidate {
        let blob = git_stdin(
            &self.source,
            &["hash-object", "-w", "--stdin", "--path", path],
            contents,
        );

        let index = self._temp.path().join("forged-index");
        let index = index.to_str().expect("index path").to_owned();
        git_env(
            &self.source,
            &["read-tree", &candidate.release.commit],
            &[("GIT_INDEX_FILE", index.as_str())],
        );
        git_env_stdin(
            &self.source,
            &["update-index", "--index-info"],
            &[("GIT_INDEX_FILE", index.as_str())],
            &format!("100644 {blob}\t{path}\n"),
        );
        let tree = git_env(
            &self.source,
            &["write-tree"],
            &[("GIT_INDEX_FILE", index.as_str())],
        );

        let raw = git_raw(
            &self.source,
            &["cat-file", "commit", &candidate.release.commit],
        );
        let message = raw.split_once("\n\n").expect("commit message").1.to_owned();
        let name = git(
            &self.source,
            &[
                "show",
                "--no-patch",
                "--format=%an",
                &candidate.release.commit,
            ],
        );
        let email = git(
            &self.source,
            &[
                "show",
                "--no-patch",
                "--format=%ae",
                &candidate.release.commit,
            ],
        );
        let date = git(
            &self.source,
            &[
                "show",
                "--no-patch",
                "--date=raw",
                "--format=%ad",
                &candidate.release.commit,
            ],
        );
        let commit = git_env_stdin(
            &self.source,
            &["commit-tree", &tree, "-p", &candidate.source.commit],
            &[
                ("GIT_AUTHOR_NAME", name.as_str()),
                ("GIT_AUTHOR_EMAIL", email.as_str()),
                ("GIT_AUTHOR_DATE", date.as_str()),
                ("GIT_COMMITTER_NAME", name.as_str()),
                ("GIT_COMMITTER_EMAIL", email.as_str()),
                ("GIT_COMMITTER_DATE", date.as_str()),
            ],
            &message,
        );

        let tag_body = git_raw(
            &self.source,
            &["cat-file", "tag", &candidate.global_tag.object],
        );
        let tag_body = tag_body.replacen(
            &format!("object {}\n", candidate.release.commit),
            &format!("object {commit}\n"),
            1,
        );
        let tag_object = git_stdin(&self.source, &["mktag"], &tag_body);

        git(
            &self.source,
            &["update-ref", "refs/heads/intentional-release", &commit],
        );
        git(
            &self.source,
            &[
                "update-ref",
                "refs/tags/intentional-global-release",
                &tag_object,
            ],
        );
        let bundle = self.handoff.join("release.bundle");
        fs::remove_file(&bundle).expect("replace bundle");
        git(
            &self.source,
            &[
                "bundle",
                "create",
                bundle.to_str().expect("bundle path"),
                &format!("^{}", candidate.source.commit),
                "refs/heads/intentional-release",
                "refs/tags/intentional-global-release",
            ],
        );
        git(
            &self.source,
            &["update-ref", "-d", "refs/heads/intentional-release"],
        );
        git(
            &self.source,
            &["update-ref", "-d", "refs/tags/intentional-global-release"],
        );

        let materialized = format!("candidate/{path}");
        fs::write(self.handoff.join(&materialized), contents).expect("materialize forgery");

        let mut manifest = candidate.clone();
        manifest.release.commit = commit.clone();
        manifest.release.tree = tree;
        manifest.global_tag.object = tag_object;
        manifest.global_tag.target = commit;
        manifest.changed_tree.push(ChangedPath {
            path: path.to_owned(),
            status: ChangeStatus::Added,
            digest: Some(digest(contents.as_bytes())),
        });
        manifest
            .changed_tree
            .sort_by(|left, right| left.path.cmp(&right.path));
        manifest.files.push(CandidateFile {
            path: materialized,
            sha256: digest(contents.as_bytes()),
            size: contents.len() as u64,
        });
        let bundle_bytes = fs::read(&bundle).expect("bundle bytes");
        for file in &mut manifest.files {
            if file.path == "release.bundle" {
                file.sha256 = digest(&bundle_bytes);
                file.size = bundle_bytes.len() as u64;
            }
        }
        manifest
            .files
            .sort_by(|left, right| left.path.cmp(&right.path));
        manifest.git_bundle.sha256 = digest(&bundle_bytes);
        self.write_manifest(&manifest.to_yaml().expect("forged manifest"));
        manifest
    }

    fn verify(&self, clone: &Path) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("intentional"));
        command.arg("-C").arg(clone);
        command.args(["verify", "handoff"]).arg(&self.handoff);
        command
    }
}

fn digest(bytes: &[u8]) -> String {
    format!("sha256:{:x}", <sha2::Sha256 as sha2::Digest>::digest(bytes))
}

fn git_raw(directory: &Path, arguments: &[&str]) -> String {
    run_git(directory, arguments, &[], None)
}

fn git_env(directory: &Path, arguments: &[&str], environment: &[(&str, &str)]) -> String {
    run_git(directory, arguments, environment, None)
        .trim()
        .to_owned()
}

fn git_stdin(directory: &Path, arguments: &[&str], input: &str) -> String {
    run_git(directory, arguments, &[], Some(input))
        .trim()
        .to_owned()
}

fn git_env_stdin(
    directory: &Path,
    arguments: &[&str],
    environment: &[(&str, &str)],
    input: &str,
) -> String {
    run_git(directory, arguments, environment, Some(input))
        .trim()
        .to_owned()
}

fn git(directory: &Path, arguments: &[&str]) -> String {
    run_git(directory, arguments, &[], None).trim().to_owned()
}

fn run_git(
    directory: &Path,
    arguments: &[&str],
    environment: &[(&str, &str)],
    input: Option<&str>,
) -> String {
    let mut command = ProcessCommand::new("git");
    command
        .args(arguments)
        .current_dir(directory)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Fixture Author")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture Author")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (key, value) in environment {
        command.env(key, value);
    }
    if input.is_some() {
        command.stdin(std::process::Stdio::piped());
    }
    let mut child = command.spawn().expect("run git");
    if let Some(input) = input {
        use std::io::Write;
        child
            .stdin
            .take()
            .expect("git input")
            .write_all(input.as_bytes())
            .expect("write git input");
    }
    let output = child.wait_with_output().expect("collect git output");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("UTF-8 git output")
}

#[test]
fn prepares_a_deterministic_candidate_without_mutating_the_workspace() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    let source = git(&fixture.source, &["rev-parse", "HEAD"]);

    let candidate = fixture.prepare();

    assert_eq!(candidate.source.commit, source);
    assert_eq!(candidate.release.parent, source);
    assert_eq!(candidate.global_tag.target, candidate.release.commit);
    assert_eq!(candidate.global_tag.name, "1.1.0");
    assert_eq!(candidate.global_tag.id, "release-unit/widget/primary");

    // Preparation constructs Git objects only. The checkout still matches S.
    assert_eq!(git(&fixture.source, &["rev-parse", "HEAD"]), source);
    assert_eq!(
        git(
            &fixture.source,
            &["status", "--porcelain", "--untracked-files=no"]
        ),
        ""
    );
    // The annotated global tag is recorded under the reserved namespace so it
    // never becomes version authority in the preparing checkout.
    assert_eq!(
        git(
            &fixture.source,
            &["rev-parse", "refs/intentional/release/global-tag"]
        ),
        candidate.global_tag.object
    );
    assert_eq!(
        git(&fixture.source, &["tag", "--list"]),
        "1.0.0",
        "preparation must not create the release tag in the tag namespace"
    );
    assert_eq!(
        git(&fixture.remote, &["tag", "--list", "1.1.0"]),
        "",
        "preparation must not publish the global release tag"
    );
    // The fixed transport ref names are never written into the workspace.
    for reference in [
        "refs/heads/intentional-release",
        "refs/tags/intentional-global-release",
    ] {
        assert!(
            !git(&fixture.source, &["show-ref"]).contains(reference),
            "preparation must not write {reference} into the workspace"
        );
    }

    // The commit identity is a fixed protocol value bound to the source timestamp.
    let identity = git(
        &fixture.source,
        &[
            "show",
            "--no-patch",
            "--format=%an <%ae>%n%cn <%ce>%n%at %ct",
            &candidate.release.commit,
        ],
    );
    let source_timestamp = git(
        &fixture.source,
        &["show", "--no-patch", "--format=%ct", &source],
    );
    assert_eq!(
        identity,
        format!(
            "Intentional <releases@intentional.foo>\nIntentional <releases@intentional.foo>\n{source_timestamp} {source_timestamp}"
        )
    );
}

#[test]
fn rebuilds_the_same_candidate_from_the_same_source() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    let first = fixture.prepare();

    let second_handoff = fixture._temp.path().join("second-candidate");
    fixture
        .cli()
        .args(["release", "prepare", "--output"])
        .arg(&second_handoff)
        .assert()
        .success();
    let second = ReleaseCandidate::from_yaml(
        &fs::read_to_string(second_handoff.join(RELEASE_CANDIDATE_MANIFEST)).expect("manifest"),
    )
    .expect("valid manifest");

    assert_eq!(first.release.commit, second.release.commit);
    assert_eq!(first.release.tree, second.release.tree);
    assert_eq!(first.global_tag.object, second.global_tag.object);
    assert_eq!(first.plan.digest, second.plan.digest);
    assert_eq!(first.changed_tree, second.changed_tree);
}

#[test]
fn refuses_a_source_the_remote_default_branch_no_longer_identifies() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();

    // A second workspace advances the remote default branch after acceptance.
    let racing = fixture._temp.path().join("racing");
    git(
        fixture._temp.path(),
        &[
            "clone",
            "--quiet",
            fixture.remote.to_str().expect("remote path"),
            racing.to_str().expect("clone path"),
        ],
    );
    fs::write(racing.join("NOTES.md"), "later work\n").expect("write");
    git(&racing, &["add", "-A"]);
    git(
        &racing,
        &["commit", "--quiet", "-m", "Advance the default branch"],
    );
    git(&racing, &["push", "--quiet", "origin", "main"]);

    fixture
        .cli()
        .args(["release", "prepare", "--output"])
        .arg(&fixture.handoff)
        .assert()
        .failure()
        .stderr(predicates::str::contains("not the accepted source commit"));
}

#[test]
fn refuses_a_workspace_that_does_not_match_the_source_commit() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    fixture.write("package.json", "{\n  \"version\": \"9.9.9\"\n}\n");

    fixture
        .cli()
        .args(["release", "prepare", "--output"])
        .arg(&fixture.handoff)
        .assert()
        .failure()
        .stderr(predicates::str::contains("package.json"));
}

#[test]
fn verifies_a_prepared_handoff_in_a_fresh_privileged_clone() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    let candidate = fixture.prepare();
    let clone = fixture.privileged_clone("privileged");

    fixture
        .verify(&clone)
        .assert()
        .success()
        .stdout(predicates::str::contains(format!(
            "release-sha: {}",
            candidate.release.commit
        )))
        .stdout(predicates::str::contains("global-tag: 1.1.0"))
        .stdout(predicates::str::contains(format!(
            "plan-digest: {}",
            candidate.plan.digest
        )));

    // Verification materializes exactly the declared objects and exposes them.
    assert_eq!(
        git(&clone, &["rev-parse", "refs/intentional/handoff/release"]),
        candidate.release.commit
    );
    assert_eq!(
        git(&clone, &["rev-parse", "refs/tags/1.1.0"]),
        candidate.global_tag.object
    );
    assert_eq!(
        git(
            &clone,
            &["rev-parse", &format!("{}^", candidate.release.commit)]
        ),
        candidate.source.commit
    );
}

#[test]
fn repeats_verification_idempotently_after_a_partial_recovery() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    let candidate = fixture.prepare();
    let clone = fixture.privileged_clone("privileged");

    fixture.verify(&clone).assert().success();
    fixture.verify(&clone).assert().success();

    assert_eq!(
        git(&clone, &["rev-parse", "refs/tags/1.1.0"]),
        candidate.global_tag.object
    );
}

#[test]
fn refuses_a_handoff_whose_materialized_candidate_was_tampered_with() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    fixture.prepare();
    let clone = fixture.privileged_clone("privileged");

    fs::write(
        fixture.handoff.join("candidate/package.json"),
        "{\n  \"version\": \"9.9.9\"\n}\n",
    )
    .expect("tamper");

    fixture
        .verify(&clone)
        .assert()
        .failure()
        .stderr(predicates::str::contains("inventoried digest"));
}

#[test]
fn refuses_a_handoff_whose_inventory_was_rewritten_to_match_tampering() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    let candidate = fixture.prepare();
    let clone = fixture.privileged_clone("privileged");

    let tampered = "{\n  \"version\": \"9.9.9\"\n}\n";
    let path = fixture.handoff.join("candidate/package.json");
    fs::write(&path, tampered).expect("tamper");

    let mut manifest = candidate.clone();
    let digest = format!(
        "sha256:{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(tampered.as_bytes())
    );
    for file in &mut manifest.files {
        if file.path == "candidate/package.json" {
            file.sha256 = digest.clone();
            file.size = tampered.len() as u64;
        }
    }
    for entry in &mut manifest.changed_tree {
        if entry.path == "package.json" {
            entry.digest = Some(digest.clone());
        }
    }
    fixture.write_manifest(&manifest.to_yaml().expect("manifest"));

    fixture
        .verify(&clone)
        .assert()
        .failure()
        .stderr(predicates::str::contains("changed-tree evidence"));
}

#[test]
fn refuses_a_handoff_that_transports_an_uninventoried_file() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    fixture.prepare();
    let clone = fixture.privileged_clone("privileged");

    fs::write(fixture.handoff.join("candidate/smuggled.sh"), "#!/bin/sh\n").expect("smuggle");

    fixture
        .verify(&clone)
        .assert()
        .failure()
        .stderr(predicates::str::contains("uninventoried files"));
}

#[test]
fn refuses_a_handoff_whose_manifest_claims_a_different_release_commit() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    let candidate = fixture.prepare();
    let clone = fixture.privileged_clone("privileged");

    let mut manifest = candidate.clone();
    manifest.release.commit = "0".repeat(40);
    manifest.global_tag.target = manifest.release.commit.clone();
    fixture.write_manifest(&manifest.to_yaml().expect("manifest"));

    fixture
        .verify(&clone)
        .assert()
        .failure()
        .stderr(predicates::str::contains("release and tag identities"));
}

#[test]
fn refuses_a_manifest_whose_release_parent_is_not_the_accepted_source() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    let candidate = fixture.prepare();
    let clone = fixture.privileged_clone("privileged");

    let mut manifest = candidate.clone();
    manifest.release.parent = "0".repeat(40);
    let text = manifest_yaml_without_validation(&manifest);
    fixture.write_manifest(&text);

    fixture
        .verify(&clone)
        .assert()
        .failure()
        .stderr(predicates::str::contains("accepted source commit"));
}

#[test]
fn refuses_a_manifest_whose_release_tree_disagrees_with_the_release_commit() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    let candidate = fixture.prepare();
    let clone = fixture.privileged_clone("privileged");

    let mut manifest = candidate.clone();
    manifest.release.tree = "0".repeat(40);
    fixture.write_manifest(&manifest.to_yaml().expect("manifest"));

    fixture
        .verify(&clone)
        .assert()
        .failure()
        .stderr(predicates::str::contains("not the declared tree"));
}

#[test]
fn refuses_a_manifest_that_renames_the_annotated_global_tag() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    let candidate = fixture.prepare();
    let clone = fixture.privileged_clone("privileged");

    let mut manifest = candidate.clone();
    manifest.global_tag.name = "9.9.9".to_owned();
    fixture.write_manifest(&manifest.to_yaml().expect("manifest"));

    fixture
        .verify(&clone)
        .assert()
        .failure()
        .stderr(predicates::str::contains("global release tag"));
}

#[test]
fn refuses_a_handoff_whose_sealed_plan_was_rewritten() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    let candidate = fixture.prepare();
    let clone = fixture.privileged_clone("privileged");

    let plan_path = fixture.handoff.join("release-plan.json");
    let plan = fs::read_to_string(&plan_path).expect("plan");
    let rewritten = plan.replace("1.1.0", "9.9.9");
    assert_ne!(plan, rewritten);
    fs::write(&plan_path, &rewritten).expect("tamper");

    let mut manifest = candidate.clone();
    let digest = format!(
        "sha256:{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(rewritten.as_bytes())
    );
    for file in &mut manifest.files {
        if file.path == "release-plan.json" {
            file.sha256 = digest.clone();
            file.size = rewritten.len() as u64;
        }
    }
    manifest.plan.sha256 = digest;
    fixture.write_manifest(&manifest.to_yaml().expect("manifest"));

    fixture
        .verify(&clone)
        .assert()
        .failure()
        .stderr(predicates::str::contains("digest"));
}

#[test]
fn refuses_a_malformed_git_bundle() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    let candidate = fixture.prepare();
    let clone = fixture.privileged_clone("privileged");

    let bundle = fixture.handoff.join("release.bundle");
    let corrupt = b"# v2 git bundle\nnot a bundle at all\n";
    fs::write(&bundle, corrupt).expect("corrupt");

    let mut manifest = candidate.clone();
    let digest = format!(
        "sha256:{:x}",
        <sha2::Sha256 as sha2::Digest>::digest(corrupt.as_slice())
    );
    for file in &mut manifest.files {
        if file.path == "release.bundle" {
            file.sha256 = digest.clone();
            file.size = corrupt.len() as u64;
        }
    }
    manifest.git_bundle.sha256 = digest;
    fixture.write_manifest(&manifest.to_yaml().expect("manifest"));

    fixture
        .verify(&clone)
        .assert()
        .failure()
        .stderr(predicates::str::contains("bundle"));
}

#[test]
fn refuses_a_bundle_whose_inventoried_size_exceeds_the_transport_bound() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    let candidate = fixture.prepare();
    let clone = fixture.privileged_clone("privileged");

    let mut manifest = candidate.clone();
    for file in &mut manifest.files {
        if file.path == "release.bundle" {
            file.size = intentional_core::MAX_BUNDLE_BYTES + 1;
        }
    }
    fixture.write_manifest(&manifest.to_yaml().expect("manifest"));

    fixture
        .verify(&clone)
        .assert()
        .failure()
        .stderr(predicates::str::contains("byte handoff bound"));
}

#[test]
fn refuses_a_bundle_that_is_oversized_on_disk() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    fixture.prepare();
    let clone = fixture.privileged_clone("privileged");

    // The inventory understates the size, so only the observed-size gate can
    // reject this bundle before it is read.
    let bundle = fs::OpenOptions::new()
        .write(true)
        .open(fixture.handoff.join("release.bundle"))
        .expect("bundle");
    bundle
        .set_len(intentional_core::MAX_BUNDLE_BYTES + 1)
        .expect("grow bundle");
    drop(bundle);

    fixture
        .verify(&clone)
        .assert()
        .failure()
        .stderr(predicates::str::contains("byte handoff bound"));
}

#[test]
fn refuses_a_handoff_that_conflicts_with_an_existing_release_tag() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    let candidate = fixture.prepare();
    let clone = fixture.privileged_clone("privileged");

    // The verifying clone already carries a different record under the same
    // release tag name, which the import must refuse rather than overwrite.
    git(
        &clone,
        &["tag", "-a", "1.1.0", "-m", "conflicting record", "HEAD"],
    );

    fixture
        .verify(&clone)
        .assert()
        .failure()
        .stderr(predicates::str::contains(
            "conflicts with the verified handoff",
        ));
    assert_ne!(
        git(&clone, &["rev-parse", "refs/tags/1.1.0"]),
        candidate.global_tag.object
    );
}

#[test]
fn refuses_a_handoff_whose_inventory_addresses_a_path_outside_the_directory() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    let candidate = fixture.prepare();
    let clone = fixture.privileged_clone("privileged");

    let mut text = candidate.to_yaml().expect("manifest");
    text = text.replace(
        "- path: candidate/package.json",
        "- path: ../escaped/package.json",
    );
    fixture.write_manifest(&text);

    fixture
        .verify(&clone)
        .assert()
        .failure()
        .stderr(predicates::str::contains("traverse directories"));
}

/// Serialize a manifest that deliberately violates a manifest-level invariant.
///
/// `to_yaml` refuses to emit an invalid manifest, which is the behaviour the
/// producer has. A hostile producer does not, so tests need a raw encoder.
fn manifest_yaml_without_validation(candidate: &ReleaseCandidate) -> String {
    serde_yaml::to_string(candidate).expect("serialize manifest")
}

#[test]
fn refuses_a_coherent_forgery_only_the_reproduction_clone_can_reject() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    let candidate = fixture.prepare();
    let clone = fixture.privileged_clone("privileged");

    // Build a handoff that agrees with itself end to end: a release tree
    // carrying an extra file, a matching commit and annotated tag, a bundle
    // regenerated from those objects, and an inventory and changed-tree that
    // both describe the result. Every declared-value check passes, so only the
    // bundle-free rebuild from the accepted source commit can reject it.
    let forged = fixture.forge_extra_file("backdoor.sh", "#!/bin/sh\nexfiltrate\n", &candidate);

    assert_ne!(forged.release.commit, candidate.release.commit);
    assert_eq!(forged.release.parent, candidate.source.commit);
    assert_eq!(forged.global_tag.target, forged.release.commit);
    forged
        .validate()
        .expect("the forgery is internally consistent");

    fixture
        .verify(&clone)
        .assert()
        .failure()
        .stderr(predicates::str::contains("reproduced release tree"));
}

#[test]
fn refuses_a_handoff_that_transports_a_symbolic_link() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    fixture.prepare();
    let clone = fixture.privileged_clone("privileged");

    std::os::unix::fs::symlink("/etc/passwd", fixture.handoff.join("candidate/linked"))
        .expect("symbolic link");

    fixture
        .verify(&clone)
        .assert()
        .failure()
        .stderr(predicates::str::contains("symbolic link"));
}

#[test]
fn refuses_a_manifest_that_moves_the_source_to_a_different_real_commit() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    let candidate = fixture.prepare();
    let clone = fixture.privileged_clone("privileged");

    // Both the source and the declared parent move to the baseline commit, so
    // the manifest stays self-consistent and the refusal has to come from the
    // parent of the imported release commit.
    let baseline = git(&fixture.source, &["rev-parse", "HEAD~1"]);
    assert_ne!(baseline, candidate.source.commit);
    let mut manifest = candidate.clone();
    manifest.source.commit = baseline.clone();
    manifest.release.parent = baseline;
    fixture.write_manifest(&manifest.to_yaml().expect("manifest"));

    fixture
        .verify(&clone)
        .assert()
        .failure()
        .stderr(predicates::str::contains("sole parent"));
}

/// Executor configuration whose only purpose is to derive the release workflow.
const EXECUTOR_CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-1
workspace-tags:
  release:
    template: '{version}'
github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml }
release-units:
  component:
    path: component
    tags:
      primary: { role: primary, template: '{id}@{version}', require-phase: after-publication }
"#;

const STUB_WORKFLOW: &str =
    "name: managed\n\non:\n  workflow_dispatch:\n\njobs:\n  repository_job:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n";

/// The exact command the derived authority-transition job runs to verify a handoff.
///
/// The workflow is reconciled rather than transcribed, so this test executes
/// whatever a repository would actually receive today, with the runner
/// temporary directory resolved the way GitHub Actions resolves it.
fn generated_handoff_command(runner_temp: &Path) -> Vec<String> {
    let workspace = tempfile::tempdir().expect("temporary workspace");
    let root = workspace.path();
    for (relative, contents) in [
        (".intentional/config.yml", EXECUTOR_CONFIG),
        (
            "component/Cargo.toml",
            "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\n",
        ),
        (".github/workflows/release.yml", STUB_WORKFLOW),
        (".github/workflows/publish.yml", STUB_WORKFLOW),
    ] {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().expect("parent")).expect("fixture directory");
        fs::write(path, contents).expect("fixture file");
    }

    let comparison =
        intentional_core::compare_workflow(root, intentional_core::WorkflowRole::Release, None)
            .expect("comparison runs");
    let applied = comparison.apply().expect("transformation applies");
    let derived = fs::read_to_string(root.join(&applied.path)).expect("derived workflow");
    let document: serde_yaml::Value =
        serde_yaml::from_str(&derived).expect("derived workflow parses");

    let jobs = document["jobs"]
        .as_mapping()
        .expect("the derived workflow declares jobs");
    let mut commands = jobs
        .values()
        .filter_map(|job| job["steps"].as_sequence())
        .flatten()
        .filter_map(|step| step["run"].as_str())
        .map(|body| body.replace("${{ runner.temp }}", &runner_temp.display().to_string()))
        .filter_map(|body| shell_words::split(&body).ok())
        .filter(|tokens| tokens.len() > 3 && tokens[..3] == ["intentional", "verify", "handoff"]);
    let command = commands
        .next()
        .expect("the authority transition verifies the handoff");
    assert!(
        commands.next().is_none(),
        "exactly one managed step verifies the handoff"
    );
    command
}

#[test]
fn runs_the_generated_authority_transition_command_against_a_prepared_handoff() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    let runner_temp = tempfile::tempdir().expect("runner temporary directory");
    let command = generated_handoff_command(runner_temp.path());
    let candidate = PathBuf::from(command.last().expect("the command names a handoff"));

    // The prepare job writes the candidate to the location the artifact download
    // restores it to, which is the location this command reads.
    fixture
        .cli()
        .args(["release", "prepare", "--output"])
        .arg(&candidate)
        .assert()
        .success();

    // The job runs from the checkout root with no arguments the template omits.
    Command::new(assert_cmd::cargo::cargo_bin!("intentional"))
        .current_dir(fixture.privileged_clone("generated-command"))
        .args(&command[1..])
        .assert()
        .success()
        .stdout(predicates::str::contains("release handoff verified"))
        .stdout(predicates::str::contains("global-tag: 1.1.0"));
}
