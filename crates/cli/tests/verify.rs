// ---
// relationships:
//   tests: release-planning-and-records
// ---

//! `intentional verify`: one established Release, checked from Git history.

use assert_cmd::Command;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use tempfile::TempDir;

struct TestRepo {
    _temp: TempDir,
    root: PathBuf,
}

impl TestRepo {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("sample");
        fs::create_dir(&root).expect("workspace directory");
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.name", "Fixture Author"]);
        git(&root, &["config", "user.email", "fixture@example.invalid"]);
        Self { _temp: temp, root }
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent");
        }
        fs::write(path, contents).expect("fixture write");
    }

    fn read(&self, relative: &str) -> String {
        fs::read_to_string(self.root.join(relative)).expect("fixture read")
    }

    fn commit(&self, message: &str) {
        git(&self.root, &["add", "-A"]);
        git(&self.root, &["commit", "-q", "-m", message]);
    }

    fn cli(&self) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("intentional"));
        command.arg("-C").arg(&self.root);
        command
    }

    fn intent(&self, slug: &str, message: &str) {
        self.write(
            &format!(".intentional/intents/{slug}.md"),
            &format!("---\nsample-library: minor\n---\n\n{message}\n"),
        );
        self.commit(&format!("add intent {slug}"));
    }

    /// Apply, commit, and create every tag of one release.
    fn release(&self, args: &[&str]) {
        self.cli().arg("apply").args(args).assert().success();
        self.commit("apply release");
        self.cli().arg("tag").args(args).assert().success();
        self.cli()
            .args(["tag", "--phase", "after-publication"])
            .args(args)
            .assert()
            .success();
    }

    /// Run `verify --json`; return the exit code and the report.
    fn verify(&self, tag: &str) -> (i32, Value) {
        let output = self
            .cli()
            .args(["verify", tag, "--json"])
            .output()
            .expect("verify command");
        let report = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
            panic!(
                "verify {tag} printed no JSON ({error}): {}",
                String::from_utf8_lossy(&output.stderr)
            )
        });
        (output.status.code().expect("exit code"), report)
    }

    /// Replace tag `name` with an annotated tag at `target` whose message is
    /// `message`, tagged the way Intentional tags: fixed identity, committer
    /// time of the target.
    fn forge(&self, name: &str, target: &str, message: &str) {
        let _ = ProcessCommand::new("git")
            .args(["tag", "-d", name])
            .current_dir(&self.root)
            .output();
        let date = git(
            &self.root,
            &["log", "-1", "--format=%cd", "--date=raw", target],
        );
        let output = ProcessCommand::new("git")
            .args(["tag", "-a", name, target, "-m", message])
            .env("GIT_COMMITTER_NAME", "Intentional")
            .env("GIT_COMMITTER_EMAIL", "intentional@wyrd.company")
            .env("GIT_COMMITTER_DATE", date)
            .current_dir(&self.root)
            .output()
            .expect("run git tag");
        assert!(output.status.success(), "{output:?}");
    }

    fn message(&self, name: &str) -> String {
        let object = git(&self.root, &["cat-file", "tag", name]);
        let (_, message) = object.split_once("\n\n").expect("tag message");
        format!("{message}\n")
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    let output = ProcessCommand::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 git output")
        .trim()
        .to_owned()
}

fn repository_file(relative: &str) -> String {
    fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(relative),
    )
    .expect("repository file")
}

const CONFIG: &str = r#"contract: contract-1
settings:
  internal-dependency-bump: patch
  pre-1-0-bump-mapping: compatibility
release-units:
  sample-library:
    path: .
    projections:
      - adapter: npm
        file: package.json
        mode: committed
    tags:
      primary:
        role: primary
        template: '{id}@{version}'
      mirror:
        role: projection
        template: 'mirror@{version}'
        tag-after:
          - release-unit/sample-library/primary
workspace-tags:
  release:
    template: '{version}'
    require-phase: after-publication
    tag-after:
      - release-unit/sample-library/primary
"#;

/// One Release Unit at 1.2.3 with a projection tag and a phased workspace
/// tag, and one pending minor intent.
fn release_repo() -> TestRepo {
    let repo = TestRepo::new();
    repo.write(
        "package.json",
        "{\n  \"name\": \"sample-library\",\n  \"version\": \"1.2.3\"\n}\n",
    );
    repo.write(".intentional/config.yml", CONFIG);
    repo.write(".intentional/intents/.gitkeep", "");
    repo.commit("add fixture");
    git(&repo.root, &["tag", "sample-library@1.2.3"]);
    repo.intent("sample-otter-0001", "Add a sample capability.");
    repo
}

fn codes(report: &Value, severity: &str) -> Vec<String> {
    report["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .filter(|diagnostic| diagnostic["severity"] == severity)
        .map(|diagnostic| diagnostic["code"].as_str().expect("code").to_owned())
        .collect()
}

fn assert_verified(repo: &TestRepo, tag: &str, identities: &[&str]) -> Value {
    let (code, report) = repo.verify(tag);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["verified"], true);
    assert_eq!(
        report["guarantees"],
        serde_json::json!([
            "tag-record",
            "plan-reconstruction",
            "materialized-source",
            "identity-tags"
        ])
    );
    assert_eq!(report["identity_tags"], serde_json::json!(identities));
    assert!(codes(&report, "error").is_empty(), "{report:#}");
    report
}

fn assert_fails_with(repo: &TestRepo, tag: &str, code: &str) -> Value {
    let (exit, report) = repo.verify(tag);
    assert_eq!(exit, 1, "{report:#}");
    assert_eq!(report["verified"], false);
    assert!(
        codes(&report, "error").contains(&code.to_owned()),
        "expected {code}: {report:#}"
    );
    report
}

#[test]
fn verify_passes_for_every_identity_tag_of_a_release() {
    let repo = release_repo();
    repo.release(&[]);
    let identities = ["mirror@1.3.0", "sample-library@1.3.0", "0.0.1"];
    for tag in identities {
        let report = assert_verified(&repo, tag, &identities);
        assert_eq!(report["target"], git(&repo.root, &["rev-parse", "HEAD"]));
        assert_eq!(
            report["generator"],
            format!("intentional {}", env!("CARGO_PKG_VERSION"))
        );
    }
    repo.cli()
        .args(["verify", "sample-library@1.3.0"])
        .assert()
        .success()
        .stdout(predicates::str::starts_with(format!(
            "verified sample-library@1.3.0 at {}\n",
            git(&repo.root, &["rev-parse", "HEAD"])
        )));
}

#[test]
fn verify_reads_an_earlier_release_from_history() {
    let repo = release_repo();
    repo.release(&[]);
    repo.write("release-notes.md", "Release executor notes.\n");
    repo.commit("record executor notes");
    repo.intent("sample-otter-0002", "Add a second capability.");
    repo.release(&[]);
    assert_verified(
        &repo,
        "sample-library@1.3.0",
        &["mirror@1.3.0", "sample-library@1.3.0", "0.0.1"],
    );
    assert_verified(
        &repo,
        "sample-library@1.4.0",
        &["mirror@1.4.0", "sample-library@1.4.0", "0.0.2"],
    );
}

#[test]
fn verify_passes_for_a_channel_release_after_later_commits() {
    let repo = release_repo();
    repo.cli()
        .args(["apply", "--channel", "rc"])
        .assert()
        .success();
    repo.commit("apply release candidate");
    repo.write("release-notes.md", "Release executor notes.\n");
    repo.commit("record executor notes");
    repo.cli()
        .args(["tag", "--channel", "rc"])
        .assert()
        .success();
    repo.cli()
        .args(["tag", "--channel", "rc", "--phase", "after-publication"])
        .assert()
        .success();
    assert_verified(
        &repo,
        "sample-library@1.3.0-rc.1",
        &[
            "mirror@1.3.0-rc.1",
            "sample-library@1.3.0-rc.1",
            "0.0.1-rc.1",
        ],
    );
}

#[test]
fn verify_fails_for_a_missing_tag() {
    let repo = release_repo();
    assert_fails_with(&repo, "sample-library@9.9.9", "tag-missing");
}

#[test]
fn verify_fails_for_a_lightweight_tag() {
    let repo = release_repo();
    let report = assert_fails_with(&repo, "sample-library@1.2.3", "tag-not-annotated");
    assert_eq!(report["guarantees"], serde_json::json!([]));
}

#[test]
fn verify_fails_for_an_annotated_tag_without_a_tag_record() {
    let repo = release_repo();
    git(&repo.root, &["tag", "-a", "notes", "-m", "Release notes"]);
    assert_fails_with(&repo, "notes", "tag-record-missing");
}

#[test]
fn verify_fails_for_a_tag_record_that_breaks_its_schema() {
    let repo = release_repo();
    repo.release(&[]);
    let message = repo
        .message("mirror@1.3.0")
        .replace("contract: contract-1", "contract: contract-9");
    repo.forge("mirror@1.3.0", "HEAD", &message);
    assert_fails_with(&repo, "mirror@1.3.0", "tag-record-invalid");
}

#[test]
fn verify_fails_for_a_tagger_other_than_intentional() {
    let repo = release_repo();
    repo.release(&[]);
    let message = repo.message("mirror@1.3.0");
    git(&repo.root, &["tag", "-d", "mirror@1.3.0"]);
    git(&repo.root, &["tag", "-a", "mirror@1.3.0", "-m", &message]);
    assert_fails_with(&repo, "mirror@1.3.0", "tagger-mismatch");
}

#[test]
fn verify_refuses_a_baseline_tag_as_a_release() {
    let repo = TestRepo::new();
    repo.write(
        "package.json",
        "{\n  \"name\": \"sample-library\",\n  \"version\": \"1.2.3\"\n}\n",
    );
    repo.write(".intentional/config.yml", CONFIG);
    repo.commit("add fixture");
    repo.cli()
        .args(["tag", "--baseline", "--version", "workspace/release=0.1.0"])
        .assert()
        .success();
    assert_fails_with(&repo, "sample-library@1.2.3", "baseline-record");
}

#[test]
fn verify_fails_for_an_altered_plan_digest() {
    let repo = release_repo();
    repo.release(&[]);
    let digest = repo
        .message("sample-library@1.3.0")
        .lines()
        .find_map(|line| line.strip_prefix("plan-digest: "))
        .expect("plan digest")
        .to_owned();
    let altered = format!("sha256:{}", "d".repeat(64));
    for name in ["sample-library@1.3.0", "mirror@1.3.0", "0.0.1"] {
        let message = repo.message(name).replace(&digest, &altered);
        repo.forge(name, "HEAD", &message);
    }
    assert_fails_with(&repo, "sample-library@1.3.0", "plan-digest-mismatch");
}

#[test]
fn verify_reports_a_different_producing_version_as_an_advisory() {
    let repo = release_repo();
    repo.release(&[]);
    let digest = repo
        .message("sample-library@1.3.0")
        .lines()
        .find_map(|line| line.strip_prefix("plan-digest: "))
        .expect("plan digest")
        .to_owned();
    let altered = format!("sha256:{}", "d".repeat(64));
    let current = format!("generator: intentional {}", env!("CARGO_PKG_VERSION"));
    for name in ["sample-library@1.3.0", "mirror@1.3.0", "0.0.1"] {
        let message = repo
            .message(name)
            .replace(&digest, &altered)
            .replace(&current, "generator: intentional 0.0.1");
        repo.forge(name, "HEAD", &message);
    }
    let (code, report) = repo.verify("sample-library@1.3.0");
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(
        codes(&report, "advisory"),
        ["producing-version-differs".to_owned()]
    );
    assert!(codes(&report, "error").is_empty(), "{report:#}");
    assert_eq!(
        report["guarantees"],
        serde_json::json!(["tag-record", "materialized-source", "identity-tags"])
    );
}

#[test]
fn verify_fails_for_a_materialized_tree_that_differs_from_the_reconstruction() {
    let repo = release_repo();
    repo.cli().arg("apply").assert().success();
    let changelog = repo.read("CHANGELOG.md");
    repo.write(
        "CHANGELOG.md",
        &changelog.replace("- Add a sample capability.", "- Add an edited capability."),
    );
    repo.commit("apply release with an edited changelog");
    repo.cli().arg("tag").assert().success();
    repo.cli()
        .args(["tag", "--phase", "after-publication"])
        .assert()
        .success();
    let report = assert_fails_with(&repo, "sample-library@1.3.0", "materialized-tree-mismatch");
    let message = report["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .find(|diagnostic| diagnostic["code"] == "materialized-tree-mismatch")
        .expect("diagnostic")["message"]
        .as_str()
        .expect("message")
        .to_owned();
    assert!(message.ends_with("CHANGELOG.md"), "{message}");
}

#[test]
fn verify_fails_for_a_missing_identity_tag() {
    let repo = release_repo();
    repo.release(&[]);
    git(&repo.root, &["tag", "-d", "0.0.1"]);
    let report = assert_fails_with(&repo, "sample-library@1.3.0", "identity-tag-missing");
    assert_eq!(
        report["identity_tags"],
        serde_json::json!(["mirror@1.3.0", "sample-library@1.3.0"])
    );
}

#[test]
fn verify_fails_for_an_identity_tag_from_another_release() {
    let repo = release_repo();
    repo.release(&[]);
    let message = repo.message("mirror@1.3.0");
    let digest = message
        .lines()
        .find_map(|line| line.strip_prefix("plan-digest: "))
        .expect("plan digest")
        .to_owned();
    repo.forge(
        "mirror@1.3.0",
        "HEAD",
        &message.replace(&digest, &format!("sha256:{}", "e".repeat(64))),
    );
    let report = assert_fails_with(&repo, "sample-library@1.3.0", "identity-tag-mismatch");
    assert_eq!(
        report["identity_tags"],
        serde_json::json!(["sample-library@1.3.0", "0.0.1"])
    );
}

/// Tags written by released `0.2.0`: the fixture plan and tag records in
/// `docs/specifications/fixtures/released-0.2.0`, on the workspace the
/// fixture README describes.
#[test]
fn tags_created_by_released_0_2_0_verify() {
    let fixtures = "docs/specifications/fixtures/released-0.2.0";
    let repo = TestRepo::new();
    repo.write(
        "package.json",
        "{\n  \"name\": \"sample-library\",\n  \"version\": \"1.2.3\"\n}\n",
    );
    repo.write(
        ".intentional/config.yml",
        &repository_file(&format!("{fixtures}/release-config.yml")),
    );
    repo.commit("add fixture");
    git(&repo.root, &["tag", "sample-library@1.2.3"]);
    repo.write(
        ".intentional/intents/swift-falcon-c3b1.md",
        "---\nsample-library: minor\n---\n\nAdd a sample capability.\n",
    );
    repo.commit("add intent");
    repo.cli().arg("apply").assert().success();
    repo.commit("apply release");
    repo.forge(
        "sample-library@1.3.0",
        "HEAD",
        &repository_file(&format!("{fixtures}/release-unit-tag-record.txt")),
    );
    repo.forge(
        "0.0.1",
        "HEAD",
        &repository_file(&format!("{fixtures}/workspace-tag-record.txt")),
    );
    let plan: Value =
        serde_json::from_str(&repository_file(&format!("{fixtures}/release-plan.json")))
            .expect("fixture plan");
    let report = assert_verified(
        &repo,
        "sample-library@1.3.0",
        &["sample-library@1.3.0", "0.0.1"],
    );
    assert_eq!(report["plan_digest"], plan["digest"]);
    assert_eq!(report["generator"], "intentional 0.2.0");
}

#[test]
fn verify_fails_for_a_tagger_time_other_than_the_commit_time() {
    let repo = release_repo();
    repo.release(&[]);
    let message = repo.message("mirror@1.3.0");
    git(&repo.root, &["tag", "-d", "mirror@1.3.0"]);
    let output = ProcessCommand::new("git")
        .args(["tag", "-a", "mirror@1.3.0", "-m", &message])
        .env("GIT_COMMITTER_NAME", "Intentional")
        .env("GIT_COMMITTER_EMAIL", "intentional@wyrd.company")
        .env("GIT_COMMITTER_DATE", "1000000000 +0000")
        .current_dir(&repo.root)
        .output()
        .expect("run git tag");
    assert!(output.status.success(), "{output:?}");
    assert_fails_with(&repo, "mirror@1.3.0", "tagger-mismatch");
}

#[test]
fn verify_fails_when_the_release_commit_keeps_a_consumed_intent() {
    let repo = release_repo();
    repo.intent("sample-otter-0002", "Add a second capability.");
    let kept = repo.read(".intentional/intents/sample-otter-0002.md");
    repo.cli().arg("apply").assert().success();
    repo.write(".intentional/intents/sample-otter-0002.md", &kept);
    repo.commit("apply release but keep one intent");
    repo.cli().arg("tag").assert().success();
    repo.cli()
        .args(["tag", "--phase", "after-publication"])
        .assert()
        .success();
    let report = assert_fails_with(&repo, "sample-library@1.3.0", "materialized-tree-mismatch");
    assert!(
        report
            .to_string()
            .contains(".intentional/intents/sample-otter-0002.md"),
        "{report:#}"
    );
}

#[test]
fn verify_fails_for_an_identity_tag_at_another_commit() {
    let repo = release_repo();
    repo.release(&[]);
    let message = repo.message("mirror@1.3.0");
    repo.forge("mirror@1.3.0", "HEAD^", &message);
    let report = assert_fails_with(&repo, "sample-library@1.3.0", "identity-tag-mismatch");
    assert!(
        report.to_string().contains("unexpected target"),
        "{report:#}"
    );
}

#[test]
fn verify_reads_an_earlier_channel_iteration() {
    let repo = release_repo();
    for iteration in ["first", "second"] {
        repo.cli()
            .args(["apply", "--channel", "rc"])
            .assert()
            .success();
        repo.commit(&format!("apply {iteration} release candidate"));
        repo.cli()
            .args(["tag", "--channel", "rc"])
            .assert()
            .success();
        repo.cli()
            .args(["tag", "--channel", "rc", "--phase", "after-publication"])
            .assert()
            .success();
    }
    assert_verified(
        &repo,
        "sample-library@1.3.0-rc.1",
        &[
            "mirror@1.3.0-rc.1",
            "sample-library@1.3.0-rc.1",
            "0.0.1-rc.1",
        ],
    );
}
