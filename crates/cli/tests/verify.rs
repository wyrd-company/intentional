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

    /// Replace tag `name` with a copy of its tag object that has no tagger
    /// header, as Intentional before deterministic taggers wrote it.
    fn strip_tagger(&self, name: &str) {
        use std::io::Write;
        let raw = git(&self.root, &["cat-file", "tag", name]);
        let raw = raw
            .lines()
            .filter(|line| !line.starts_with("tagger "))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let mut child = ProcessCommand::new("git")
            .args(["hash-object", "--literally", "-t", "tag", "-w", "--stdin"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .current_dir(&self.root)
            .spawn()
            .expect("run git hash-object");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(raw.as_bytes())
            .expect("write tag object");
        let output = child.wait_with_output().expect("git hash-object");
        assert!(output.status.success(), "{output:?}");
        let object = String::from_utf8(output.stdout).expect("object id");
        git(
            &self.root,
            &["update-ref", &format!("refs/tags/{name}"), object.trim()],
        );
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

/// The tags `tag --baseline` creates on the `CONFIG` workspace at 1.2.3 with
/// workspace tag stream `release` at 0.1.0, in tag ID order.
const BASELINE_TAGS: [&str; 3] = ["mirror@1.2.3", "sample-library@1.2.3", "0.1.0"];

/// The `CONFIG` workspace at 1.2.3, committed, with a baseline established.
fn baseline_repo() -> TestRepo {
    let repo = TestRepo::new();
    repo.write(
        "package.json",
        "{\n  \"name\": \"sample-library\",\n  \"version\": \"1.2.3\"\n}\n",
    );
    repo.write(".intentional/config.yml", CONFIG);
    repo.write(".intentional/intents/.gitkeep", "");
    repo.commit("add fixture");
    repo.cli()
        .args(["tag", "--baseline", "--version", "workspace/release=0.1.0"])
        .assert()
        .success();
    repo
}

fn assert_baseline_verified(repo: &TestRepo, tag: &str, identities: &[&str]) -> Value {
    let (code, report) = repo.verify(tag);
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(report["verified"], true);
    assert_eq!(report["baseline"], true);
    assert_eq!(
        report["guarantees"],
        serde_json::json!(["tag-record", "baseline-reconstruction", "identity-tags"])
    );
    assert_eq!(report["identity_tags"], serde_json::json!(identities));
    assert!(codes(&report, "error").is_empty(), "{report:#}");
    report
}

fn plan_digest(message: &str) -> String {
    message
        .lines()
        .find_map(|line| line.strip_prefix("plan-digest: "))
        .expect("plan digest")
        .to_owned()
}

#[test]
fn verify_passes_for_every_tag_of_a_baseline() {
    let repo = baseline_repo();
    let head = git(&repo.root, &["rev-parse", "HEAD"]);
    for tag in BASELINE_TAGS {
        let report = assert_baseline_verified(&repo, tag, &BASELINE_TAGS);
        assert_eq!(report["target"], head);
        assert_eq!(
            report["plan_digest"],
            plan_digest(&repo.message("sample-library@1.2.3"))
        );
    }
    repo.cli()
        .args(["verify", "0.1.0"])
        .assert()
        .success()
        .stdout(predicates::str::starts_with(format!(
            "verified 0.1.0 at {head}\nguarantee: tag-record\nguarantee: baseline-reconstruction\n"
        )));
}

#[test]
fn verify_reads_a_baseline_from_history_after_a_release() {
    let repo = baseline_repo();
    repo.intent("sample-otter-0001", "Add a sample capability.");
    repo.release(&[]);
    assert_baseline_verified(&repo, "sample-library@1.2.3", &BASELINE_TAGS);
    assert_verified(
        &repo,
        "sample-library@1.3.0",
        &["mirror@1.3.0", "sample-library@1.3.0", "0.1.1"],
    );
}

/// A workspace whose `sample-library@1.2.3` tag predates Intentional: `tag
/// --baseline` takes the version from it and does not retag it.
fn baseline_repo_with_history(history: &[&str]) -> TestRepo {
    let repo = TestRepo::new();
    repo.write(
        "package.json",
        "{\n  \"name\": \"sample-library\",\n  \"version\": \"1.2.3\"\n}\n",
    );
    repo.commit("release 1.2.3");
    git(&repo.root, history);
    repo.write(".intentional/config.yml", CONFIG);
    repo.write(".intentional/intents/.gitkeep", "");
    repo.commit("adopt intentional");
    repo.cli()
        .args(["tag", "--baseline", "--version", "workspace/release=0.1.0"])
        .assert()
        .success();
    repo
}

#[test]
fn verify_passes_for_a_baseline_whose_version_a_tag_without_a_record_supplies() {
    for history in [
        &["tag", "sample-library@1.2.3"][..],
        &["tag", "-a", "sample-library@1.2.3", "-m", "Release 1.2.3"][..],
    ] {
        let repo = baseline_repo_with_history(history);
        assert_ne!(
            git(&repo.root, &["rev-list", "-n", "1", "sample-library@1.2.3"]),
            git(&repo.root, &["rev-parse", "HEAD"])
        );
        for tag in ["mirror@1.2.3", "0.1.0"] {
            assert_baseline_verified(&repo, tag, &["mirror@1.2.3", "0.1.0"]);
        }
    }
}

#[test]
fn verify_fails_for_a_baseline_tag_that_only_later_history_holds() {
    let repo = baseline_repo();
    let message = repo.message("mirror@1.2.3");
    git(&repo.root, &["tag", "-d", "mirror@1.2.3"]);
    repo.write("release-notes.md", "Release executor notes.\n");
    repo.commit("record executor notes");
    for history in [&["tag", "mirror@1.2.3"][..], &[][..]] {
        if history.is_empty() {
            repo.forge("mirror@1.2.3", "HEAD", &message);
        } else {
            git(&repo.root, history);
        }
        let report = assert_fails_with(&repo, "sample-library@1.2.3", "identity-tag-missing");
        assert_eq!(
            report["identity_tags"],
            serde_json::json!(["sample-library@1.2.3", "0.1.0"])
        );
    }
}

#[test]
fn verify_fails_for_an_altered_baseline_plan_digest() {
    let repo = baseline_repo();
    let digest = plan_digest(&repo.message("sample-library@1.2.3"));
    let altered = format!("sha256:{}", "d".repeat(64));
    for name in BASELINE_TAGS {
        let message = repo.message(name).replace(&digest, &altered);
        repo.forge(name, "HEAD", &message);
    }
    let report = assert_fails_with(&repo, "sample-library@1.2.3", "baseline-digest-mismatch");
    assert_eq!(report["identity_tags"], serde_json::json!(BASELINE_TAGS));
    assert_eq!(
        report["guarantees"],
        serde_json::json!(["tag-record", "identity-tags"])
    );
}

#[test]
fn verify_reports_a_baseline_from_another_producing_version_as_an_advisory() {
    let repo = baseline_repo();
    let digest = plan_digest(&repo.message("sample-library@1.2.3"));
    let altered = format!("sha256:{}", "d".repeat(64));
    let current = format!("generator: intentional {}", env!("CARGO_PKG_VERSION"));
    for name in BASELINE_TAGS {
        let message = repo
            .message(name)
            .replace(&digest, &altered)
            .replace(&current, "generator: intentional 0.0.1");
        repo.forge(name, "HEAD", &message);
    }
    let (code, report) = repo.verify("sample-library@1.2.3");
    assert_eq!(code, 0, "{report:#}");
    assert_eq!(
        codes(&report, "advisory"),
        ["producing-version-differs".to_owned()]
    );
    assert!(codes(&report, "error").is_empty(), "{report:#}");
    assert_eq!(
        report["guarantees"],
        serde_json::json!(["tag-record", "identity-tags"])
    );
}

#[test]
fn verify_recomputes_a_baseline_digest_under_the_recorded_generator() {
    let repo = baseline_repo();
    let digest = plan_digest(&repo.message("sample-library@1.2.3"));
    let versions = [("sample-library", "1.2.3"), ("workspace/release", "0.1.0")]
        .into_iter()
        .map(|(id, version)| (id.to_owned(), version.to_owned()))
        .collect();
    let earlier = intentional_core::baseline_digest("contract-1", "0.0.1", &versions)
        .expect("baseline digest");
    assert_ne!(earlier, digest);
    let current = format!("generator: intentional {}", env!("CARGO_PKG_VERSION"));
    for name in BASELINE_TAGS {
        let message = repo
            .message(name)
            .replace(&digest, &earlier)
            .replace(&current, "generator: intentional 0.0.1");
        repo.forge(name, "HEAD", &message);
    }
    let report = assert_baseline_verified(&repo, "sample-library@1.2.3", &BASELINE_TAGS);
    assert!(codes(&report, "advisory").is_empty(), "{report:#}");
    assert_eq!(report["generator"], "intentional 0.0.1");
}

#[test]
fn verify_fails_for_a_missing_baseline_identity_tag() {
    let repo = baseline_repo();
    git(&repo.root, &["tag", "-d", "mirror@1.2.3"]);
    let report = assert_fails_with(&repo, "sample-library@1.2.3", "identity-tag-missing");
    assert_eq!(
        report["identity_tags"],
        serde_json::json!(["sample-library@1.2.3", "0.1.0"])
    );
    assert_eq!(
        report["guarantees"],
        serde_json::json!(["tag-record", "baseline-reconstruction"])
    );
}

#[test]
fn verify_fails_for_a_missing_baseline_tag_that_alone_supplies_a_version() {
    let repo = baseline_repo();
    git(&repo.root, &["tag", "-d", "0.1.0"]);
    assert_fails_with(&repo, "sample-library@1.2.3", "reconstruction-failed");
}

#[test]
fn verify_fails_through_a_peer_baseline_tag_with_another_tagger() {
    let repo = baseline_repo();
    let message = repo.message("mirror@1.2.3");
    git(&repo.root, &["tag", "-d", "mirror@1.2.3"]);
    git(&repo.root, &["tag", "-a", "mirror@1.2.3", "-m", &message]);
    let report = assert_fails_with(&repo, "sample-library@1.2.3", "identity-tag-mismatch");
    assert!(
        report["diagnostics"]
            .to_string()
            .contains("mirror@1.2.3 has unexpected tagger"),
        "{report:#}"
    );
    assert_eq!(
        report["identity_tags"],
        serde_json::json!(["sample-library@1.2.3", "0.1.0"])
    );
    repo.strip_tagger("mirror@1.2.3");
    assert_fails_with(&repo, "sample-library@1.2.3", "identity-tag-mismatch");
}

#[test]
fn verify_fails_for_a_peer_baseline_record_that_disagrees_on_any_identity_field() {
    let digest = plan_digest(&baseline_repo().message("mirror@1.2.3"));
    for (field, from, to) in [
        (
            "tag-id",
            "tag-id: release-unit/sample-library/mirror",
            "tag-id: release-unit/sample-library/primary",
        ),
        ("version", "version: 1.2.3", "version: 1.2.4"),
        (
            "plan-digest",
            digest.as_str(),
            &*format!("sha256:{}", "e".repeat(64)),
        ),
        ("baseline", "baseline: true", "baseline: false"),
    ] {
        let repo = baseline_repo();
        let message = repo.message("mirror@1.2.3");
        assert!(message.contains(from), "{field}: {message}");
        repo.forge("mirror@1.2.3", "HEAD", &message.replace(from, to));
        let report = assert_fails_with(&repo, "sample-library@1.2.3", "identity-tag-mismatch");
        assert!(
            report["diagnostics"]
                .to_string()
                .contains(&format!("mirror@1.2.3 has unexpected {field}")),
            "{field}: {report:#}"
        );
    }
}

#[test]
fn verify_fails_for_a_baseline_record_outside_the_baseline_set() {
    let repo = baseline_repo();
    let message = repo.message("sample-library@1.2.3").replace(
        "tag-id: release-unit/sample-library/primary",
        "tag-id: release-unit/unknown/primary",
    );
    repo.forge("extra", "HEAD", &message);
    let report = assert_fails_with(&repo, "extra", "identity-tag-mismatch");
    assert_eq!(report["identity_tags"], serde_json::json!(BASELINE_TAGS));
}

/// Rewrite one tag of `baseline_repo()` with a defect.
fn damage(repo: &TestRepo, name: &str, defect: &str) {
    match defect {
        "no tagger" => repo.strip_tagger(name),
        "another tagger" => {
            let message = repo.message(name);
            git(&repo.root, &["tag", "-d", name]);
            git(&repo.root, &["tag", "-a", name, "-m", &message]);
        }
        "baseline false" => {
            let message = repo
                .message(name)
                .replace("baseline: true", "baseline: false");
            repo.forge(name, "HEAD", &message);
        }
        "deleted" => {
            git(&repo.root, &["tag", "-d", name]);
        }
        _ => panic!("unknown defect {defect}"),
    }
}

/// Give the selected tag another generator version, so that its baseline
/// digest difference is an advisory.
fn from_another_generator(repo: &TestRepo, name: &str) {
    let current = format!("generator: intentional {}", env!("CARGO_PKG_VERSION"));
    let message = repo
        .message(name)
        .replace(&current, "generator: intentional 0.0.1");
    repo.forge(name, "HEAD", &message);
}

#[test]
fn verify_fails_through_the_workspace_tag_when_the_primary_lost_its_tagger_and_baseline_flag() {
    let repo = baseline_repo();
    damage(&repo, "sample-library@1.2.3", "baseline false");
    damage(&repo, "sample-library@1.2.3", "no tagger");
    damage(&repo, "mirror@1.2.3", "deleted");
    from_another_generator(&repo, "0.1.0");
    let report = assert_fails_with(&repo, "0.1.0", "identity-tag-mismatch");
    let errors = codes(&report, "error");
    assert!(
        errors.contains(&"identity-tag-missing".to_owned()),
        "{report:#}"
    );
    let diagnostics = report["diagnostics"].to_string();
    assert!(
        diagnostics.contains("sample-library@1.2.3 has unexpected tagger, baseline"),
        "{report:#}"
    );
    assert_eq!(report["identity_tags"], serde_json::json!(["0.1.0"]));
}

#[test]
fn verify_fails_through_the_primary_tag_when_the_workspace_peer_reads_baseline_false() {
    let repo = baseline_repo();
    damage(&repo, "0.1.0", "baseline false");
    from_another_generator(&repo, "sample-library@1.2.3");
    let report = assert_fails_with(&repo, "sample-library@1.2.3", "identity-tag-mismatch");
    assert!(
        report["diagnostics"]
            .to_string()
            .contains("0.1.0 has unexpected baseline"),
        "{report:#}"
    );
    assert_eq!(
        report["identity_tags"],
        serde_json::json!(["mirror@1.2.3", "sample-library@1.2.3"])
    );
}

/// Every defect in a primary or workspace tag fails verification through
/// every other selected tag, also when the selected tag names another
/// generator version. A stream's own tags cannot take it out of the set.
#[test]
fn verify_fails_for_each_primary_or_workspace_defect_through_every_other_tag() {
    for (damaged, defect, code, detail) in [
        (
            "sample-library@1.2.3",
            "no tagger",
            "identity-tag-mismatch",
            "sample-library@1.2.3 has unexpected tagger",
        ),
        (
            "sample-library@1.2.3",
            "another tagger",
            "identity-tag-mismatch",
            "sample-library@1.2.3 has unexpected tagger",
        ),
        (
            "sample-library@1.2.3",
            "baseline false",
            "identity-tag-mismatch",
            "sample-library@1.2.3 has unexpected baseline",
        ),
        (
            "sample-library@1.2.3",
            "deleted",
            "identity-tag-missing",
            "sample-library@1.2.3 does not exist",
        ),
        (
            "0.1.0",
            "no tagger",
            "identity-tag-mismatch",
            "0.1.0 has unexpected tagger",
        ),
        (
            "0.1.0",
            "another tagger",
            "identity-tag-mismatch",
            "0.1.0 has unexpected tagger",
        ),
        (
            "0.1.0",
            "baseline false",
            "identity-tag-mismatch",
            "0.1.0 has unexpected baseline",
        ),
    ] {
        for selected in BASELINE_TAGS.into_iter().filter(|tag| *tag != damaged) {
            for another_generator in [false, true] {
                let repo = baseline_repo();
                damage(&repo, damaged, defect);
                if another_generator {
                    from_another_generator(&repo, selected);
                }
                let case = format!(
                    "{damaged} {defect}, verify {selected}, another generator {another_generator}"
                );
                let (exit, report) = repo.verify(selected);
                assert_eq!(exit, 1, "{case}: {report:#}");
                assert!(
                    codes(&report, "error").contains(&code.to_owned()),
                    "{case}: {report:#}"
                );
                assert!(
                    report["diagnostics"].to_string().contains(detail),
                    "{case}: {report:#}"
                );
            }
        }
    }
}

/// `CONFIG` without the workspace tag's order after the primary tag, so that
/// the workspace tag stream can be baselined alone.
fn unordered_workspace_config() -> String {
    let ordered = "    require-phase: after-publication\n    tag-after:\n      - release-unit/sample-library/primary\n";
    assert!(CONFIG.contains(ordered));
    CONFIG.replace(ordered, "    require-phase: after-publication\n")
}

/// sample-library released at 1.3.0 with its primary and mirror tags only,
/// on `unordered_workspace_config()`; `later` commits once more after the
/// release. Then a baseline of the workspace tag stream at HEAD.
fn baseline_after_a_release_repo(later: bool) -> TestRepo {
    let repo = TestRepo::new();
    repo.write(
        "package.json",
        "{\n  \"name\": \"sample-library\",\n  \"version\": \"1.2.3\"\n}\n",
    );
    repo.write(".intentional/config.yml", &unordered_workspace_config());
    repo.write(".intentional/intents/.gitkeep", "");
    repo.commit("add fixture");
    git(&repo.root, &["tag", "sample-library@1.2.3"]);
    repo.intent("sample-otter-0001", "Add a sample capability.");
    repo.cli().arg("apply").assert().success();
    repo.commit("apply release");
    repo.cli().arg("tag").assert().success();
    if later {
        repo.write("release-notes.md", "Release executor notes.\n");
        repo.commit("record executor notes");
    }
    repo.cli()
        .args(["tag", "--baseline", "--version", "workspace/release=0.1.0"])
        .assert()
        .success()
        .stdout("create annotated tag 0.1.0\n");
    repo
}

#[test]
fn verify_passes_for_a_baseline_after_a_release_that_establishes_its_unit() {
    let repo = baseline_after_a_release_repo(true);
    assert_baseline_verified(&repo, "0.1.0", &["0.1.0"]);
}

/// A release tag record on an earlier commit establishes its unit outside
/// the baseline only when the verifying reader accepts it. Otherwise the unit
/// stays in the set, and its tags fail as baseline tags.
fn assert_earlier_release_record_fails(defect: &str, field: &str) {
    let repo = baseline_after_a_release_repo(true);
    let release = git(&repo.root, &["rev-parse", "HEAD~1"]);
    let name = "sample-library@1.3.0";
    let message = repo.message(name);
    match defect {
        "no tagger" => repo.strip_tagger(name),
        "another tagger" => {
            git(&repo.root, &["tag", "-d", name]);
            git(&repo.root, &["tag", "-a", name, "-m", &message, &release]);
        }
        "tag-id" => repo.forge(
            name,
            &release,
            &message.replace(
                "tag-id: release-unit/sample-library/primary",
                "tag-id: release-unit/sample-library/mirror",
            ),
        ),
        _ => repo.forge(
            name,
            &release,
            &message.replace("version: 1.3.0", "version: 1.3.1"),
        ),
    }
    let report = assert_fails_with(&repo, "0.1.0", "identity-tag-mismatch");
    let unexpected = report["diagnostics"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .filter_map(|diagnostic| diagnostic["message"].as_str())
        .find(|message| message.starts_with(&format!("identity tag {name} has unexpected target")))
        .unwrap_or_else(|| panic!("{defect}: {report:#}"))
        .to_owned();
    assert!(unexpected.contains(field), "{defect}: {unexpected}");
}

#[test]
fn verify_fails_for_a_baseline_whose_earlier_release_record_has_no_tagger() {
    assert_earlier_release_record_fails("no tagger", "tagger");
}

#[test]
fn verify_fails_for_a_baseline_whose_earlier_release_record_has_another_tagger() {
    assert_earlier_release_record_fails("another tagger", "tagger");
}

#[test]
fn verify_fails_for_a_baseline_whose_earlier_release_record_names_another_tag_id() {
    assert_earlier_release_record_fails("tag-id", "tag-id");
}

#[test]
fn verify_fails_for_a_baseline_whose_earlier_release_record_names_another_version() {
    assert_earlier_release_record_fails("version", "version");
}

/// `tag --baseline` leaves a lightweight or annotated tag without a tag
/// record in place at the baseline commit itself; verify accepts it as the
/// history that supplies its version.
#[test]
fn verify_passes_for_a_baseline_whose_recordless_primary_tag_marks_the_baseline_commit() {
    for history in [
        &["tag", "sample-library@1.2.3"][..],
        &["tag", "-a", "sample-library@1.2.3", "-m", "Release 1.2.3"][..],
    ] {
        let repo = TestRepo::new();
        repo.write(
            "package.json",
            "{\n  \"name\": \"sample-library\",\n  \"version\": \"1.2.3\"\n}\n",
        );
        repo.write(".intentional/config.yml", CONFIG);
        repo.write(".intentional/intents/.gitkeep", "");
        repo.commit("add fixture");
        git(&repo.root, history);
        repo.cli()
            .args(["tag", "--baseline", "--version", "workspace/release=0.1.0"])
            .assert()
            .success()
            .stdout("create annotated tag mirror@1.2.3\ncreate annotated tag 0.1.0\n");
        for tag in ["mirror@1.2.3", "0.1.0"] {
            assert_baseline_verified(&repo, tag, &["mirror@1.2.3", "0.1.0"]);
        }
    }
}

/// A Release tagged at the baseline commit establishes its unit there, so
/// `tag --baseline` creates only the tags of the other streams, and verify
/// leaves the Release's tags out of the baseline.
#[test]
fn verify_passes_for_a_baseline_at_the_commit_of_a_release_it_excludes() {
    let repo = baseline_after_a_release_repo(false);
    assert_eq!(
        git(&repo.root, &["rev-list", "-n", "1", "sample-library@1.3.0"]),
        git(&repo.root, &["rev-parse", "HEAD"])
    );
    assert_baseline_verified(&repo, "0.1.0", &["0.1.0"]);
}

#[test]
fn verify_passes_for_a_baseline_at_a_release_commit_from_another_producing_version() {
    let repo = baseline_after_a_release_repo(false);
    let message = repo.message("sample-library@1.3.0");
    let current = format!("generator: intentional {}", env!("CARGO_PKG_VERSION"));
    repo.forge(
        "sample-library@1.3.0",
        "HEAD",
        &message
            .replace(
                &plan_digest(&message),
                &format!("sha256:{}", "d".repeat(64)),
            )
            .replace(&current, "generator: intentional 0.0.1"),
    );
    assert_baseline_verified(&repo, "0.1.0", &["0.1.0"]);
}

/// A non-baseline tag record at the baseline commit leaves its unit out of
/// the baseline only when it is a Release tag there; otherwise the unit stays
/// in the set and its tags fail as baseline tags.
#[test]
fn verify_fails_for_a_baseline_at_a_release_commit_whose_release_tag_is_not_that_release() {
    for defect in [
        "no tagger",
        "another tagger",
        "altered plan-digest",
        "baseline digest",
    ] {
        let repo = baseline_after_a_release_repo(false);
        let name = "sample-library@1.3.0";
        let message = repo.message(name);
        let baseline = plan_digest(&repo.message("0.1.0"));
        match defect {
            "no tagger" => repo.strip_tagger(name),
            "another tagger" => {
                git(&repo.root, &["tag", "-d", name]);
                git(&repo.root, &["tag", "-a", name, "-m", &message]);
            }
            "altered plan-digest" => repo.forge(
                name,
                "HEAD",
                &message.replace(
                    &plan_digest(&message),
                    &format!("sha256:{}", "d".repeat(64)),
                ),
            ),
            _ => repo.forge(
                name,
                "HEAD",
                &message.replace(&plan_digest(&message), &baseline),
            ),
        }
        let report = assert_fails_with(&repo, "0.1.0", "identity-tag-mismatch");
        assert!(
            report["diagnostics"]
                .to_string()
                .contains(&format!("identity tag {name} has unexpected")),
            "{defect}: {report:#}"
        );
    }
}

/// `verify` reads Git history only: a baseline created from configuration
/// that the baseline commit does not hold cannot be recomputed.
#[test]
fn verify_fails_for_a_baseline_created_from_uncommitted_configuration() {
    let repo = TestRepo::new();
    repo.write(
        "package.json",
        "{\n  \"name\": \"sample-library\",\n  \"version\": \"1.2.3\"\n}\n",
    );
    repo.commit("add package");
    repo.write(".intentional/config.yml", CONFIG);
    repo.cli()
        .args(["tag", "--baseline", "--version", "workspace/release=0.1.0"])
        .assert()
        .success();
    let report = assert_fails_with(&repo, "sample-library@1.2.3", "reconstruction-failed");
    assert!(
        report["diagnostics"]
            .to_string()
            .contains(".intentional/config.yml"),
        "{report:#}"
    );
}

/// A baseline created from a projection version that the baseline commit
/// does not hold breaks the projection rule of `tag --baseline` there.
#[test]
fn verify_fails_for_a_baseline_created_from_an_uncommitted_projection_version() {
    let repo = TestRepo::new();
    repo.write(
        "package.json",
        "{\n  \"name\": \"sample-library\",\n  \"version\": \"1.2.3\"\n}\n",
    );
    repo.write(".intentional/config.yml", CONFIG);
    repo.commit("add fixture");
    repo.write(
        "package.json",
        "{\n  \"name\": \"sample-library\",\n  \"version\": \"1.2.4\"\n}\n",
    );
    repo.cli()
        .args(["tag", "--baseline", "--version", "workspace/release=0.1.0"])
        .assert()
        .success();
    for tag in ["0.1.0", "sample-library@1.2.4"] {
        let report = assert_fails_with(&repo, tag, "baseline-projection-mismatch");
        assert!(
            report["diagnostics"]
                .to_string()
                .contains("package.json is 1.2.3"),
            "{report:#}"
        );
        assert_eq!(
            report["guarantees"],
            serde_json::json!(["tag-record", "baseline-reconstruction", "identity-tags"])
        );
    }
}

/// `tag --baseline` does not read the projections of a unit whose version a
/// tag without a tag record supplies, so verify does not either.
#[test]
fn verify_skips_the_projections_of_a_unit_whose_version_a_recordless_tag_supplies() {
    let repo = TestRepo::new();
    repo.write(
        "package.json",
        "{\n  \"name\": \"sample-library\",\n  \"version\": \"1.2.3\"\n}\n",
    );
    repo.commit("release 1.2.3");
    git(&repo.root, &["tag", "sample-library@1.2.3"]);
    repo.write(
        "package.json",
        "{\n  \"name\": \"sample-library\",\n  \"version\": \"1.3.0-dev\"\n}\n",
    );
    repo.write(".intentional/config.yml", CONFIG);
    repo.write(".intentional/intents/.gitkeep", "");
    repo.commit("adopt intentional during development");
    repo.cli()
        .args(["tag", "--baseline", "--version", "workspace/release=0.1.0"])
        .assert()
        .success()
        .stdout("create annotated tag mirror@1.2.3\ncreate annotated tag 0.1.0\n");
    assert_baseline_verified(&repo, "0.1.0", &["mirror@1.2.3", "0.1.0"]);
}

/// The baseline tags written by released `0.2.0` on the workspace the fixture
/// README describes.
#[test]
fn a_baseline_created_by_released_0_2_0_verifies() {
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
    let primary = repository_file(&format!("{fixtures}/baseline-tag-record.txt"));
    repo.forge("sample-library@1.2.3", "HEAD", &primary);
    repo.forge(
        "0.1.0",
        "HEAD",
        &repository_file(&format!("{fixtures}/baseline-workspace-tag-record.txt")),
    );
    for tag in ["sample-library@1.2.3", "0.1.0"] {
        let report = assert_baseline_verified(&repo, tag, &["sample-library@1.2.3", "0.1.0"]);
        assert_eq!(report["plan_digest"], plan_digest(&primary));
        assert_eq!(report["generator"], "intentional 0.2.0");
    }
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
fn verify_fails_for_a_release_identity_tag_without_a_record_on_an_earlier_commit() {
    let repo = release_repo();
    repo.release(&[]);
    git(&repo.root, &["tag", "-d", "mirror@1.3.0"]);
    git(&repo.root, &["tag", "mirror@1.3.0", "HEAD~1"]);
    let report = assert_fails_with(&repo, "sample-library@1.3.0", "identity-tag-mismatch");
    assert_eq!(
        report["identity_tags"],
        serde_json::json!(["sample-library@1.3.0", "0.0.1"])
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

#[test]
fn verify_fails_for_a_tag_outside_the_release() {
    let repo = release_repo();
    repo.release(&[]);
    let message = repo.message("sample-library@1.3.0");
    repo.forge("copy@1.3.0", "HEAD", &message);
    assert_fails_with(&repo, "copy@1.3.0", "identity-tag-mismatch");
}

#[test]
fn verify_fails_for_a_tag_without_a_tagger() {
    let repo = release_repo();
    repo.release(&[]);
    repo.strip_tagger("mirror@1.3.0");
    assert_fails_with(&repo, "mirror@1.3.0", "tagger-mismatch");
}

#[test]
fn verify_fails_through_an_unchanged_peer_of_an_identity_tag_with_another_tagger() {
    let repo = release_repo();
    repo.release(&[]);
    let message = repo.message("mirror@1.3.0");
    git(&repo.root, &["tag", "-d", "mirror@1.3.0"]);
    git(&repo.root, &["tag", "-a", "mirror@1.3.0", "-m", &message]);
    let report = assert_fails_with(&repo, "sample-library@1.3.0", "identity-tag-mismatch");
    assert_eq!(
        report["identity_tags"],
        serde_json::json!(["sample-library@1.3.0", "0.0.1"])
    );
}

#[test]
fn verify_fails_through_an_unchanged_peer_of_an_identity_tag_without_a_tagger() {
    let repo = release_repo();
    repo.release(&[]);
    repo.strip_tagger("mirror@1.3.0");
    let report = assert_fails_with(&repo, "sample-library@1.3.0", "identity-tag-mismatch");
    assert_eq!(
        report["identity_tags"],
        serde_json::json!(["sample-library@1.3.0", "0.0.1"])
    );
}

/// Each identity field a peer tag record can disagree on, one at a time.
#[test]
fn verify_fails_for_a_peer_record_that_disagrees_on_any_identity_field() {
    for (field, from, to) in [
        (
            "tag-id",
            "tag-id: release-unit/sample-library/mirror",
            "tag-id: release-unit/sample-library/primary",
        ),
        ("version", "version: 1.3.0", "version: 1.3.1"),
        ("baseline", "baseline: false", "baseline: true"),
    ] {
        let repo = release_repo();
        repo.release(&[]);
        let message = repo.message("mirror@1.3.0");
        assert!(message.contains(from), "{field}");
        repo.forge("mirror@1.3.0", "HEAD", &message.replace(from, to));
        let report = assert_fails_with(&repo, "sample-library@1.3.0", "identity-tag-mismatch");
        assert!(
            report.to_string().contains(&format!("unexpected {field}")),
            "{field}: {report:#}"
        );
    }
}

#[test]
fn verify_ignores_a_record_for_an_unknown_release_unit_when_finding_a_channel_source() {
    let repo = release_repo();
    repo.release(&["--channel", "rc"]);
    let message = repo.message("sample-library@1.3.0-rc.1").replace(
        "release-unit/sample-library/primary",
        "release-unit/aaa/primary",
    );
    repo.forge("unrelated@1.3.0-rc.1", "HEAD", &message);
    assert_verified(
        &repo,
        "sample-library@1.3.0-rc.1",
        &[
            "mirror@1.3.0-rc.1",
            "sample-library@1.3.0-rc.1",
            "0.0.1-rc.1",
        ],
    );
    assert_fails_with(&repo, "unrelated@1.3.0-rc.1", "identity-tag-mismatch");
}

#[test]
fn verify_ignores_a_record_under_another_tag_name_when_finding_a_channel_source() {
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
    // A record whose version names another release, under a tag name that
    // no configured template renders for it, must not choose the source.
    let message = repo
        .message("sample-library@1.3.0-rc.1")
        .replace("version: 1.3.0-rc.1", "version: 1.4.0-rc.1");
    repo.forge("zz-unrelated@1.4.0-rc.1", "HEAD", &message);
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
fn verify_fails_for_a_tagger_that_differs_only_in_name_or_email() {
    for (name, email) in [
        ("Another Tagger", "intentional@wyrd.company"),
        ("Intentional", "another@example.invalid"),
    ] {
        let repo = release_repo();
        repo.release(&[]);
        let message = repo.message("mirror@1.3.0");
        let date = git(
            &repo.root,
            &["log", "-1", "--format=%cd", "--date=raw", "HEAD"],
        );
        git(&repo.root, &["tag", "-d", "mirror@1.3.0"]);
        let output = ProcessCommand::new("git")
            .args(["tag", "-a", "mirror@1.3.0", "-m", &message])
            .env("GIT_COMMITTER_NAME", name)
            .env("GIT_COMMITTER_EMAIL", email)
            .env("GIT_COMMITTER_DATE", date)
            .current_dir(&repo.root)
            .output()
            .expect("run git tag");
        assert!(output.status.success(), "{output:?}");
        assert_fails_with(&repo, "mirror@1.3.0", "tagger-mismatch");
        assert_fails_with(&repo, "sample-library@1.3.0", "identity-tag-mismatch");
    }
}

#[test]
fn verify_fails_through_a_peer_that_is_lightweight() {
    let repo = release_repo();
    repo.release(&[]);
    git(&repo.root, &["tag", "-d", "mirror@1.3.0"]);
    git(&repo.root, &["tag", "mirror@1.3.0"]);
    let report = assert_fails_with(&repo, "sample-library@1.3.0", "identity-tag-mismatch");
    assert!(
        report.to_string().contains("carries no tag record"),
        "{report:#}"
    );
}

#[test]
fn verify_fails_through_a_peer_whose_record_breaks_its_schema() {
    let repo = release_repo();
    repo.release(&[]);
    let message = repo
        .message("mirror@1.3.0")
        .replace("contract: contract-1", "contract: contract-9");
    repo.forge("mirror@1.3.0", "HEAD", &message);
    let report = assert_fails_with(&repo, "sample-library@1.3.0", "identity-tag-mismatch");
    assert!(
        report.to_string().contains("violates its published schema"),
        "{report:#}"
    );
}

#[test]
fn verify_ignores_a_record_tagged_by_another_identity_when_finding_a_channel_source() {
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
    // A configured name at another version, but not tagged by Intentional.
    let message = repo
        .message("sample-library@1.3.0-rc.1")
        .replace("version: 1.3.0-rc.1", "version: 1.4.0-rc.1");
    git(
        &repo.root,
        &["tag", "-a", "sample-library@1.4.0-rc.1", "-m", &message],
    );
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
