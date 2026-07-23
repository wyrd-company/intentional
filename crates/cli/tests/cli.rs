// ---
// relationships:
//   tests: intent-driven-polyglot-release
// ---

use assert_cmd::Command;
use intentional_core::{
    initialize, Adapter, CandidateResolution, InitPlan, InitState, ProjectionMode,
};
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

fn devcontainer_manifest(id: &str, version: &str) -> String {
    format!(
        "{{\n  \"id\": \"{id}\",\n  \"version\": \"{version}\",\n  \"name\": \"Ignored display value\"\n}}\n"
    )
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
fn skill_prints_the_embedded_agent_workflow_verbatim() {
    let output = Command::new(assert_cmd::cargo::cargo_bin!("intentional"))
        .arg("skill")
        .output()
        .expect("skill command");

    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8 skill output"),
        include_str!("../skills/intentional/SKILL.md")
    );
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
    repo.cli()
        .args(["tag", "--plan", "release-plan.json", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
    repo.cli()
        .args(["tag", "--plan", "release-plan.json"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
    assert_eq!(git(&repo.root, &["tag", "--list"]), tags);
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
fn init_routes_all_six_ecosystems_through_candidates() {
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

    let result = initialize(&repo.root, false).expect("candidate plan");
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

    let result = initialize(&repo.root, false).expect("repeatable configured init");
    assert_eq!(result.state, InitState::Success);
    assert!(result.operations.is_empty());
    assert!(result.plan.is_none());
}

#[test]
fn init_requires_a_git_repository() {
    let temp = tempfile::tempdir().expect("temporary directory");
    let root = temp.path().join("workspace");
    fs::create_dir(&root).expect("workspace directory");
    fs::write(root.join("package.json"), npm_manifest("1.0.0")).expect("fixture manifest");

    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("intentional"));
    command
        .arg("-C")
        .arg(&root)
        .arg("init")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "intentional init requires a Git repository",
        ))
        .stderr(predicate::str::contains("git check-ignore").not())
        .stderr(predicate::str::contains("GIT_DISCOVERY_ACROSS_FILESYSTEM").not());
}

#[test]
fn repeatable_init_reconciles_receipts_and_reopens_changed_exclusions() {
    for resolution in ["excluded", "independent", "projection"] {
        let repo = TestRepo::new();
        repo.write("pnpm-workspace.yaml", "packages:\n  - components/*\n");
        repo.write("components/library/package.json", &npm_manifest("1.0.0"));
        repo.write(
            "components/library/pyproject.toml",
            "[project]\nname = \"sample-example\"\nversion = \"1.0.0\"\n",
        );
        repo.commit("add discovery fixtures");

        repo.cli().arg("init").assert().code(2);
        resolve_plan(&repo, |identity| {
            if identity == "sample-example" {
                CandidateResolution::Excluded
            } else {
                CandidateResolution::Independent {
                    release_unit: identity.to_owned(),
                }
            }
        });
        repo.cli().arg("init").assert().success();

        let no_op = initialize(&repo.root, false).expect("repeatable no-op");
        assert_eq!(no_op.state, InitState::Success);
        assert!(no_op.operations.is_empty());
        let config = intentional_core::Config::load(&repo.root).expect("reconciled config");
        assert_eq!(config.discovery.managed_paths.len(), 1);
        assert_eq!(config.discovery.excluded_paths.len(), 1);

        repo.write(
            "components/library/pyproject.toml",
            "[project]\nname = \"sample-example\"\nversion = \"1.0.1\"\n",
        );
        repo.cli().arg("init").assert().code(2);
        let changed: InitPlan = serde_yaml::from_str(
            &fs::read_to_string(repo.root.join(".intentional/init-plan.yml"))
                .expect("changed exclusion plan"),
        )
        .expect("valid changed exclusion plan");
        assert_eq!(changed.discovery_candidates.len(), 1);
        assert_eq!(
            changed.discovery_candidates[0].native_identity.as_deref(),
            Some("sample-example")
        );
        assert!(changed.discovery_candidates[0].resolution.is_none());

        resolve_plan(&repo, |_| match resolution {
            "excluded" => CandidateResolution::Excluded,
            "independent" => CandidateResolution::Independent {
                release_unit: "sample-example".to_owned(),
            },
            "projection" => CandidateResolution::Projection {
                release_unit: "sample-library".to_owned(),
                target_candidate: None,
            },
            _ => unreachable!(),
        });
        repo.cli().arg("init").assert().success();

        let config = intentional_core::Config::load(&repo.root).expect("re-resolved config");
        let matching_receipts = config
            .discovery
            .managed_paths
            .iter()
            .filter(|receipt| receipt.path == Path::new("components/library/pyproject.toml"))
            .count()
            + config
                .discovery
                .excluded_paths
                .iter()
                .filter(|receipt| receipt.path == Path::new("components/library/pyproject.toml"))
                .count();
        assert_eq!(matching_receipts, 1, "resolution {resolution}");
        match resolution {
            "excluded" => {
                assert_eq!(config.discovery.managed_paths.len(), 1);
                assert_eq!(config.discovery.excluded_paths.len(), 1);
            }
            "independent" => {
                assert_eq!(config.discovery.managed_paths.len(), 2);
                assert!(config.discovery.excluded_paths.is_empty());
                assert!(config.release_units.contains_key("sample-example"));
            }
            "projection" => {
                assert_eq!(config.discovery.managed_paths.len(), 2);
                assert!(config.discovery.excluded_paths.is_empty());
                assert!(config.release_units["sample-library"]
                    .projections
                    .iter()
                    .any(|projection| projection.file == Path::new("pyproject.toml")));
            }
            _ => unreachable!(),
        }
        assert!(!repo.root.join(".intentional/init-plan.yml").exists());
        assert!(initialize(&repo.root, false)
            .expect("repeatable resolved init")
            .operations
            .is_empty());
    }
}

#[test]
fn devcontainer_detectors_extract_only_identity_and_semver_projection_evidence() {
    let repo = TestRepo::new();
    repo.write(
        "devcontainer-feature.json",
        &devcontainer_manifest("sample-feature", "1.2.3-beta.1+build.5"),
    );
    repo.write(
        "devcontainer-template.json",
        &devcontainer_manifest("sample-template", "2.3.4"),
    );
    repo.write("install.sh", "exit 99\n");
    repo.write("devcontainer.json", "not json\n");
    repo.commit("add detector fixtures");

    let first = initialize(&repo.root, false)
        .expect("Dev Container candidate plan")
        .plan
        .expect("unresolved candidates");
    let second = initialize(&repo.root, false)
        .expect("repeatable candidate plan")
        .plan
        .expect("unresolved candidates");
    assert_eq!(first, second);
    assert_eq!(first.discovery_candidates.len(), 2);

    for (detector, identity, version, path) in [
        (
            "devcontainer-feature",
            "sample-feature",
            "1.2.3-beta.1+build.5",
            "devcontainer-feature.json",
        ),
        (
            "devcontainer-template",
            "sample-template",
            "2.3.4",
            "devcontainer-template.json",
        ),
    ] {
        let candidate = first
            .discovery_candidates
            .iter()
            .find(|candidate| candidate.detector == detector)
            .expect("detector candidate");
        assert_eq!(candidate.path, Path::new(path));
        assert_eq!(candidate.native_identity.as_deref(), Some(identity));
        assert_eq!(
            candidate.raw_version.as_ref().map(|raw| raw.value.as_str()),
            Some(version)
        );
        assert_eq!(candidate.evidence.len(), 1);
        assert_eq!(
            candidate.raw_version.as_ref().expect("version").evidence,
            candidate.evidence
        );
        let projection = candidate.projection.as_ref().expect("projection");
        assert_eq!(projection.adapter, Adapter::Json);
        assert_eq!(projection.path, Path::new(path));
        assert_eq!(projection.mode, ProjectionMode::Committed);
        assert_eq!(projection.pointer.as_deref(), Some("/version"));
        assert!(candidate.diagnostics.is_empty());
    }
}

#[test]
fn devcontainer_detectors_report_narrow_extraction_diagnostics() {
    let repo = TestRepo::new();
    repo.write("devcontainer-feature.json", "{ unreadable json\n");
    repo.write(
        "devcontainer-template.json",
        "{\n  \"id\": 42,\n  \"version\": \"release-2\",\n  \"unexpected\": false\n}\n",
    );
    repo.commit("add extraction fixtures");

    let plan = initialize(&repo.root, false)
        .expect("diagnostic candidate plan")
        .plan
        .expect("unresolved candidates");
    let feature = plan
        .discovery_candidates
        .iter()
        .find(|candidate| candidate.detector == "devcontainer-feature")
        .expect("feature candidate");
    assert!(feature.native_identity.is_none());
    assert!(feature.raw_version.is_none());
    assert!(feature.projection.is_none());
    assert!(feature.tag.is_none());
    assert_eq!(
        feature
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>(),
        vec!["devcontainer-json-unreadable"]
    );

    let template = plan
        .discovery_candidates
        .iter()
        .find(|candidate| candidate.detector == "devcontainer-template")
        .expect("template candidate");
    assert!(template.native_identity.is_none());
    assert_eq!(
        template.raw_version.as_ref().map(|raw| raw.value.as_str()),
        Some("release-2")
    );
    assert!(template.projection.is_none());
    assert_eq!(
        template
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>(),
        vec![
            "devcontainer-id-unreadable",
            "devcontainer-version-not-semver"
        ]
    );
    assert!(template.diagnostics.iter().all(|diagnostic| {
        diagnostic.evidence == template.evidence && !diagnostic.message.contains("overall artifact")
    }));
}

#[test]
fn non_semver_devcontainer_candidate_can_be_a_tag_only_independent_unit() {
    let repo = TestRepo::new();
    repo.write(
        "devcontainer-feature.json",
        &devcontainer_manifest("sample-feature", "release-2"),
    );
    repo.commit("add tag-only fixture");

    repo.cli().arg("init").assert().code(2);
    let plan_path = repo.root.join(".intentional/init-plan.yml");
    let plan: InitPlan = serde_yaml::from_str(
        &fs::read_to_string(&plan_path).expect("tag-only initialization plan"),
    )
    .expect("valid tag-only initialization plan");
    let candidate = &plan.discovery_candidates[0];
    assert_eq!(candidate.native_identity.as_deref(), Some("sample-feature"));
    assert_eq!(
        candidate.raw_version.as_ref().map(|raw| raw.value.as_str()),
        Some("release-2")
    );
    assert!(candidate.projection.is_none());
    assert!(candidate.tag.is_some());
    assert!(candidate
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "devcontainer-version-not-semver"));

    resolve_plan(&repo, |_| CandidateResolution::Independent {
        release_unit: "sample-feature".to_owned(),
    });
    repo.cli().arg("init").assert().success();
    let config = intentional_core::Config::load(&repo.root).expect("tag-only config");
    assert!(config.release_units["sample-feature"]
        .projections
        .is_empty());
    assert_eq!(config.release_units["sample-feature"].tags.len(), 1);
    assert_eq!(config.discovery.managed_paths.len(), 1);
}

#[test]
fn successful_candidate_projection_init_has_no_debug_stderr() {
    let repo = TestRepo::new();
    repo.write("package.json", &npm_manifest("1.0.0"));
    repo.write("pubspec.yaml", "name: sample_companion\nversion: 1.0.0\n");
    repo.commit("add output fixtures");

    repo.cli().arg("init").assert().code(2);
    let plan_path = repo.root.join(".intentional/init-plan.yml");
    let mut plan: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).expect("output initialization plan"))
            .expect("valid output initialization plan");
    let creator = plan
        .discovery_candidates
        .iter()
        .find(|candidate| candidate.detector == "npm-package")
        .expect("npm creator")
        .id
        .clone();
    for candidate in &mut plan.discovery_candidates {
        candidate.resolution = Some(if candidate.detector == "npm-package" {
            CandidateResolution::Independent {
                release_unit: "sample-library".to_owned(),
            }
        } else {
            CandidateResolution::Projection {
                release_unit: "sample-library".to_owned(),
                target_candidate: Some(creator.clone()),
            }
        });
    }
    fs::write(&plan_path, plan.to_yaml().expect("resolved output plan"))
        .expect("write output plan");

    repo.cli()
        .arg("init")
        .assert()
        .success()
        .stderr(predicate::str::is_empty());
}

#[test]
fn devcontainer_candidates_support_every_resolution_flow() {
    let independent = TestRepo::new();
    independent.write(
        "devcontainer-feature.json",
        &devcontainer_manifest("sample-independent", "1.0.0"),
    );
    independent.commit("add independent fixture");
    independent.cli().arg("init").assert().code(2);
    resolve_plan(&independent, |_| CandidateResolution::Independent {
        release_unit: "sample-independent".to_owned(),
    });
    independent.cli().arg("init").assert().success();
    let config = intentional_core::Config::load(&independent.root).expect("independent config");
    let projection = &config.release_units["sample-independent"].projections[0];
    assert_eq!(projection.adapter, Adapter::Json);
    assert_eq!(projection.file, Path::new("devcontainer-feature.json"));
    assert_eq!(projection.pointer.as_deref(), Some("/version"));

    let same_plan = TestRepo::new();
    same_plan.write("package.json", &npm_manifest("1.0.0"));
    same_plan.write(
        "devcontainer-feature.json",
        &devcontainer_manifest("sample-feature", "1.0.0"),
    );
    same_plan.commit("add same-plan fixtures");
    same_plan.cli().arg("init").assert().code(2);
    let plan_path = same_plan.root.join(".intentional/init-plan.yml");
    let mut plan: InitPlan = serde_yaml::from_str(
        &fs::read_to_string(&plan_path).expect("same-plan initialization plan"),
    )
    .expect("valid same-plan initialization plan");
    let creator = plan
        .discovery_candidates
        .iter()
        .find(|candidate| candidate.detector == "npm-package")
        .expect("npm creator")
        .id
        .clone();
    for candidate in &mut plan.discovery_candidates {
        candidate.resolution = Some(if candidate.detector == "npm-package" {
            CandidateResolution::Independent {
                release_unit: "sample-library".to_owned(),
            }
        } else {
            CandidateResolution::Projection {
                release_unit: "sample-library".to_owned(),
                target_candidate: Some(creator.clone()),
            }
        });
    }
    fs::write(&plan_path, plan.to_yaml().expect("resolved same-plan plan"))
        .expect("write same-plan plan");
    same_plan.cli().arg("init").assert().success();
    let config = intentional_core::Config::load(&same_plan.root).expect("same-plan config");
    assert_eq!(config.release_units["sample-library"].projections.len(), 2);

    let configured = TestRepo::new();
    configured.write("package.json", &npm_manifest("1.0.0"));
    configured.write(
        "devcontainer-template.json",
        &devcontainer_manifest("sample-template", "1.0.0"),
    );
    configured.write(
        ".intentional/config.yml",
        "$schema: https://intentional.foo/schemas/config.yml\ncontract: contract-1\nsettings:\n  internal-dependency-bump: patch\n  pre-1-0-bump-mapping: compatibility\ndiscovery:\n  managed-paths:\n    - detector: npm-package\n      path: package.json\n      release-unit: sample-library\nrelease-units:\n  sample-library:\n    path: .\n    projections:\n      - adapter: npm\n        file: package.json\n        mode: committed\n    tags:\n      primary:\n        role: primary\n        template: '{id}@{version}'\n",
    );
    configured.commit("add configured projection fixture");
    configured.cli().arg("init").assert().code(2);
    resolve_plan(&configured, |_| CandidateResolution::Projection {
        release_unit: "sample-library".to_owned(),
        target_candidate: None,
    });
    configured.cli().arg("init").assert().success();
    let config = intentional_core::Config::load(&configured.root).expect("configured projection");
    assert!(config.release_units["sample-library"]
        .projections
        .iter()
        .any(|projection| {
            projection.adapter == Adapter::Json
                && projection.file == Path::new("devcontainer-template.json")
                && projection.pointer.as_deref() == Some("/version")
        }));

    let excluded = TestRepo::new();
    excluded.write("package.json", &npm_manifest("1.0.0"));
    excluded.write(
        "devcontainer-template.json",
        &devcontainer_manifest("sample-template", "1.0.0"),
    );
    excluded.commit("add exclusion fixtures");
    excluded.cli().arg("init").assert().code(2);
    resolve_plan(&excluded, |identity| {
        if identity == "sample-template" {
            CandidateResolution::Excluded
        } else {
            CandidateResolution::Independent {
                release_unit: identity.to_owned(),
            }
        }
    });
    excluded.cli().arg("init").assert().success();
    let config = intentional_core::Config::load(&excluded.root).expect("exclusion config");
    assert_eq!(config.discovery.excluded_paths.len(), 1);
    assert_eq!(
        config.discovery.excluded_paths[0].detector,
        "devcontainer-template"
    );
    assert!(!config.release_units.contains_key("sample-template"));
}

#[test]
fn repeatable_init_consumes_a_stale_plan_after_the_candidate_closes() {
    let repo = TestRepo::new();
    repo.write("package.json", &npm_manifest("1.0.0"));
    repo.write(
        "examples/pyproject.toml",
        "[project]\nname = \"sample-example\"\nversion = \"1.0.0\"\n",
    );
    repo.commit("add discovery fixtures");
    repo.cli().arg("init").assert().code(2);
    resolve_plan(&repo, |identity| {
        if identity == "sample-example" {
            CandidateResolution::Excluded
        } else {
            CandidateResolution::Independent {
                release_unit: identity.to_owned(),
            }
        }
    });
    repo.cli().arg("init").assert().success();

    repo.write(
        "examples/pyproject.toml",
        "[project]\nname = \"sample-example\"\nversion = \"1.0.1\"\n",
    );
    repo.cli().arg("init").assert().code(2);
    assert!(repo.root.join(".intentional/init-plan.yml").exists());

    repo.write(
        "examples/pyproject.toml",
        "[project]\nname = \"sample-example\"\nversion = \"1.0.0\"\n",
    );
    repo.cli().arg("init").assert().success();
    assert!(!repo.root.join(".intentional/init-plan.yml").exists());
    assert!(initialize(&repo.root, false)
        .expect("no plan no-op")
        .operations
        .is_empty());
}

#[test]
fn candidate_resolution_preserves_configured_cross_ecosystem_dependencies() {
    let repo = TestRepo::new();
    repo.write("package.json", &npm_manifest("1.0.0"));
    repo.write(
        "components/rust/Cargo.toml",
        "[package]\nname = \"sample-rust\"\nversion = \"1.0.0\"\nedition = \"2021\"\n",
    );
    repo.write(
        "examples/pyproject.toml",
        "[project]\nname = \"sample-example\"\nversion = \"1.0.0\"\n",
    );
    repo.write(
        ".intentional/config.yml",
        "$schema: https://intentional.foo/schemas/config.yml\ncontract: contract-1\nsettings:\n  internal-dependency-bump: patch\n  pre-1-0-bump-mapping: component\ndiscovery:\n  managed-paths:\n    - detector: npm-package\n      path: package.json\n      release-unit: sample-library\n    - detector: cargo-package\n      path: components/rust/Cargo.toml\n      release-unit: sample-rust\nrelease-units:\n  sample-library:\n    path: .\n    projections:\n      - adapter: npm\n        file: package.json\n        mode: committed\n    tags:\n      primary:\n        role: primary\n        template: 'sample-library@{version}'\n    depends-on: [ sample-rust ]\n  sample-rust:\n    path: components/rust\n    projections:\n      - adapter: cargo\n        file: Cargo.toml\n        mode: committed\n    tags:\n      primary:\n        role: primary\n        template: 'sample-rust@{version}'\n",
    );
    repo.commit("add configured dependency fixture");

    repo.cli().arg("init").assert().code(2);
    resolve_plan(&repo, |_| CandidateResolution::Excluded);
    repo.cli().arg("init").assert().success();

    let config = intentional_core::Config::load(&repo.root).expect("resolved config");
    assert_eq!(
        config.release_units["sample-library"].depends_on,
        vec!["sample-rust"]
    );
}

#[test]
fn candidate_resolution_removes_stale_manifest_owned_npm_dependencies() {
    let repo = TestRepo::new();
    repo.write(
        "package.json",
        "{\n  \"name\": \"sample-library\",\n  \"version\": \"1.0.0\",\n  \"dependencies\": { \"sample-peer\": \"^1.0.0\" }\n}\n",
    );
    repo.write(
        "components/peer/package.json",
        "{\n  \"name\": \"sample-peer\",\n  \"version\": \"1.0.0\"\n}\n",
    );
    repo.commit("add native dependency fixtures");

    repo.cli().arg("init").assert().code(2);
    resolve_plan(&repo, |identity| CandidateResolution::Independent {
        release_unit: identity.to_owned(),
    });
    repo.cli().arg("init").assert().success();
    let initial = intentional_core::Config::load(&repo.root).expect("initial config");
    assert_eq!(
        initial.release_units["sample-library"].depends_on,
        vec!["sample-peer"]
    );

    repo.write("package.json", &npm_manifest("1.0.0"));
    repo.write(
        "examples/pyproject.toml",
        "[project]\nname = \"sample-example\"\nversion = \"1.0.0\"\n",
    );

    repo.cli().arg("init").assert().code(2);
    resolve_plan(&repo, |_| CandidateResolution::Excluded);
    repo.cli().arg("init").assert().success();

    let config = intentional_core::Config::load(&repo.root).expect("resolved config");
    assert!(config.release_units["sample-library"].depends_on.is_empty());
}

#[test]
fn discovery_honors_gitignore_and_hard_caches_but_not_broad_directory_names() {
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
        ("tests/fixtures", "sample-fixture"),
        ("vendor", "sample-vendor"),
    ] {
        repo.write(
            &format!("{directory}/package.json"),
            &format!("{{\n  \"name\": \"{name}\",\n  \"version\": \"1.0.0\"\n}}\n"),
        );
    }
    repo.commit("add walking fixtures");

    let plan = initialize(&repo.root, false)
        .expect("repository-wide plan")
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
        "sample-fixture",
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
        .success()
        .stdout(predicate::str::is_empty());
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
