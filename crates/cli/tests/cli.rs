// ---
// relationships:
//   tests: intent-driven-polyglot-release
// ---

use assert_cmd::Command;
use intentional_core::{
    canonical_json, initialize, Adapter, CandidateResolution, Generator, InitPlan, InitState,
    PlanReleaseUnit, PlanTag, ProjectionMode, ReleasePlan,
};
use predicates::prelude::*;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
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

    /// A path beside the repository rather than inside it.
    ///
    /// Evidence assembly refuses a working tree that is not the tree the
    /// release published, and anything written under `root` after the release
    /// is a file the release commit does not carry. The derived workflow puts
    /// these under the runner's temp directory for the same reason.
    fn outside(&self, relative: &str) -> PathBuf {
        let path = self._temp.path().join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("scratch parent");
        }
        path
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

    fn cli_with_env(&self, env: &[(&str, &str)]) -> Command {
        let mut command = self.cli();
        for (key, value) in env {
            command.env(key, value);
        }
        command
    }
}

fn git(root: &Path, args: &[&str]) -> String {
    git_raw(root, args).trim().to_owned()
}

fn git_raw(root: &Path, args: &[&str]) -> String {
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
        .to_owned()
}

fn npm_manifest(version: &str) -> String {
    format!("{{\n  \"name\": \"sample-library\",\n  \"version\": \"{version}\"\n}}\n")
}

fn config(mode: &str) -> String {
    format!(
        "$schema: https://intentional.foo/schemas/config.yml\ncontract: contract-2\nsettings:\n  internal-dependency-bump: patch\n  pre-1-0-bump-mapping: component\nrelease-units:\n  sample:\n    path: .\n    projections:\n      - adapter: npm\n        file: package.json\n        mode: {mode}\n    tags:\n      primary:\n        role: primary\n        template: 'sample@{{version}}'\n"
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
            package: candidate
                .native_identity
                .clone()
                .expect("fixture package identity"),
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
                    package: identity.to_owned(),
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
                package: "sample-example".to_owned(),
            },
            "projection" => CandidateResolution::Projection {
                release_unit: "sample-library".to_owned(),
                package: "sample-example".to_owned(),
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
        package: "sample-feature".to_owned(),
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
                package: "sample-library".to_owned(),
            }
        } else {
            CandidateResolution::Projection {
                release_unit: "sample-library".to_owned(),
                package: "sample-companion".to_owned(),
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
        package: "sample-independent".to_owned(),
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
                package: "sample-library".to_owned(),
            }
        } else {
            CandidateResolution::Projection {
                release_unit: "sample-library".to_owned(),
                package: "sample-feature".to_owned(),
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
        "$schema: https://intentional.foo/schemas/config.yml\ncontract: contract-2\nsettings:\n  internal-dependency-bump: patch\n  pre-1-0-bump-mapping: compatibility\ndiscovery:\n  managed-paths:\n    - detector: npm-package\n      path: package.json\n      release-unit: sample-library\n      package: sample-library\nrelease-units:\n  sample-library:\n    path: .\n    packages:\n      sample-library: { path: . }\n    projections:\n      - adapter: npm\n        file: package.json\n        mode: committed\n    tags:\n      primary:\n        role: primary\n        template: '{id}@{version}'\n",
    );
    configured.commit("add configured projection fixture");
    configured.cli().arg("init").assert().code(2);
    resolve_plan(&configured, |_| CandidateResolution::Projection {
        release_unit: "sample-library".to_owned(),
        package: "sample-template".to_owned(),
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
                package: identity.to_owned(),
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

fn write_tag_only_fixtures(repo: &TestRepo) {
    repo.write("action.yml", "name: Root\nruns:\n  using: composite\n");
    repo.write(
        "actions/publish/action.yml",
        "name: Publish\nruns:\n  using: composite\n",
    );
    repo.write(
        "actions/verify/action.yaml",
        "name: Verify\nruns:\n  using: composite\n",
    );
    repo.write("Dockerfile", "FROM scratch\n");
    repo.write("images/Dockerfile.runtime", "FROM scratch\n");
    repo.write("images/toolchain.Dockerfile", "FROM scratch\n");
    repo.write(
        "modules/network/main.tf",
        "resource \"null_resource\" \"sample\" {}\n",
    );
    repo.write("modules/network/variables.tf", "variable \"sample\" {}\n");
    repo.write(
        "modules/storage/outputs.tf",
        "output \"sample\" {\n  value = 1\n}\n",
    );
}

#[test]
fn tag_only_detectors_derive_identity_from_path_evidence_alone() {
    let repo = TestRepo::new();
    write_tag_only_fixtures(&repo);
    repo.write(".github/workflows/release.yml", "name: release\n");
    repo.write("compose.yaml", "services: {}\n");
    repo.write(
        ".terraform/modules/vendored/main.tf",
        "resource \"null_resource\" \"vendored\" {}\n",
    );
    repo.commit("add tag-only fixtures");

    let first = initialize(&repo.root, false)
        .expect("tag-only candidate plan")
        .plan
        .expect("unresolved candidates");
    let second = initialize(&repo.root, false)
        .expect("repeatable candidate plan")
        .plan
        .expect("unresolved candidates");
    assert_eq!(first, second);

    let observed = first
        .discovery_candidates
        .iter()
        .map(|candidate| {
            (
                candidate.detector.as_str(),
                candidate.path.to_string_lossy().into_owned(),
                candidate.native_identity.clone(),
                candidate.evidence.len(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        observed,
        [
            ("docker-image", "Dockerfile".to_owned(), None, 1),
            (
                "docker-image",
                "images/Dockerfile.runtime".to_owned(),
                Some("runtime".to_owned()),
                1
            ),
            (
                "docker-image",
                "images/toolchain.Dockerfile".to_owned(),
                Some("toolchain".to_owned()),
                1
            ),
            ("github-action", "action.yml".to_owned(), None, 1),
            (
                "github-action",
                "actions/publish/action.yml".to_owned(),
                Some("publish".to_owned()),
                1
            ),
            (
                "github-action",
                "actions/verify/action.yaml".to_owned(),
                Some("verify".to_owned()),
                1
            ),
            (
                "terraform-module",
                "modules/network".to_owned(),
                Some("network".to_owned()),
                3
            ),
            (
                "terraform-module",
                "modules/storage".to_owned(),
                Some("storage".to_owned()),
                2
            ),
        ]
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
    );

    for candidate in &first.discovery_candidates {
        assert!(candidate.projection.is_none(), "{} projects", candidate.id);
        assert!(candidate.raw_version.is_none(), "{} versions", candidate.id);
        let tag = candidate.tag.as_ref().expect("tag-only tag suggestion");
        assert_eq!(tag.template, "{id}@{version}");
        let expected = if candidate.native_identity.is_some() {
            Vec::new()
        } else {
            vec![format!(
                "{}-identity-not-path-derivable",
                candidate.detector
            )]
        };
        assert_eq!(
            candidate
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.clone())
                .collect::<Vec<_>>(),
            expected
        );
    }

    let module = first
        .discovery_candidates
        .iter()
        .find(|candidate| candidate.path == Path::new("modules/network"))
        .expect("Terraform module candidate");
    assert_eq!(
        module
            .evidence
            .iter()
            .map(|item| item.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        vec![
            "modules/network".to_owned(),
            "modules/network/main.tf".to_owned(),
            "modules/network/variables.tf".to_owned()
        ]
    );
}

#[test]
fn terraform_plugin_go_modules_present_as_providers() {
    let repo = TestRepo::new();
    repo.write(
        "go.mod",
        "module github.com/example/terraform-provider-sample\n\ngo 1.22\n\nrequire (\n\tgithub.com/hashicorp/terraform-plugin-framework v1.8.0\n)\n",
    );
    repo.write(
        "tools/go.mod",
        "module github.com/example/sample-tools\n\ngo 1.22\n\nrequire (\n\tgithub.com/hashicorp/terraform-plugin-sdk/v2 v2.33.0 // indirect\n)\n",
    );
    repo.write(
        "replaced/go.mod",
        "module github.com/example/plain-tool\n\ngo 1.22\n\nreplace (\n\tgithub.com/hashicorp/terraform-plugin-sdk/v2 => ./vendored/sdk\n)\n",
    );
    repo.write(
        "excluded/go.mod",
        "module github.com/example/other-tool\n\ngo 1.22\n\nexclude github.com/hashicorp/terraform-plugin-framework v1.8.0\n",
    );
    repo.write(
        "single/go.mod",
        "module github.com/example/terraform-provider-single\n\ngo 1.22\n\nrequire github.com/hashicorp/terraform-plugin-framework v1.8.0\n",
    );
    repo.commit("add Go module fixtures");

    let plan = initialize(&repo.root, false)
        .expect("provider candidate plan")
        .plan
        .expect("unresolved candidates");
    let provider = plan
        .discovery_candidates
        .iter()
        .find(|candidate| candidate.path == Path::new("go.mod"))
        .expect("provider candidate");
    assert_eq!(provider.detector, "terraform-provider");
    assert_eq!(
        provider.native_identity.as_deref(),
        Some("github.com/example/terraform-provider-sample")
    );
    assert!(provider.raw_version.is_none());
    // A provider is a Go module, so it keeps the Go projection at mode none:
    // no version is written, but a major bump can rewrite the path suffix.
    let projection = provider.projection.as_ref().expect("Go projection");
    assert_eq!(projection.adapter, Adapter::Go);
    assert_eq!(projection.path, Path::new("go.mod"));
    assert_eq!(projection.mode, ProjectionMode::None);

    let single = plan
        .discovery_candidates
        .iter()
        .find(|candidate| candidate.path == Path::new("single/go.mod"))
        .expect("single-line require candidate");
    assert_eq!(single.detector, "terraform-provider");

    // Indirect requirements and non-require directives name a plugin module
    // without depending on it.
    for path in ["tools/go.mod", "replaced/go.mod", "excluded/go.mod"] {
        let module = plan
            .discovery_candidates
            .iter()
            .find(|candidate| candidate.path == Path::new(path))
            .unwrap_or_else(|| panic!("plain Go module candidate for {path}"));
        assert_eq!(module.detector, "go-module", "{path}");
    }
}

#[test]
fn go_commands_are_package_candidates_with_clone_durable_receipts() {
    let repo = TestRepo::new();
    repo.write("go.mod", "module example.invalid/sample-tool\n\ngo 1.22\n");
    repo.write("cmd/alpha/main.go", "package main\n\nfunc main() {}\n");
    repo.write("apps/delta/main.go", "package main\n\nfunc main() {}\n");
    repo.write("cmd/a/b/c/d/main.go", "package main\n\nfunc main() {}\n");
    repo.write(
        "nested/go.mod",
        "module example.invalid/nested-tool\n\ngo 1.22\n",
    );
    repo.write(
        "nested/apps/epsilon/main.go",
        "package main\n\nfunc main() {}\n",
    );
    repo.write(
        "cmd/beta/main.go",
        "//go:build !ignored\n\npackage main // import \"example.invalid/sample-tool/cmd/beta\"\n\nfunc main() {}\n",
    );
    repo.write("cmd/beta/helper.go", "package main\n\nconst sample = 1\n");
    repo.write("tools/gamma/main.go", "package main\n\nfunc main() {}\n");
    repo.write("cmd/ignored/main.go", "package main\n\nfunc main() {}\n");
    repo.write("cmd/.cache/main.go", "package main\n\nfunc main() {}\n");
    repo.write(
        ".goreleaser.yaml",
        "version: 2\nbuilds:\n  - main: ./tools/gamma\n",
    );
    repo.write(".gitignore", "cmd/ignored/\n");
    repo.commit("add Go command fixtures");

    repo.cli().arg("init").assert().code(2);
    let plan_path = repo.root.join(".intentional/init-plan.yml");
    let mut plan: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).expect("Go initialization plan"))
            .expect("valid Go initialization plan");
    assert_eq!(
        plan.discovery_candidates
            .iter()
            .map(|candidate| {
                (
                    candidate.detector.as_str(),
                    candidate.path.to_string_lossy().into_owned(),
                )
            })
            .collect::<std::collections::BTreeSet<_>>(),
        [
            ("go-command", "apps/delta".to_owned()),
            ("go-command", "cmd/a/b/c/d".to_owned()),
            ("go-command", "cmd/alpha".to_owned()),
            ("go-command", "cmd/beta".to_owned()),
            ("go-command", "nested/apps/epsilon".to_owned()),
            ("go-command", "tools/gamma".to_owned()),
            ("go-module", "go.mod".to_owned()),
            ("go-module", "nested/go.mod".to_owned()),
        ]
        .into_iter()
        .collect(),
    );
    let module = plan
        .discovery_candidates
        .iter()
        .find(|candidate| candidate.detector == "go-module")
        .expect("module candidate")
        .id
        .clone();
    for candidate in &mut plan.discovery_candidates {
        candidate.resolution = Some(match candidate.path.to_string_lossy().as_ref() {
            "go.mod" => CandidateResolution::Independent {
                release_unit: "sample-tool".to_owned(),
                package: "module".to_owned(),
            },
            "cmd/alpha" => CandidateResolution::Projection {
                release_unit: "sample-tool".to_owned(),
                package: "alpha".to_owned(),
                target_candidate: Some(module.clone()),
            },
            "apps/delta" | "cmd/a/b/c/d" => CandidateResolution::Excluded,
            "nested/go.mod" | "nested/apps/epsilon" => CandidateResolution::Excluded,
            "cmd/beta" => CandidateResolution::Excluded,
            "tools/gamma" => CandidateResolution::Excluded,
            path => panic!("unexpected Go candidate {path}"),
        });
    }
    fs::write(&plan_path, plan.to_yaml().expect("resolved Go plan"))
        .expect("write resolved Go plan");
    repo.cli().arg("init").assert().success();

    let config = intentional_core::Config::load(&repo.root).expect("Go config");
    assert_eq!(
        config.release_units["sample-tool"]
            .packages
            .iter()
            .map(|(id, package)| (id.as_str(), package.path.as_path()))
            .collect::<Vec<_>>(),
        vec![
            ("alpha", Path::new("cmd/alpha")),
            ("module", Path::new("."))
        ]
    );
    let alpha = config
        .discovery
        .managed_paths
        .iter()
        .find(|receipt| receipt.path == Path::new("cmd/alpha"))
        .expect("alpha managed receipt");
    assert_eq!(alpha.package, "alpha");
    assert_eq!(
        config
            .discovery
            .excluded_paths
            .iter()
            .map(|receipt| receipt.path.as_path())
            .collect::<std::collections::BTreeSet<_>>(),
        [
            Path::new("apps/delta"),
            Path::new("cmd/a/b/c/d"),
            Path::new("cmd/beta"),
            Path::new("nested/apps/epsilon"),
            Path::new("nested/go.mod"),
            Path::new("tools/gamma"),
        ]
        .into_iter()
        .collect()
    );

    repo.commit("record Go discovery decisions");
    let clone_root = repo.outside("fresh-clone");
    git(
        repo._temp.path(),
        &[
            "clone",
            "-q",
            repo.root.to_str().expect("repository path"),
            clone_root.to_str().expect("clone path"),
        ],
    );
    assert!(!clone_root.join(".intentional/init-plan.yml").exists());
    assert!(initialize(&clone_root, false)
        .expect("fresh clone discovery")
        .operations
        .is_empty());

    fs::write(
        clone_root.join("cmd/beta/main.go"),
        "package main\n\nfunc main() { println(\"changed\") }\n",
    )
    .expect("change excluded command");
    assert_eq!(
        initialize(&clone_root, false)
            .expect("changed command discovery")
            .plan
            .expect("changed command plan")
            .discovery_candidates
            .iter()
            .map(|candidate| candidate.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        vec!["cmd/beta".to_owned()]
    );
}

#[test]
fn tag_only_candidates_support_every_resolution_flow() {
    let repo = TestRepo::new();
    repo.write("package.json", &npm_manifest("1.0.0"));
    write_tag_only_fixtures(&repo);
    repo.commit("add tag-only resolution fixtures");

    repo.cli().arg("init").assert().code(2);
    let plan_path = repo.root.join(".intentional/init-plan.yml");
    let mut plan: InitPlan = serde_yaml::from_str(
        &fs::read_to_string(&plan_path).expect("tag-only initialization plan"),
    )
    .expect("valid tag-only initialization plan");
    let creator = plan
        .discovery_candidates
        .iter()
        .find(|candidate| candidate.detector == "npm-package")
        .expect("npm creator")
        .id
        .clone();
    for candidate in &mut plan.discovery_candidates {
        candidate.resolution = Some(match candidate.native_identity.as_deref() {
            _ if candidate.detector == "npm-package" => CandidateResolution::Independent {
                release_unit: "sample-library".to_owned(),
                package: "sample-library".to_owned(),
            },
            Some("publish" | "runtime" | "network") => CandidateResolution::Independent {
                release_unit: candidate
                    .native_identity
                    .clone()
                    .expect("path-derived identity"),
                package: candidate
                    .native_identity
                    .clone()
                    .expect("path-derived package identity"),
            },
            Some("verify" | "toolchain") => CandidateResolution::Projection {
                release_unit: "sample-library".to_owned(),
                package: candidate
                    .native_identity
                    .clone()
                    .expect("path-derived package identity"),
                target_candidate: Some(creator.clone()),
            },
            _ => CandidateResolution::Excluded,
        });
    }
    fs::write(&plan_path, plan.to_yaml().expect("resolved tag-only plan"))
        .expect("write tag-only plan");
    repo.cli().arg("init").assert().success();

    let config = intentional_core::Config::load(&repo.root).expect("tag-only config");
    for (id, path) in [
        ("publish", "actions/publish"),
        ("runtime", "images"),
        ("network", "modules/network"),
    ] {
        let unit = &config.release_units[id];
        assert!(unit.projections.is_empty(), "{id} carries a projection");
        assert_eq!(unit.path, Path::new(path));
        assert_eq!(unit.tags["primary"].template, "{id}@{version}");
    }
    assert_eq!(config.release_units["sample-library"].projections.len(), 1);

    let managed = config
        .discovery
        .managed_paths
        .iter()
        .map(|receipt| {
            (
                receipt.detector.as_str(),
                receipt.path.to_string_lossy().into_owned(),
                receipt.release_unit.as_str(),
            )
        })
        .collect::<Vec<_>>();
    assert!(managed.contains(&(
        "github-action",
        "actions/verify/action.yaml".to_owned(),
        "sample-library"
    )));
    assert!(managed.contains(&("terraform-module", "modules/network".to_owned(), "network")));
    let excluded = config
        .discovery
        .excluded_paths
        .iter()
        .map(|receipt| receipt.path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        excluded,
        vec![
            "Dockerfile".to_owned(),
            "action.yml".to_owned(),
            "modules/storage".to_owned()
        ]
    );

    assert!(initialize(&repo.root, false)
        .expect("receipt no-op")
        .operations
        .is_empty());

    repo.write("Dockerfile", "FROM scratch\nLABEL sample=1\n");
    repo.cli().arg("init").assert().code(2);
    let reopened: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).expect("reopened plan"))
            .expect("valid reopened plan");
    assert_eq!(
        reopened
            .discovery_candidates
            .iter()
            .map(|candidate| candidate.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        vec!["Dockerfile".to_owned()]
    );
}

/// Report the unresolved candidate paths a rescan produces, or nothing when it is a no-op.
fn rescan_candidate_paths(repo: &TestRepo) -> Vec<String> {
    let result = initialize(&repo.root, false).expect("rescan");
    result
        .plan
        .map(|plan| {
            plan.discovery_candidates
                .iter()
                .map(|candidate| candidate.path.to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn terraform_module_receipts_key_on_the_directory_not_its_contents() {
    let repo = TestRepo::new();
    repo.write(
        "modules/network/main.tf",
        "resource \"null_resource\" \"sample\" {}\n",
    );
    repo.write("modules/network/variables.tf", "variable \"sample\" {}\n");
    repo.write(
        "modules/storage/outputs.tf",
        "output \"sample\" {\n  value = 1\n}\n",
    );
    repo.commit("add Terraform module fixtures");

    repo.cli().arg("init").assert().code(2);
    resolve_plan(&repo, |identity| {
        if identity == "storage" {
            CandidateResolution::Excluded
        } else {
            CandidateResolution::Independent {
                release_unit: identity.to_owned(),
                package: identity.to_owned(),
            }
        }
    });
    repo.cli().arg("init").assert().success();
    assert!(rescan_candidate_paths(&repo).is_empty());

    // A managed module that gains a source file stays managed.
    repo.write(
        "modules/network/outputs.tf",
        "output \"added\" {\n  value = 1\n}\n",
    );
    assert!(rescan_candidate_paths(&repo).is_empty());

    // A managed module that loses main.tf stays the same single candidate.
    fs::remove_file(repo.root.join("modules/network/main.tf")).expect("remove anchor");
    assert!(rescan_candidate_paths(&repo).is_empty());
    let config = intentional_core::Config::load(&repo.root).expect("config after anchor removal");
    assert_eq!(
        config
            .discovery
            .managed_paths
            .iter()
            .filter(|receipt| receipt.detector == "terraform-module")
            .map(|receipt| receipt.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        vec!["modules/network".to_owned()]
    );

    // An excluded module reopens under its own path when any source changes,
    // including a source that would previously have moved the anchor.
    repo.write(
        "modules/storage/main.tf",
        "resource \"null_resource\" \"storage\" {}\n",
    );
    assert_eq!(
        rescan_candidate_paths(&repo),
        vec!["modules/storage".to_owned()]
    );

    repo.cli().arg("init").assert().code(2);
    resolve_plan(&repo, |_| CandidateResolution::Excluded);
    repo.cli().arg("init").assert().success();
    let config = intentional_core::Config::load(&repo.root).expect("config after re-exclusion");
    assert_eq!(
        config
            .discovery
            .excluded_paths
            .iter()
            .map(|receipt| receipt.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        vec!["modules/storage".to_owned()]
    );

    // Editing a non-anchor source of an excluded module also reopens it.
    repo.write(
        "modules/storage/outputs.tf",
        "output \"sample\" {\n  value = 2\n}\n",
    );
    assert_eq!(
        rescan_candidate_paths(&repo),
        vec!["modules/storage".to_owned()]
    );
}

#[test]
fn a_root_terraform_module_is_one_workspace_root_candidate() {
    let repo = TestRepo::new();
    repo.write("main.tf", "resource \"null_resource\" \"root\" {}\n");
    repo.write("variables.tf", "variable \"root\" {}\n");
    repo.commit("add root Terraform module");

    let plan = initialize(&repo.root, false)
        .expect("root module plan")
        .plan
        .expect("unresolved candidates");
    assert_eq!(plan.discovery_candidates.len(), 1);
    let candidate = &plan.discovery_candidates[0];
    assert_eq!(candidate.detector, "terraform-module");
    assert_eq!(candidate.path, Path::new("."));
    assert!(candidate.native_identity.is_none());
    assert_eq!(
        candidate
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>(),
        vec!["terraform-module-identity-not-path-derivable"]
    );

    let plan_path = repo.root.join(".intentional/init-plan.yml");
    let mut plan = plan;
    plan.discovery_candidates[0].resolution = Some(CandidateResolution::Independent {
        release_unit: "root-network".to_owned(),
        package: "root-network".to_owned(),
    });
    fs::create_dir_all(plan_path.parent().expect("plan directory")).expect("plan directory");
    fs::write(&plan_path, plan.to_yaml().expect("resolved root plan")).expect("write root plan");
    repo.cli().arg("init").assert().success();
    let config = intentional_core::Config::load(&repo.root).expect("root module config");
    assert_eq!(config.release_units["root-network"].path, Path::new("."));
    assert_eq!(
        config.discovery.managed_paths[0]
            .path
            .to_string_lossy()
            .into_owned(),
        "."
    );
}

#[test]
fn docker_and_identity_suggestions_reject_unusable_path_evidence() {
    let repo = TestRepo::new();
    repo.write(
        "docs/Dockerfile.md",
        "How to build the image.
",
    );
    repo.write(
        "docs/Dockerfile.example",
        "FROM scratch
",
    );
    repo.write(
        "docs/Dockerfile.bak",
        "FROM scratch
",
    );
    repo.write(
        ".config/Dockerfile",
        "FROM scratch
",
    );
    repo.write(
        "-leading/Dockerfile",
        "FROM scratch
",
    );
    repo.write(
        "images/Dockerfile.runtime",
        "FROM scratch
",
    );
    repo.commit("add Docker naming fixtures");

    let plan = initialize(&repo.root, false)
        .expect("Docker naming plan")
        .plan
        .expect("unresolved candidates");
    let mut observed = plan
        .discovery_candidates
        .iter()
        .map(|candidate| {
            (
                candidate.path.to_string_lossy().into_owned(),
                candidate.native_identity.clone(),
            )
        })
        .collect::<Vec<_>>();
    observed.sort();
    assert_eq!(
        observed,
        vec![
            ("-leading/Dockerfile".to_owned(), None),
            (".config/Dockerfile".to_owned(), None),
            (
                "images/Dockerfile.runtime".to_owned(),
                Some("runtime".to_owned())
            ),
        ]
    );
}

#[test]
fn tag_only_release_units_require_explicit_baseline_versions() {
    let repo = TestRepo::new();
    repo.write("images/Dockerfile.runtime", "FROM scratch\n");
    repo.commit("add tag-only baseline fixture");

    repo.cli().arg("init").assert().code(2);
    resolve_plan(&repo, |identity| CandidateResolution::Independent {
        release_unit: identity.to_owned(),
        package: identity.to_owned(),
    });
    repo.cli().arg("init").assert().success();
    repo.commit("adopt Intentional");

    repo.cli()
        .args(["tag", "--baseline"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "tag-only release unit runtime requires --version runtime=X.Y.Z",
        ));
    repo.cli()
        .args(["tag", "--baseline", "--version", "runtime=1.4.0"])
        .assert()
        .success();
    assert_eq!(git(&repo.root, &["tag", "--list"]), "runtime@1.4.0");
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
                package: identity.to_owned(),
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
        "$schema: https://intentional.foo/schemas/config.yml\ncontract: contract-2\nsettings:\n  internal-dependency-bump: patch\n  pre-1-0-bump-mapping: component\ndiscovery:\n  managed-paths:\n    - detector: npm-package\n      path: package.json\n      release-unit: sample-library\n      package: sample-library\n    - detector: cargo-package\n      path: components/rust/Cargo.toml\n      release-unit: sample-rust\n      package: sample-rust\nrelease-units:\n  sample-library:\n    path: .\n    packages:\n      sample-library: { path: . }\n    projections:\n      - adapter: npm\n        file: package.json\n        mode: committed\n    tags:\n      primary:\n        role: primary\n        template: 'sample-library@{version}'\n    depends-on: [ sample-rust ]\n  sample-rust:\n    path: components/rust\n    packages:\n      sample-rust: { path: . }\n    projections:\n      - adapter: cargo\n        file: Cargo.toml\n        mode: committed\n    tags:\n      primary:\n        role: primary\n        template: 'sample-rust@{version}'\n",
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
        package: identity.to_owned(),
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
        (".devcontainer", "cached-devcontainer"),
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
        "cached-devcontainer",
    ] {
        assert!(!names.contains(excluded), "unexpected candidate {excluded}");
    }
}

#[test]
fn development_environment_directories_produce_no_release_candidates() {
    let repo = TestRepo::new();
    repo.write(".devcontainer/Dockerfile", "FROM scratch\n");
    repo.write(
        ".devcontainer/devcontainer.json",
        "{ \"name\": \"sample\" }\n",
    );
    repo.write(
        ".devcontainer/action.yml",
        "name: Sample\nruns:\n  using: composite\n",
    );
    repo.write(
        ".devcontainer/main.tf",
        "resource \"null_resource\" \"sample\" {}\n",
    );
    repo.write("images/Dockerfile.runtime", "FROM scratch\n");
    repo.commit("add development environment fixtures");

    let plan = initialize(&repo.root, false)
        .expect("development environment plan")
        .plan
        .expect("unresolved candidates");
    assert_eq!(
        plan.discovery_candidates
            .iter()
            .map(|candidate| candidate.path.to_string_lossy().into_owned())
            .collect::<Vec<_>>(),
        vec!["images/Dockerfile.runtime".to_owned()]
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

fn git_fsck_strict(root: &Path) {
    let output = ProcessCommand::new("git")
        .args(["fsck", "--strict"])
        .current_dir(root)
        .output()
        .expect("run git fsck");
    assert!(
        output.status.success(),
        "git fsck --strict failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn tag_object_id(root: &Path, name: &str) -> String {
    git(root, &["rev-parse", &format!("refs/tags/{name}")])
}

fn assert_tagger_header(root: &Path, name: &str) {
    let record = git(root, &["cat-file", "-p", name]);
    assert!(
        record.lines().any(|line| line.starts_with("tagger ")),
        "expected tagger header in {name}: {record}"
    );
    assert!(
        record.contains("Intentional <intentional@wyrd.company>"),
        "expected deterministic tagger identity in {name}: {record}"
    );
}

fn create_taggerless_fixture_tag(root: &Path, name: &str, body: &str) {
    let mut child = ProcessCommand::new("git")
        .args(["hash-object", "-t", "tag", "-w", "--literally", "--stdin"])
        .current_dir(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn git hash-object");
    use std::io::Write;
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(body.as_bytes())
        .expect("write tag body");
    let output = child.wait_with_output().expect("finish git hash-object");
    assert!(
        output.status.success(),
        "git hash-object failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let oid = String::from_utf8(output.stdout)
        .expect("UTF-8 git output")
        .trim()
        .to_owned();
    git(root, &["update-ref", &format!("refs/tags/{name}"), &oid]);
    assert_eq!(git(root, &["cat-file", "-t", name]), "tag");
}

fn strip_tagger_from_tag_object(raw: &str) -> String {
    raw.split_inclusive('\n')
        .filter(|line| !line.starts_with("tagger "))
        .collect()
}

fn raw_tag_object(root: &Path, name: &str) -> String {
    let oid = git(root, &["rev-parse", &format!("refs/tags/{name}")]);
    git_raw(root, &["cat-file", "tag", &oid])
}

const VARIED_AMBIENT_ENV: &[(&str, &str)] = &[
    ("GIT_AUTHOR_NAME", "Different Author"),
    ("GIT_AUTHOR_EMAIL", "different@example.invalid"),
    ("GIT_COMMITTER_NAME", "Different Committer"),
    ("GIT_COMMITTER_EMAIL", "committer@example.invalid"),
    ("TZ", "Pacific/Auckland"),
];

fn prepare_applied_release(repo: &TestRepo) -> Value {
    repo.write("package.json", &npm_manifest("0.0.0"));
    repo.commit("add fixture");
    initialize_independent(repo);
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
    let output = repo.cli().arg("plan").output().expect("plan command");
    assert!(output.status.success());
    let plan: Value = serde_json::from_slice(&output.stdout).expect("plan JSON");
    fs::write(repo.root.join("release-plan.json"), &output.stdout).unwrap();
    repo.cli().arg("apply").assert().success();
    repo.commit("apply release");
    plan
}

#[test]
fn annotated_release_tags_include_deterministic_tagger_and_pass_strict_fsck() {
    let repo = TestRepo::new();
    let plan = prepare_applied_release(&repo);
    repo.cli()
        .args(["tag", "--plan", "release-plan.json"])
        .assert()
        .success();
    assert_tagger_header(&repo.root, "0.0.1");
    assert_tagger_header(&repo.root, "sample-library@0.0.1");
    let record = git(&repo.root, &["cat-file", "-p", "sample-library@0.0.1"]);
    assert!(record.contains(&format!(
        "plan-digest: {}",
        plan["digest"].as_str().unwrap()
    )));
    git_fsck_strict(&repo.root);
}

#[test]
fn baseline_tags_include_deterministic_tagger_and_pass_strict_fsck() {
    let repo = TestRepo::new();
    repo.write("package.json", &npm_manifest("1.0.0"));
    repo.commit("add fixture");
    initialize_independent(&repo);
    // A workspace tag carries its own version stream, so the baseline states
    // where that stream starts rather than deriving it from a release unit.
    repo.cli()
        .args(["tag", "--baseline", "--version", "workspace/release=1.0.0"])
        .assert()
        .success();
    assert_tagger_header(&repo.root, "sample-library@1.0.0");
    git_fsck_strict(&repo.root);
}

#[test]
fn tag_object_identity_is_independent_of_ambient_git_identity_and_timezone() {
    let repo = TestRepo::new();
    let plan = prepare_applied_release(&repo);
    repo.cli()
        .args(["tag", "--plan", "release-plan.json"])
        .assert()
        .success();
    let workspace_tag_id = tag_object_id(&repo.root, "0.0.1");
    let release_unit_tag_id = tag_object_id(&repo.root, "sample-library@0.0.1");
    git(&repo.root, &["tag", "-d", "0.0.1"]);
    git(&repo.root, &["tag", "-d", "sample-library@0.0.1"]);
    repo.cli_with_env(VARIED_AMBIENT_ENV)
        .args(["tag", "--plan", "release-plan.json"])
        .assert()
        .success();
    assert_eq!(tag_object_id(&repo.root, "0.0.1"), workspace_tag_id);
    assert_eq!(
        tag_object_id(&repo.root, "sample-library@0.0.1"),
        release_unit_tag_id
    );
    git_fsck_strict(&repo.root);
    let _ = plan;
}

#[test]
fn legacy_taggerless_release_records_remain_valid_authority() {
    let repo = TestRepo::new();
    let plan = prepare_applied_release(&repo);
    repo.cli()
        .args(["tag", "--plan", "release-plan.json"])
        .assert()
        .success();
    let release_record = raw_tag_object(&repo.root, "sample-library@0.0.1");
    let taggerless = strip_tagger_from_tag_object(&release_record);
    git(&repo.root, &["tag", "-d", "sample-library@0.0.1"]);
    create_taggerless_fixture_tag(&repo.root, "sample-library@0.0.1", &taggerless);
    repo.cli()
        .args(["tag", "--plan", "release-plan.json"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
    repo.cli().arg("check").assert().success();
    repo.cli()
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("Drift: none"));
    let _ = plan;
}

#[derive(Serialize)]
struct PlanDigestPayload<'a> {
    contract: &'a str,
    generator: &'a Generator,
    #[serde(skip_serializing_if = "Option::is_none")]
    channel: &'a Option<String>,
    release_units: &'a [PlanReleaseUnit],
    tags: &'a [PlanTag],
    tag_order: &'a [String],
}

fn reseal_plan_with_generator(mut plan: ReleasePlan, generator_version: &str) -> ReleasePlan {
    plan.generator.version = generator_version.to_owned();
    let payload = PlanDigestPayload {
        contract: &plan.contract,
        generator: &plan.generator,
        channel: &plan.channel,
        release_units: &plan.release_units,
        tags: &plan.tags,
        tag_order: &plan.tag_order,
    };
    plan.digest = format!(
        "sha256:{:x}",
        Sha256::digest(canonical_json(&payload).expect("canonical JSON").as_bytes())
    );
    plan
}

fn reseal_plan_with_contract(mut plan: ReleasePlan, contract: &str) -> ReleasePlan {
    plan.contract = contract.to_owned();
    let generator_version = plan.generator.version.clone();
    reseal_plan_with_generator(plan, &generator_version)
}

fn prepare_applied_release_with_prior_plan_generator(
    repo: &TestRepo,
    prior_generator: &str,
) -> ReleasePlan {
    repo.write("package.json", &npm_manifest("0.0.0"));
    repo.commit("add fixture");
    initialize_independent(repo);
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
    let output = repo.cli().arg("plan").output().expect("plan command");
    assert!(output.status.success());
    let plan: ReleasePlan = serde_json::from_slice(&output.stdout).expect("plan JSON from CLI");
    let plan = reseal_plan_with_generator(plan, prior_generator);
    fs::write(
        repo.root.join("release-plan.json"),
        plan.to_canonical_json().expect("plan JSON"),
    )
    .expect("write plan");
    repo.cli().arg("apply").assert().success();
    repo.commit("apply release");
    plan
}

#[test]
fn accepts_prior_version_sealed_plan_for_self_hosted_release() {
    let repo = TestRepo::new();
    let prior_generator = "0.1.0";
    let plan = prepare_applied_release_with_prior_plan_generator(&repo, prior_generator);
    repo.cli()
        .args(["tag", "--plan", "release-plan.json", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("create annotated tag 0.0.1"));
    repo.cli()
        .args(["tag", "--plan", "release-plan.json"])
        .assert()
        .success();
    let tag_generator = format!("generator: intentional {}", env!("CARGO_PKG_VERSION"));
    let release_record = raw_tag_object(&repo.root, "sample-library@0.0.1");
    assert!(release_record.contains(&tag_generator));
    assert!(release_record.contains(&format!("plan-digest: {}", plan.digest)));
    assert_tagger_header(&repo.root, "0.0.1");
    assert_tagger_header(&repo.root, "sample-library@0.0.1");
    git_fsck_strict(&repo.root);
}

#[test]
fn accepts_a_supplied_plan_written_under_a_supported_prior_contract() {
    let repo = TestRepo::new();
    let current = prepare_applied_release_with_prior_plan_generator(&repo, "0.1.0");
    let prior = reseal_plan_with_contract(current, "contract-1");
    fs::write(
        repo.root.join("release-plan.json"),
        prior.to_canonical_json().expect("prior plan JSON"),
    )
    .expect("write prior plan");

    repo.cli()
        .args(["tag", "--plan", "release-plan.json"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "create annotated tag sample-library@0.0.1",
        ));
    let record = raw_tag_object(&repo.root, "sample-library@0.0.1");
    assert!(record.contains("contract: contract-2"), "{record}");
    assert!(record.contains(&format!("plan-digest: {}", prior.digest)));
}

#[test]
fn rejects_future_plan_generator_version() {
    let repo = TestRepo::new();
    let plan = prepare_applied_release_with_prior_plan_generator(&repo, "99.0.0");
    repo.cli()
        .args(["tag", "--plan", "release-plan.json", "--dry-run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("newer than intentional"));
    let _ = plan;
}

#[test]
fn rejects_supplied_plan_digest_mismatch() {
    let repo = TestRepo::new();
    prepare_applied_release_with_prior_plan_generator(&repo, "0.1.0");
    let mut plan_text = fs::read_to_string(repo.root.join("release-plan.json")).unwrap();
    plan_text = plan_text.replace("sha256:", "sha256:deadbeef");
    fs::write(repo.root.join("release-plan.json"), plan_text).unwrap();
    repo.cli()
        .args(["tag", "--plan", "release-plan.json", "--dry-run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("digest mismatch"));
}

#[test]
fn executor_init_requires_resolutions_then_configures_publication() {
    let repo = TestRepo::new();
    repo.write(
        ".intentional/config.yml",
        r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-2
release-units:
  component:
    path: component
    tags:
      primary: { role: primary, template: '{id}@{version}' }
"#,
    );
    repo.write(
        "component/package.json",
        r#"{"name":"example-component","version":"1.0.0"}"#,
    );

    repo.cli()
        .args(["executor", "init"])
        .assert()
        .code(2)
        .stdout(predicate::str::contains("ruleset bypass actor"))
        .stdout(predicate::str::contains(
            ".intentional/executor-init-plan.yml",
        ));

    let plan_path = repo.root.join(".intentional/executor-init-plan.yml");
    let plan = fs::read_to_string(&plan_path).expect("plan written");
    fs::write(
        &plan_path,
        plan.replace("resolution: null", "resolution: accept"),
    )
    .expect("resolve plan");

    repo.cli().args(["executor", "init"]).assert().code(2);
    let plan = fs::read_to_string(&plan_path).expect("plan written");
    fs::write(
        &plan_path,
        plan.replace("resolution: null", "resolution: decline"),
    )
    .expect("resolve additional target");

    repo.cli()
        .args(["executor", "init"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "configure the npm primary publisher",
        ));

    let config = fs::read_to_string(repo.root.join(".intentional/config.yml")).expect("config");
    assert!(config.contains("github:"), "{config}");
    assert!(config.contains("npm:"), "{config}");
}

#[test]
fn executor_check_reports_locally_observable_nonconformance() {
    let repo = executor_repository();
    let workflow = "name: sample\non: { workflow_dispatch: {} }\njobs:\n  candidate_check:\n    runs-on: ubuntu-latest\n    steps: [ { run: 'true' } ]\n";
    repo.write(".github/workflows/publish.yml", workflow);

    repo.cli()
        .args(["executor", "check"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "publication: component/npm/primary",
        ))
        .stdout(predicate::str::contains(
            "release workflow: .github/workflows/release.yml does not exist",
        ));

    repo.write(".github/workflows/release.yml", workflow);
    repo.cli()
        .args(["executor", "check"])
        .assert()
        .code(1)
        .stdout(predicate::str::contains(
            "Reserved workflow slice differs from the derived contract",
        ));

    for role in ["release", "publish"] {
        repo.cli()
            .args(["executor", "diff", role, "--apply"])
            .assert()
            .success();
    }
    repo.cli()
        .args(["executor", "check"])
        .assert()
        .success()
        .stdout(predicate::str::contains("executor check passed"));
}

/// Workspace with a configured GitHub executor and one resolvable publication.
fn executor_repository() -> TestRepo {
    let repo = TestRepo::new();
    repo.write(
        ".intentional/config.yml",
        r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-2
workspace-tags:
  release:
    template: '{version}'
github:
  workflows:
    release: { path: .github/workflows/release.yml, gates: [ candidate_check ] }
    publish: { path: .github/workflows/publish.yml }
release-units:
  component:
    path: component
    packages:
      package:
        path: .
        npm: {}
    tags:
      primary: { role: primary, template: '{id}@{version}', require-phase: after-publication }
"#,
    );
    repo.write(
        "component/package.json",
        r#"{"name":"example-component","version":"1.0.0"}"#,
    );
    repo
}

#[test]
fn executor_diff_reports_a_patch_applies_it_and_refuses_stale_input() {
    let repo = executor_repository();
    let source = "# repository owned\nname: release\n\non:\n  workflow_dispatch:\n\njobs:\n  candidate_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n";
    repo.write(".github/workflows/release.yml", source);

    repo.cli()
        .args(["executor", "diff", "release"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "--- a/.github/workflows/release.yml",
        ))
        .stdout(predicate::str::contains("+  intentional_prepare:"));
    assert_eq!(
        std::fs::read_to_string(repo.root.join(".github/workflows/release.yml"))
            .expect("workflow readable"),
        source,
        "a comparison without --apply never writes"
    );

    let json = repo
        .cli()
        .args(["executor", "diff", "release", "--format", "json"])
        .output()
        .expect("diff runs");
    let result: serde_json::Value =
        serde_json::from_slice(&json.stdout).expect("structured result parses");
    assert_eq!(result["status"], "different");
    assert_eq!(result["applied"], false);
    assert_eq!(result["workflow"]["path"], ".github/workflows/release.yml");

    repo.cli()
        .args(["executor", "diff", "release", "--apply"])
        .assert()
        .success()
        .stdout(predicate::str::contains("applied the transformation"));
    let applied = std::fs::read_to_string(repo.root.join(".github/workflows/release.yml"))
        .expect("workflow readable");
    assert!(
        applied.starts_with("# repository owned"),
        "repository content survives the applied transformation: {applied}"
    );

    repo.cli()
        .args(["executor", "diff", "release"])
        .assert()
        .success()
        .stdout(predicate::str::contains("(conformant)"));
}

#[test]
fn executor_diff_compares_an_explicit_workflow_and_blocks_on_a_missing_one() {
    let repo = executor_repository();
    repo.write(
        "candidate.yml",
        "name: candidate\non:\n  workflow_dispatch:\njobs:\n  candidate_check:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n",
    );
    repo.cli()
        .args(["executor", "diff", "release", "--workflow", "candidate.yml"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--- a/candidate.yml"));

    repo.cli()
        .args(["executor", "diff", "publish"])
        .assert()
        .failure()
        .stdout(predicate::str::contains("[workflow-missing]"));
}

#[test]
fn executor_check_requires_the_github_executor() {
    let repo = TestRepo::new();
    repo.write(
        ".intentional/config.yml",
        r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-2
release-units:
  component:
    path: component
    tags:
      primary: { role: primary, template: '{id}@{version}' }
"#,
    );
    repo.cli()
        .args(["executor", "check"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no github executor configuration"));
}

#[test]
fn executor_init_dry_run_writes_nothing_and_prints_the_plan() {
    let repo = TestRepo::new();
    repo.write(
        ".intentional/config.yml",
        r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-2
release-units:
  component:
    path: component
    tags:
      primary: { role: primary, template: '{id}@{version}' }
"#,
    );
    repo.write(
        "component/package.json",
        r#"{"name":"example-component","version":"1.0.0"}"#,
    );

    repo.cli()
        .args(["executor", "init", "--dry-run"])
        .assert()
        .code(2)
        .stdout(predicate::str::contains(
            "executor initialization state: needs-input",
        ))
        .stdout(predicate::str::contains(
            "would write .intentional/executor-init-plan.yml",
        ))
        .stdout(predicate::str::contains(
            "$schema: https://intentional.foo/schemas/executor-init-plan.yml",
        ));

    assert!(
        !repo
            .root
            .join(".intentional/executor-init-plan.yml")
            .exists(),
        "a dry run never writes the plan it previews"
    );
}

const EVIDENCE_CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-2
github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml }
workspace-tags:
  release:
    template: 'release/{version}'
release-units:
  sample-library:
    path: .
    packages:
      package:
        path: .
        npm: {}
    projections:
      - adapter: json
        file: package.json
        pointer: /version
        mode: committed
    tags:
      primary: { role: primary, template: '{id}@{version}', require-phase: before-publication }
"#;

fn evidence_fragment(released: &BTreeMap<String, String>) -> String {
    let source = &released["source"];
    let release = &released["release"];
    let tag_name = &released["global-tag"];
    let tag_object = &released["global-tag-object"];
    let plan_digest = &released["plan-digest"];
    let version = &released["version"];
    format!(
        r#"$schema: https://intentional.foo/schemas/publisher-evidence/v1
contract: publisher-evidence-1
release-unit: sample-library
publisher: npm
target: primary
source-commit: {source}
release-commit: {release}
global-tag:
  name: {tag_name}
  object: {tag_object}
  target: {release}
plan-digest: {plan_digest}
subject:
  kind: npm-package
  identity: sample-library
  version: {version}
  digest: sha256:5555555555555555555555555555555555555555555555555555555555555555
packager:
  id: npm
  version: 10.8.2
build-provenance: []
attached-metadata: []
destination:
  identity: registry.example.test/sample-library
  version: {version}
  digest: sha512-example
clean-client:
  mode: public
  client: npm
  version: 10.8.2
  digest: sha512-example
destination-aliases: []
phase-tags: []
"#
    )
}

/// Drive a fixture repository to a genuinely released checkout.
///
/// `evidence assemble` proves the checkout it is given by reproducing the
/// release from the accepted source commit, so the CLI's end-to-end assembly
/// path needs a repository that actually carries a release. Preparing the
/// candidate and importing its bundle is what the release workflow does; doing
/// it here keeps this test on the same artifact rather than on a checkout
/// assembled out of parts.
///
/// Returns the identities the release carries.
fn release_the_fixture(repo: &TestRepo) -> BTreeMap<String, String> {
    let remote = repo.root.join("..").join("remote.git");
    git(
        &repo.root,
        &[
            "init",
            "--quiet",
            "--bare",
            "--initial-branch=main",
            remote.to_str().expect("remote path"),
        ],
    );
    git(&repo.root, &["add", "-A"]);
    git(
        &repo.root,
        &["commit", "--quiet", "-m", "Create the workspace"],
    );
    // A workspace tag carries its own version stream, so the baseline states
    // where that stream starts rather than deriving it from a release unit.
    repo.cli()
        .args(["tag", "--baseline", "--version", "workspace/release=1.0.0"])
        .assert()
        .success();
    git(
        &repo.root,
        &[
            "remote",
            "add",
            "origin",
            remote.to_str().expect("remote path"),
        ],
    );
    git(
        &repo.root,
        &["push", "--quiet", "origin", "HEAD:main", "--tags"],
    );
    repo.cli()
        .args([
            "add",
            "--release-unit",
            "sample-library:minor",
            "--message",
            "Add a capability",
        ])
        .assert()
        .success();
    git(&repo.root, &["add", "-A"]);
    git(
        &repo.root,
        &["commit", "--quiet", "-m", "Record release intent"],
    );
    git(&repo.root, &["push", "--quiet", "origin", "HEAD:main"]);

    let handoff = repo.root.join("..").join("release-candidate");
    repo.cli()
        .args(["release", "prepare", "--output"])
        .arg(&handoff)
        .assert()
        .success();
    let manifest: serde_yaml::Value = serde_yaml::from_str(
        &fs::read_to_string(handoff.join("release-candidate.yml")).expect("manifest"),
    )
    .expect("the manifest parses");

    let bundle = handoff.join("release.bundle");
    git(
        &repo.root,
        &[
            "fetch",
            "--quiet",
            bundle.to_str().expect("bundle path"),
            "refs/heads/intentional-release:refs/intentional/imported",
            "refs/tags/intentional-global-release:refs/intentional/imported-tag",
        ],
    );
    let field = |path: [&str; 2]| {
        manifest[path[0]][path[1]]
            .as_str()
            .expect("a manifest identity")
            .to_owned()
    };
    let tag_name = field(["global-tag", "name"]);
    let tag_object = field(["global-tag", "object"]);
    git(
        &repo.root,
        &["update-ref", &format!("refs/tags/{tag_name}"), &tag_object],
    );
    let release = field(["release", "commit"]);
    git(&repo.root, &["checkout", "--quiet", "--detach", &release]);
    // The version is read from the sealed plan rather than predicted, because
    // it is what the plan assigns and the fragments have to record it.
    let plan: Value = serde_json::from_str(
        &fs::read_to_string(handoff.join("release-plan.json")).expect("sealed plan"),
    )
    .expect("the sealed plan parses");
    let version = plan["release_units"][0]["new_version"]
        .as_str()
        .expect("the plan assigns a version")
        .to_owned();
    BTreeMap::from([
        ("version".to_owned(), version),
        ("source".to_owned(), field(["source", "commit"])),
        ("release".to_owned(), release),
        ("global-tag".to_owned(), tag_name),
        ("global-tag-object".to_owned(), tag_object),
        ("plan-digest".to_owned(), field(["plan", "digest"])),
    ])
}

/// The sealed before-publication evidence this configuration declares.
fn evidence_phase(released: &BTreeMap<String, String>) -> String {
    let source = &released["source"];
    let release = &released["release"];
    let tag_name = &released["global-tag"];
    let plan_digest = &released["plan-digest"];
    let version = &released["version"];
    format!(
        r#"$schema: https://intentional.foo/schemas/phase-tag-evidence/v1
phase: before-publication
source-commit: {source}
release-commit: {release}
global-tag: {tag_name}
plan-digest: {plan_digest}
subjects:
  - release-unit: sample-library
    identity: sample-library
    version: {version}
    digest: sha256:5555555555555555555555555555555555555555555555555555555555555555
intended-destinations:
  - release-unit: sample-library
    publisher: npm
    target: primary
"#
    )
}

fn assembly_environment() -> Vec<(&'static str, &'static str)> {
    vec![
        ("GITHUB_REPOSITORY", "example-owner/example-repository"),
        ("GITHUB_WORKFLOW", "publish"),
        ("GITHUB_RUN_ID", "42"),
        ("GITHUB_RUN_ATTEMPT", "1"),
        ("GITHUB_SHA", "2222222222222222222222222222222222222222"),
    ]
}

/// Write one fixture file at an absolute path outside the repository.
fn write_outside(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("fixture parent");
    }
    fs::write(path, contents).expect("write fixture file");
}

/// Stage one contribution through the CLI and place it under its transport name.
fn contribute_artifact(repo: &TestRepo, namespace: &str, job: &str, attempt: &str) -> PathBuf {
    let staged = repo
        .outside(&format!("staging/{namespace}-{job}-{attempt}"))
        .to_str()
        .expect("staging path")
        .to_owned();
    let output = repo
        .cli_with_env(&[("GITHUB_JOB", job), ("GITHUB_RUN_ATTEMPT", attempt)])
        .args([
            "evidence",
            "contribute",
            "--namespace",
            namespace,
            "--value-file",
            "value.yml",
            "--attachment",
            "report.json",
            "--output",
            &staged,
        ])
        .assert()
        .success();
    let stdout = String::from_utf8(output.get_output().stdout.clone()).expect("UTF-8 stdout");
    let artifact = stdout
        .lines()
        .find_map(|line| line.strip_prefix("artifact-name: "))
        .expect("contribute names its transport artifact")
        .to_owned();
    let destination = repo.outside("artifacts").join(&artifact);
    fs::create_dir_all(destination.parent().expect("artifacts directory"))
        .expect("artifacts directory");
    fs::rename(&staged, &destination).expect("transport the bundle");
    destination
}

#[test]
fn evidence_contribute_writes_a_bundle_and_names_its_transport_artifact() {
    let repo = TestRepo::new();
    repo.write("value.yml", "outcome: clean\n");
    repo.write("report.json", "{\"ok\":true}");

    repo.cli_with_env(&[("GITHUB_JOB", "scan"), ("GITHUB_RUN_ATTEMPT", "3")])
        .args([
            "evidence",
            "contribute",
            "--namespace",
            "assessment",
            "--value-file",
            "value.yml",
            "--attachment",
            "report.json",
            "--output",
            "contribution",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("attachment: report.json sha256:"))
        .stdout(predicate::str::contains(
            "artifact-name: intentional-contribution-",
        ))
        .stdout(predicate::str::contains("-scan-3"));

    let manifest = fs::read_to_string(repo.root.join("contribution/contribution.yml"))
        .expect("contribution manifest");
    assert!(manifest.contains("namespace: assessment"), "{manifest}");
    assert!(
        manifest.contains("file: attachments/report.json"),
        "{manifest}"
    );
    assert!(
        repo.root
            .join("contribution/attachments/report.json")
            .is_file(),
        "the bundle transports the exact contributed file"
    );
}

#[test]
fn evidence_contribute_requires_content() {
    let repo = TestRepo::new();
    repo.cli()
        .args([
            "evidence",
            "contribute",
            "--namespace",
            "assessment",
            "--output",
            "contribution",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "at least one value file or attachment is required",
        ));
}

#[test]
fn evidence_assemble_closes_one_bundle_from_fragments_and_contributions() {
    let repo = TestRepo::new();
    repo.write(".intentional/config.yml", EVIDENCE_CONFIG);
    repo.write("package.json", &npm_manifest("1.0.0"));
    repo.write("value.yml", "outcome: clean\n");
    repo.write("report.json", "{\"ok\":true}");
    let released = release_the_fixture(&repo);
    write_outside(
        &repo.outside("artifacts/publisher/evidence.yml"),
        &evidence_fragment(&released),
    );
    write_outside(
        &repo.outside("artifacts/phase/phase.yml"),
        &evidence_phase(&released),
    );
    contribute_artifact(&repo, "assessment", "scan", "1");

    let input = repo.outside("artifacts");
    let output = repo.outside("release-evidence");
    repo.cli_with_env(&assembly_environment())
        .args([
            "evidence",
            "assemble",
            "--input",
            input.to_str().expect("input path"),
            "--output",
            output.to_str().expect("output path"),
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("attachment: report.json"));

    let evidence =
        fs::read_to_string(output.join("intentional-evidence.yml")).expect("release evidence");
    assert!(
        evidence.contains("contract: release-evidence-1"),
        "{evidence}"
    );
    assert!(evidence.contains("run-id: 42"), "{evidence}");
    assert!(evidence.contains("outcome: clean"), "{evidence}");
    assert!(
        output.join("attachments/report.json").is_file(),
        "accepted contributed files are emitted once beside the statement"
    );
}

#[test]
fn evidence_assemble_requires_its_workflow_identity() {
    let repo = TestRepo::new();
    repo.write(".intentional/config.yml", EVIDENCE_CONFIG);
    repo.write("package.json", &npm_manifest("1.0.0"));
    let released = release_the_fixture(&repo);
    write_outside(
        &repo.outside("artifacts/publisher/evidence.yml"),
        &evidence_fragment(&released),
    );
    write_outside(
        &repo.outside("artifacts/phase/phase.yml"),
        &evidence_phase(&released),
    );
    let input = repo.outside("artifacts");
    let output = repo.outside("release-evidence");
    repo.cli()
        .env_remove("GITHUB_REPOSITORY")
        .args([
            "evidence",
            "assemble",
            "--input",
            input.to_str().expect("input path"),
            "--output",
            output.to_str().expect("output path"),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "GITHUB_REPOSITORY identifies the assembling workflow run",
        ));
}

#[test]
fn evidence_contribute_rejects_a_malformed_run_attempt() {
    let repo = TestRepo::new();
    repo.write("value.yml", "outcome: clean\n");
    repo.cli_with_env(&[("GITHUB_JOB", "scan"), ("GITHUB_RUN_ATTEMPT", "second")])
        .args([
            "evidence",
            "contribute",
            "--namespace",
            "assessment",
            "--value-file",
            "value.yml",
            "--output",
            "contribution",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "GITHUB_RUN_ATTEMPT must be a whole number",
        ));
}

#[test]
fn evidence_assemble_rejects_a_malformed_run_identifier() {
    let repo = TestRepo::new();
    repo.write(".intentional/config.yml", EVIDENCE_CONFIG);
    repo.write("package.json", &npm_manifest("1.0.0"));
    let released = release_the_fixture(&repo);
    write_outside(
        &repo.outside("artifacts/publisher/evidence.yml"),
        &evidence_fragment(&released),
    );
    write_outside(
        &repo.outside("artifacts/phase/phase.yml"),
        &evidence_phase(&released),
    );
    let mut environment = assembly_environment();
    environment.retain(|(key, _)| *key != "GITHUB_RUN_ID");
    environment.push(("GITHUB_RUN_ID", "run-42"));
    let input = repo.outside("artifacts");
    let output = repo.outside("release-evidence");
    repo.cli_with_env(&environment)
        .args([
            "evidence",
            "assemble",
            "--input",
            input.to_str().expect("input path"),
            "--output",
            output.to_str().expect("output path"),
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "GITHUB_RUN_ID must be a whole number",
        ));
}
