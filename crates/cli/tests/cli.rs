// ---
// relationships:
//   tests: intent-driven-polyglot-release
// ---

use assert_cmd::Command;
use intentional_core::{initialize, CandidateResolution, InitPlan, InitState};
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
        "$schema: https://intentional.foo/schemas/config.yml\ncontract: contract-1\nsettings:\n  internal-dependency-bump: patch\n  pre-1-0-bump-mapping: component\nrelease-units:\n  sample:\n    path: .\n    projections:\n      - adapter: npm\n        file: package.json\n        mode: {mode}\n    tags:\n      primary:\n        role: primary\n        template: 'sample@{{version}}'\n"
    )
}

fn intent(bump: &str, message: &str) -> String {
    format!("---\nsample: {bump}\n---\n\n{message}\n")
}

fn initialize_independent(repo: &TestRepo) {
    repo.cli().arg("init").assert().code(2);
    let path = repo.root.join(".intentional/init-plan.yml");
    let mut plan: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&path).expect("initialization plan"))
            .expect("valid initialization plan");
    for candidate in &mut plan.discovery_candidates {
        candidate.resolution = Some(CandidateResolution::Independent {
            release_unit: candidate
                .native_identity
                .clone()
                .expect("fixture candidate identity"),
        });
    }
    fs::write(&path, plan.to_yaml().expect("resolved initialization plan"))
        .expect("write resolved plan");
    repo.cli().arg("init").assert().success();
}

fn resolve_plan(repo: &TestRepo, resolution: impl Fn(&str) -> CandidateResolution) {
    let path = repo.root.join(".intentional/init-plan.yml");
    let mut plan: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&path).expect("initialization plan"))
            .expect("valid initialization plan");
    for candidate in &mut plan.discovery_candidates {
        candidate.resolution = Some(resolution(
            candidate
                .native_identity
                .as_deref()
                .expect("fixture candidate identity"),
        ));
    }
    fs::write(&path, plan.to_yaml().expect("resolved initialization plan"))
        .expect("write resolved plan");
}

#[test]
fn add_exposes_only_the_release_unit_selector() {
    Command::new(assert_cmd::cargo::cargo_bin!("intentional"))
        .args(["add", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--release-unit"))
        .stdout(predicate::str::contains("--package").not());

    Command::new(assert_cmd::cargo::cargo_bin!("intentional"))
        .args([
            "add",
            "--package",
            "alpha:patch",
            "--message",
            "Describe a change.",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unexpected argument '--package'"));
}

#[test]
fn init_add_status_plan_apply_tag_round_trip() {
    let repo = TestRepo::new();
    repo.write("package.json", &npm_manifest("0.0.0"));
    repo.commit("add fixture");

    initialize_independent(&repo);
    let generated = fs::read_to_string(repo.root.join(".intentional/config.yml")).unwrap();
    repo.write(
        ".intentional/config.yml",
        &generated.replace(
            "release-units:",
            "workspace-tags:\n  release:\n    template: '{version}'\nrelease-units:",
        ),
    );
    repo.cli()
        .args([
            "add",
            "--release-unit",
            "sample-library:patch",
            "--message",
            "Correct a user-visible defect.",
        ])
        .assert()
        .success();
    repo.commit("add release intent");

    repo.cli()
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "sample-library: 0.0.0 -> 0.0.1 (patch)",
        ));

    let output = repo.cli().arg("plan").output().expect("plan command");
    assert!(output.status.success());
    let plan: Value = serde_json::from_slice(&output.stdout).expect("plan JSON");
    assert_eq!(plan["release_units"][0]["old_version"], "0.0.0");
    assert_eq!(plan["release_units"][0]["new_version"], "0.0.1");
    assert!(plan["tag_order"]
        .as_array()
        .unwrap()
        .iter()
        .any(|id| id == "workspace/release"));
    assert!(plan["tags"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tag| tag["name"] == "0.0.1"));
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

    fs::write(repo.root.join("release-plan.json"), &output.stdout).unwrap();
    repo.cli()
        .args(["tag", "--plan", "release-plan.json"])
        .assert()
        .success();
    let tags = git(&repo.root, &["tag", "--list"]);
    assert!(tags.contains("0.0.1"));
    assert!(tags.contains("sample-library@0.0.1"));
    assert_eq!(git(&repo.root, &["cat-file", "-t", "0.0.1"]), "tag");
    let record = git(&repo.root, &["cat-file", "-p", "sample-library@0.0.1"]);
    assert!(record.contains(&format!(
        "plan-digest: {}",
        plan["digest"].as_str().unwrap()
    )));
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

    initialize_independent(&repo);
    let config = intentional_core::Config::load(&repo.root).expect("generated config");
    assert_eq!(config.release_units.len(), 1);
    assert_eq!(
        config.release_units["sample-library"].path,
        PathBuf::from("crates/library")
    );
}

#[test]
fn scan_all_routes_all_six_ecosystems_through_candidates() {
    let repo = TestRepo::new();
    repo.write("package.json", &npm_manifest("1.0.0"));
    repo.write(
        "components/rust/Cargo.toml",
        "[package]\nname = \"sample-rust\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
    );
    repo.write(
        "components/go/go.mod",
        "module example.invalid/sample-go\n\ngo 1.22\n",
    );
    repo.write(
        "components/python/pyproject.toml",
        "[project]\nname = \"sample-python\"\nversion = \"1.0.dev1\"\n",
    );
    repo.write(
        "components/dotnet/Sample.csproj",
        "<Project><PropertyGroup><PackageId>Sample.DotNet</PackageId><Version>1.0.0</Version></PropertyGroup></Project>\n",
    );
    repo.write(
        "components/dart/pubspec.yaml",
        "name: sample_dart\nversion: 1.0.0\n",
    );
    repo.commit("add ecosystem fixtures");

    let result = initialize(&repo.root, true, false).expect("candidate plan");
    assert_eq!(result.state, InitState::NeedsInput);
    let plan = result.plan.expect("initialization plan");
    let detectors = plan
        .discovery_candidates
        .iter()
        .map(|candidate| candidate.detector.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        detectors,
        std::collections::BTreeSet::from([
            "cargo-package",
            "dart-package",
            "go-module",
            "msbuild-project",
            "npm-package",
            "python-project",
        ])
    );
    assert!(plan
        .discovery_candidates
        .iter()
        .all(|candidate| candidate.resolution.is_none()));
    assert_eq!(
        plan.discovery_candidates
            .iter()
            .find(|candidate| candidate.detector == "python-project")
            .and_then(|candidate| candidate.raw_version.as_ref())
            .map(|version| version.value.as_str()),
        Some("1.0.dev1")
    );
    assert!(!repo.root.join(".intentional/config.yml").exists());
}

#[test]
fn configured_repository_without_detectable_manifests_is_a_no_op() {
    let repo = TestRepo::new();
    repo.write(".intentional/config.yml", &config("committed"));
    repo.commit("add configured fixture");

    let result = initialize(&repo.root, false, false).expect("repeatable configured init");
    assert_eq!(result.state, InitState::Success);
    assert!(result.operations.is_empty());
    assert!(result.plan.is_none());
}

#[test]
fn repeatable_init_reconciles_receipts_and_reopens_changed_exclusions() {
    let repo = TestRepo::new();
    repo.write("package.json", &npm_manifest("1.0.0"));
    repo.write(
        "examples/pyproject.toml",
        "[project]\nname = \"sample-example\"\nversion = \"1.0.0\"\n",
    );
    repo.commit("add discovery fixtures");

    repo.cli().args(["init", "--scan-all"]).assert().code(2);
    resolve_plan(&repo, |identity| {
        if identity == "sample-example" {
            CandidateResolution::Excluded
        } else {
            CandidateResolution::Independent {
                release_unit: identity.to_owned(),
            }
        }
    });
    repo.cli().args(["init", "--scan-all"]).assert().success();

    let no_op = initialize(&repo.root, true, false).expect("repeatable no-op");
    assert_eq!(no_op.state, InitState::Success);
    assert!(no_op.operations.is_empty());
    let config = intentional_core::Config::load(&repo.root).expect("reconciled config");
    assert_eq!(config.discovery.managed_paths.len(), 1);
    assert_eq!(config.discovery.excluded_paths.len(), 1);

    repo.write(
        "examples/pyproject.toml",
        "[project]\nname = \"sample-example\"\nversion = \"1.0.1\"\n",
    );
    let changed = initialize(&repo.root, true, false).expect("changed exclusion plan");
    assert_eq!(changed.state, InitState::NeedsInput);
    let candidates = changed.plan.expect("changed plan").discovery_candidates;
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].native_identity.as_deref(),
        Some("sample-example")
    );
    assert!(candidates[0].resolution.is_none());
}

#[test]
fn scan_all_honors_gitignore_and_hard_caches_but_not_broad_directory_names() {
    let repo = TestRepo::new();
    repo.write(
        "package.json",
        "{\n  \"name\": \"sample-library\",\n  \"version\": \"1.0.0\",\n  \"workspaces\": [\"packages/*\", \"node_modules/dependency\"]\n}\n",
    );
    repo.write(".gitignore", "ignored/\n");
    repo.write(
        "packages/visible/package.json",
        "{\n  \"name\": \"sample-visible\",\n  \"version\": \"1.0.0\"\n}\n",
    );
    for (directory, name) in [
        ("ignored", "ignored-package"),
        ("node_modules/dependency", "cached-node"),
        ("target/generated", "cached-rust"),
        (".venv/lib", "cached-python"),
        ("obj/generated", "cached-dotnet"),
        ("build", "sample-build"),
        ("dist", "sample-dist"),
        ("bin", "sample-bin"),
        ("vendor", "sample-vendor"),
    ] {
        repo.write(
            &format!("{directory}/package.json"),
            &format!("{{\n  \"name\": \"{name}\",\n  \"version\": \"1.0.0\"\n}}\n"),
        );
    }
    repo.commit("add walking fixtures");

    let workspace_names = initialize(&repo.root, false, false)
        .expect("workspace plan")
        .plan
        .expect("candidate plan")
        .discovery_candidates
        .into_iter()
        .filter_map(|candidate| candidate.native_identity)
        .collect::<std::collections::BTreeSet<_>>();
    assert!(workspace_names.contains("sample-visible"));
    assert!(!workspace_names.contains("cached-node"));

    let plan = initialize(&repo.root, true, false)
        .expect("scan-all plan")
        .plan
        .expect("candidate plan");
    let names = plan
        .discovery_candidates
        .iter()
        .filter_map(|candidate| candidate.native_identity.as_deref())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(names.is_superset(&std::collections::BTreeSet::from([
        "sample-library",
        "sample-build",
        "sample-dist",
        "sample-bin",
        "sample-vendor",
    ])));
    for excluded in [
        "ignored-package",
        "cached-node",
        "cached-rust",
        "cached-python",
        "cached-dotnet",
    ] {
        assert!(!names.contains(excluded), "unexpected candidate {excluded}");
    }
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
    assert_eq!(first["release_units"][0]["new_version"], "1.1.0-beta.1");

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
    assert_eq!(second["release_units"][0]["new_version"], "1.1.0-beta.2");

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
fn workspace_tag_advances_its_own_stream_by_the_highest_bump() {
    let repo = TestRepo::new();
    repo.write("package.json", &npm_manifest("0.1.0"));
    repo.write(
        ".intentional/config.yml",
        &config("committed")
            .replace(
                "pre-1-0-bump-mapping: component",
                "pre-1-0-bump-mapping: compatibility",
            )
            .replace(
                "release-units:",
                "workspace-tags:\n  release:\n    template: '{version}'\nrelease-units:",
            ),
    );
    repo.write(".intentional/intents/.keep", "");
    repo.commit("add fixture");
    repo.tag("sample@0.1.0");
    repo.tag("4.0.0");
    repo.write(
        ".intentional/intents/gentle-willow-1234.md",
        &intent("major", "Change a public contract."),
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
    assert_eq!(plan["release_units"][0]["new_version"], "0.2.0");
    assert!(plan["tags"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tag| tag["name"] == "5.0.0"));

    repo.cli().arg("apply").assert().success();
    repo.commit("apply release");
    repo.cli().arg("tag").assert().success();
    assert!(git(&repo.root, &["tag", "--list"]).contains("5.0.0"));
}

#[test]
fn dry_runs_print_operations_without_filesystem_or_git_changes() {
    let repo = TestRepo::new();
    repo.write("package.json", &npm_manifest("1.0.0"));
    repo.commit("add fixture");

    repo.cli()
        .args(["init", "--dry-run"])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("write .intentional/init-plan.yml"));
    assert!(!repo.root.join(".intentional").exists());
    initialize_independent(&repo);
    repo.cli()
        .args([
            "add",
            "--release-unit",
            "sample-library:patch",
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
            "--release-unit",
            "sample-library:patch",
            "--message",
            "Correct a user-visible defect.",
        ])
        .assert()
        .success();
    repo.commit("add release intent");

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
        .stdout(predicate::str::contains(
            "create annotated tag sample-library@0.0.1",
        ));
    assert_eq!(git(&repo.root, &["tag", "--list"]), tags_before);
}
