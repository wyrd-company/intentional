// ---
// relationships:
//   tests: github-release-executor
// ---

use assert_cmd::Command;
use intentional_core::{ReleaseCandidate, RELEASE_CANDIDATE_MANIFEST};
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

    fn verify(&self, clone: &Path) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("intentional"));
        command.arg("-C").arg(clone);
        command.args(["verify", "handoff"]).arg(&self.handoff);
        command
    }
}

fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = ProcessCommand::new("git")
        .args(arguments)
        .current_dir(directory)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Fixture Author")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "Fixture Author")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 git output")
        .trim()
        .to_owned()
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
    // The annotated global tag exists locally and is not published.
    assert_eq!(
        git(&fixture.source, &["rev-parse", "refs/tags/1.1.0"]),
        candidate.global_tag.object
    );
    assert_eq!(
        git(&fixture.remote, &["tag", "--list", "1.1.0"]),
        "",
        "preparation must not publish the global release tag"
    );

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
fn refuses_a_bundle_that_exceeds_the_transport_bound() {
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
    fixture.write_manifest(&manifest_yaml_without_validation(&manifest));

    fixture
        .verify(&clone)
        .assert()
        .failure()
        .stderr(predicates::str::contains("byte bound"));
}

#[test]
fn refuses_a_handoff_when_the_verifying_repository_advanced_past_the_source() {
    let fixture = ReleaseFixture::new();
    fixture.author_release();
    let candidate = fixture.prepare();
    let clone = fixture.privileged_clone("privileged");

    // The verifying clone already carries a conflicting tag for the same name.
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
