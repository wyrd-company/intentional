// ---
// relationships:
//   tests: intent-driven-polyglot-release
// ---

use assert_cmd::Command;
use predicates::prelude::*;
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

    fn commit(&self, message: &str) {
        git(&self.root, &["add", "-A"]);
        git(&self.root, &["commit", "-q", "-m", message]);
    }

    fn tag(&self, tag: &str) {
        git(&self.root, &["tag", tag]);
    }

    fn cli(&self) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("intentional"));
        command.arg("-C").arg(&self.root);
        command
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
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8 git output")
        .trim()
        .to_owned()
}

fn npm_manifest(version: &str) -> String {
    format!("{{\n  \"name\": \"sample-library\",\n  \"version\": \"{version}\"\n}}\n")
}

fn config(mode: &str) -> String {
    format!(
        "$schema: https://intentional.foo/schemas/config.yml\npackages:\n  sample:\n    path: .\n    projections:\n      - adapter: npm\n        file: package.json\n        mode: {mode}\n"
    )
}

fn intent(bump: &str, message: &str) -> String {
    format!("---\nsample: {bump}\n---\n\n{message}\n")
}

#[test]
fn init_add_status_plan_apply_tag_round_trip() {
    let repo = TestRepo::new();
    repo.write("package.json", &npm_manifest("0.0.0"));
    repo.commit("add fixture");

    repo.cli().arg("init").assert().success();
    let generated = fs::read_to_string(repo.root.join(".intentional/config.yml")).unwrap();
    repo.write(
        ".intentional/config.yml",
        &generated.replace("global-tag: false", "global-tag: true"),
    );
    repo.cli()
        .args([
            "add",
            "--package",
            "sample:patch",
            "--message",
            "Correct a user-visible defect.",
        ])
        .assert()
        .success();

    repo.cli()
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("sample: 0.0.0 -> 0.0.1 (patch)"));

    let output = repo.cli().arg("plan").output().expect("plan command");
    assert!(output.status.success());
    let plan: Value = serde_json::from_slice(&output.stdout).expect("plan JSON");
    assert_eq!(plan["packages"][0]["old_version"], "0.0.0");
    assert_eq!(plan["packages"][0]["new_version"], "0.0.1");
    assert_eq!(plan["publication_order"][0], "sample");
    assert_eq!(plan["global_tag"], "0.0.1");
    assert!(plan["digest"].as_str().unwrap().starts_with("sha256:"));

    repo.cli().arg("apply").assert().success();
    assert!(fs::read_to_string(repo.root.join("package.json"))
        .unwrap()
        .contains("\"version\": \"0.0.1\""));
    assert!(fs::read_dir(repo.root.join(".intentional/intents"))
        .unwrap()
        .next()
        .is_none());
    repo.commit("apply release");

    repo.cli().arg("tag").assert().success();
    let tags = git(&repo.root, &["tag", "--list"]);
    assert!(tags.contains("0.0.1"));
    assert!(tags.contains("sample@0.0.1"));
    repo.cli()
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("Drift: none"));
    repo.cli().arg("check").assert().success();
}

#[test]
fn init_ignores_cargo_workspace_only_manifests() {
    let repo = TestRepo::new();
    repo.write(
        "Cargo.toml",
        "[workspace]\nmembers = [\"crates/library\"]\nresolver = \"2\"\n",
    );
    repo.write(
        "crates/library/Cargo.toml",
        "[package]\nname = \"sample-library\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    repo.commit("add fixture");

    repo.cli().arg("init").assert().success();
    let config = intentional_core::Config::load(&repo.root).expect("generated config");
    assert_eq!(config.packages.len(), 1);
    assert_eq!(
        config.packages["library"].path,
        PathBuf::from("crates/library")
    );
}

#[test]
fn stamp_uses_first_parent_height_and_preserves_intents() {
    let repo = TestRepo::new();
    repo.write("package.json", &npm_manifest("1.0.0"));
    repo.write(".intentional/config.yml", &config("injected"));
    repo.write(".intentional/intents/.keep", "");
    repo.commit("add fixture");
    repo.tag("sample@1.0.0");
    repo.write("first.txt", "first\n");
    repo.commit("first change");
    repo.write(
        ".intentional/intents/clear-river-1234.md",
        &intent("minor", "Add a user-visible capability."),
    );
    repo.commit("add intent");

    let before = fs::read_to_string(repo.root.join("package.json")).unwrap();
    repo.cli()
        .args(["stamp", "--prerelease", "alpha", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("write ./package.json"));
    assert_eq!(
        fs::read_to_string(repo.root.join("package.json")).unwrap(),
        before
    );

    repo.cli()
        .args(["stamp", "--prerelease", "alpha"])
        .assert()
        .success();
    assert!(fs::read_to_string(repo.root.join("package.json"))
        .unwrap()
        .contains("1.1.0-alpha.2"));
    assert!(repo
        .root
        .join(".intentional/intents/clear-river-1234.md")
        .exists());
    assert!(!repo.root.join("CHANGELOG.md").exists());
}

#[test]
fn channel_iteration_comes_only_from_tags_and_final_consolidates() {
    let repo = TestRepo::new();
    repo.write("package.json", &npm_manifest("1.0.0"));
    repo.write(".intentional/config.yml", &config("committed"));
    repo.write(".intentional/intents/.keep", "");
    repo.commit("add fixture");
    repo.tag("sample@1.0.0");
    repo.write(
        ".intentional/intents/quiet-lantern-1234.md",
        &intent("minor", "Add a user-visible capability."),
    );
    repo.commit("add intent");

    let first: Value = serde_json::from_slice(
        &repo
            .cli()
            .args(["plan", "--channel", "beta"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(first["channel"], "beta");
    assert_eq!(first["packages"][0]["new_version"], "1.1.0-beta.1");

    repo.cli()
        .args(["apply", "--channel", "beta"])
        .assert()
        .success();
    assert!(repo
        .root
        .join(".intentional/intents/quiet-lantern-1234.md")
        .exists());
    repo.commit("apply beta");
    repo.cli()
        .args(["tag", "--channel", "beta"])
        .assert()
        .success();
    repo.cli()
        .args(["tag", "--channel", "beta"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "tag sample@1.1.0-beta.1 already exists",
        ));
    assert!(!git(&repo.root, &["tag", "--list"]).contains("beta.2"));

    let second: Value = serde_json::from_slice(
        &repo
            .cli()
            .args(["plan", "--channel", "beta"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    assert_eq!(second["packages"][0]["new_version"], "1.1.0-beta.2");

    repo.cli().arg("apply").assert().success();
    let changelog = fs::read_to_string(repo.root.join("CHANGELOG.md")).unwrap();
    assert!(changelog.contains("## 1.1.0\n"));
    assert!(!changelog.contains("beta.1"));
    assert!(!repo
        .root
        .join(".intentional/intents/quiet-lantern-1234.md")
        .exists());
    repo.commit("apply final");
    repo.cli().arg("tag").assert().success();
    let tags = git(&repo.root, &["tag", "--list"]);
    assert!(tags.contains("sample@1.1.0-beta.1"));
    assert!(tags.contains("sample@1.1.0"));
}

#[test]
fn status_reports_manifest_drift_from_tag_version() {
    let repo = TestRepo::new();
    repo.write("package.json", &npm_manifest("9.9.9"));
    repo.write(".intentional/config.yml", &config("committed"));
    repo.write(".intentional/intents/.keep", "");
    repo.commit("add fixture");
    repo.tag("sample@1.0.0");

    repo.cli()
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("manifest 9.9.9 != tag 1.0.0"));
}

#[test]
fn global_tag_advances_its_own_stream_by_the_highest_bump() {
    let repo = TestRepo::new();
    repo.write("package.json", &npm_manifest("1.0.0"));
    repo.write(
        ".intentional/config.yml",
        &config("committed").replace(
            "packages:",
            "settings:\n  global-tag: true\n  internal-dependency-bump: patch\npackages:",
        ),
    );
    repo.write(".intentional/intents/.keep", "");
    repo.commit("add fixture");
    repo.tag("sample@1.0.0");
    repo.tag("4.0.0");
    repo.write(
        ".intentional/intents/gentle-willow-1234.md",
        &intent("minor", "Add a user-visible capability."),
    );
    repo.commit("add intent");

    let plan: Value = serde_json::from_slice(
        &repo
            .cli()
            .arg("plan")
            .output()
            .expect("plan command")
            .stdout,
    )
    .expect("plan JSON");
    assert_eq!(plan["packages"][0]["new_version"], "1.1.0");
    assert_eq!(plan["global_tag"], "4.1.0");

    repo.cli().arg("apply").assert().success();
    repo.commit("apply release");
    repo.cli().arg("tag").assert().success();
    assert!(git(&repo.root, &["tag", "--list"]).contains("4.1.0"));
}

#[test]
fn dry_runs_print_operations_without_filesystem_or_git_changes() {
    let repo = TestRepo::new();
    repo.write("package.json", &npm_manifest("1.0.0"));
    repo.commit("add fixture");

    repo.cli()
        .args(["init", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("write .intentional/config.yml"));
    assert!(!repo.root.join(".intentional").exists());
    repo.cli().arg("init").assert().success();
    repo.cli()
        .args([
            "add",
            "--package",
            "sample:patch",
            "--message",
            "Correct a user-visible defect.",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("write .intentional/intents/"));
    assert!(fs::read_dir(repo.root.join(".intentional/intents"))
        .unwrap()
        .next()
        .is_none());
    repo.cli()
        .args([
            "add",
            "--package",
            "sample:patch",
            "--message",
            "Correct a user-visible defect.",
        ])
        .assert()
        .success();

    let manifest_before = fs::read_to_string(repo.root.join("package.json")).unwrap();
    let intents_before = fs::read_dir(repo.root.join(".intentional/intents"))
        .unwrap()
        .count();
    let tags_before = git(&repo.root, &["tag", "--list"]);
    repo.cli()
        .args(["apply", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("delete .intentional/intents/"));
    assert_eq!(
        fs::read_to_string(repo.root.join("package.json")).unwrap(),
        manifest_before
    );
    assert_eq!(
        fs::read_dir(repo.root.join(".intentional/intents"))
            .unwrap()
            .count(),
        intents_before
    );
    assert!(!repo.root.join("CHANGELOG.md").exists());
    assert_eq!(git(&repo.root, &["tag", "--list"]), tags_before);

    repo.cli().arg("apply").assert().success();
    repo.commit("apply release");
    repo.cli()
        .args(["tag", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("create tag sample@0.0.1"));
    assert_eq!(git(&repo.root, &["tag", "--list"]), tags_before);
}
