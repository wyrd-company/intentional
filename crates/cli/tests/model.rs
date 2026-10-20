// ---
// relationships:
//   tests: release-model
// ---

use assert_cmd::Command;
use intentional_core::{CandidateResolution, InitPlan, ReleaseModel, RELEASE_MODEL_PATH};
use predicates::prelude::*;
use serde_json::Value;
use std::{fs, path::Path, process::Command as ProcessCommand};
use tempfile::TempDir;

struct Repo(TempDir);
impl Repo {
    fn new() -> Self {
        let repo = Self(tempfile::tempdir().unwrap());
        git(repo.path(), &["init", "-q"]);
        git(repo.path(), &["config", "user.name", "Fixture Author"]);
        git(
            repo.path(),
            &["config", "user.email", "fixture@example.invalid"],
        );
        repo.write(".intentional/config.yml", "contract: contract-1\nrelease-units:\n  sample:\n    path: .\n    projections:\n      - adapter: npm\n        file: package.json\n        mode: committed\n    tags:\n      primary:\n        role: primary\n        template: 'sample@{version}'\n");
        repo.write(
            "package.json",
            r#"{"name":"sample-library","version":"1.0.0"}"#,
        );
        repo
    }
    fn path(&self) -> &Path {
        self.0.path()
    }
    fn write(&self, file: &str, text: &str) {
        let path = self.path().join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    fn cli(&self) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("intentional"));
        command.arg("-C").arg(self.path());
        command
    }
    fn read_model(&self) -> Vec<u8> {
        fs::read(self.path().join(RELEASE_MODEL_PATH)).unwrap()
    }
}
fn git(root: &Path, args: &[&str]) {
    let result = ProcessCommand::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn model_written_unchanged_dry_run_stdout_and_json() {
    let repo = Repo::new();
    repo.cli()
        .args(["model", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("dry-run: written"));
    assert!(!repo.path().join(RELEASE_MODEL_PATH).exists());
    let output = repo
        .cli()
        .args(["model", "--stdout"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(!repo.path().join(RELEASE_MODEL_PATH).exists());
    repo.cli()
        .arg("model")
        .assert()
        .success()
        .stdout("written .intentional/release-model.json\n");
    assert_eq!(repo.read_model(), output);
    let modified = fs::metadata(repo.path().join(RELEASE_MODEL_PATH))
        .unwrap()
        .modified()
        .unwrap();
    repo.cli().args(["model", "--json"]).assert().success().stdout("{\"dry_run\":false,\"operation\":\"unchanged\",\"path\":\".intentional/release-model.json\"}\n");
    assert_eq!(repo.read_model(), output);
    assert_eq!(
        fs::metadata(repo.path().join(RELEASE_MODEL_PATH))
            .unwrap()
            .modified()
            .unwrap(),
        modified
    );
    let model: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        model["release_units"]["sample"]["projections"][0]["generic"],
        false
    );
    assert_eq!(
        model["release_units"]["sample"]["projections"][0]["native_identity"],
        "sample-library"
    );
    assert_eq!(
        model["release_units"]["sample"]["projections"][0]["workspace_file"],
        "package.json"
    );
    assert_eq!(
        model["release_units"]["sample"]["changelog"],
        "CHANGELOG.md"
    );
    assert!(output.ends_with(b"\n"));
    assert!(!output.ends_with(b"\n\n"));
    assert!(String::from_utf8(output)
        .unwrap()
        .starts_with("{\n  \"$schema\":"));
}

#[test]
fn invalid_config_or_manifest_never_writes_or_replaces_a_model() {
    let repo = Repo::new();
    repo.write("package.json", "{");
    repo.cli().arg("model").assert().code(1);
    assert!(!repo.path().join(RELEASE_MODEL_PATH).exists());
    repo.write(RELEASE_MODEL_PATH, "old model bytes\n");
    for args in [
        vec!["model"],
        vec!["model", "--stdout"],
        vec!["model", "--dry-run"],
    ] {
        repo.cli().args(args).assert().code(1);
        assert_eq!(repo.read_model(), b"old model bytes\n");
    }
    repo.write("package.json", r#"{"name":"sample-library"}"#);
    repo.cli().arg("model").assert().code(1);
    assert_eq!(repo.read_model(), b"old model bytes\n");
    repo.write(
        ".intentional/config.yml",
        "contract: unknown\nrelease-units: {}\n",
    );
    repo.cli().arg("model").assert().code(1);
    assert_eq!(repo.read_model(), b"old model bytes\n");
}

#[test]
fn model_bytes_ignore_clone_remote_branch_clock_versions_tags_and_intents() {
    let repo = Repo::new();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "fixture"]);
    let clones = tempfile::tempdir().unwrap();
    let mut outputs = Vec::new();
    for (name, remote, clock) in [
        (
            "first",
            "https://example.invalid/first",
            "2000-01-01T00:00:00Z",
        ),
        (
            "second",
            "https://example.invalid/second",
            "2030-01-01T00:00:00Z",
        ),
    ] {
        let clone = clones.path().join(name);
        git(
            clones.path(),
            &["clone", "-q", repo.path().to_str().unwrap(), name],
        );
        git(&clone, &["remote", "set-url", "origin", remote]);
        git(&clone, &["checkout", "-qb", name]);
        git(
            &clone,
            &[
                "tag",
                &format!("sample@{}", if name == "first" { "1.0.0" } else { "2.0.0" }),
            ],
        );
        if name == "second" {
            fs::create_dir_all(clone.join(".intentional/intents")).unwrap();
            fs::write(
                clone.join(".intentional/intents/change.md"),
                "---\nsample: minor\n---\n\nAdd a capability.\n",
            )
            .unwrap();
        }
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("intentional"));
        outputs.push(
            command
                .arg("-C")
                .arg(&clone)
                .args(["model", "--stdout"])
                .env("GIT_AUTHOR_DATE", clock)
                .env("GIT_COMMITTER_DATE", clock)
                .assert()
                .success()
                .get_output()
                .stdout
                .clone(),
        );
        assert!(!clone.join(RELEASE_MODEL_PATH).exists());
    }
    assert_eq!(outputs[0], outputs[1]);
    let model: Value = serde_json::from_slice(&outputs[0]).unwrap();
    assert_eq!(
        model.as_object().unwrap().keys().collect::<Vec<_>>(),
        vec![
            "$schema",
            "paths",
            "release_groups",
            "release_units",
            "workspace_tags"
        ]
    );
    repo.write(
        "package.json",
        r#"{"name":"sample-library","version":"9.0.0"}"#,
    );
    assert_eq!(
        ReleaseModel::build(repo.path())
            .unwrap()
            .encode()
            .unwrap()
            .as_bytes(),
        outputs[0]
    );
}

fn resolve_init(repo: &Repo) {
    repo.cli().arg("init").assert().code(2);
    let path = repo.path().join(".intentional/init-plan.yml");
    let mut plan: InitPlan = serde_yaml::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    for candidate in &mut plan.discovery_candidates {
        candidate.resolution = Some(CandidateResolution::Independent {
            release_unit: candidate.native_identity.clone().unwrap(),
        });
    }
    fs::write(path, plan.to_yaml().unwrap()).unwrap();
}

#[test]
fn init_writes_model_with_config_and_noop_leaves_absent_model_absent() {
    let repo = Repo::new();
    fs::remove_file(repo.path().join(".intentional/config.yml")).unwrap();
    resolve_init(&repo);
    repo.cli()
        .args(["init", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "write .intentional/release-model.json",
        ));
    assert!(!repo.path().join(RELEASE_MODEL_PATH).exists());
    repo.cli().arg("init").assert().success();
    let model = repo.read_model();
    assert_eq!(
        model,
        ReleaseModel::build(repo.path())
            .unwrap()
            .encode()
            .unwrap()
            .into_bytes()
    );
    repo.cli().arg("init").assert().success();
    assert_eq!(model, repo.read_model());
    fs::remove_file(repo.path().join(RELEASE_MODEL_PATH)).unwrap();
    repo.cli().arg("init").assert().success();
    assert!(!repo.path().join(RELEASE_MODEL_PATH).exists());
}

#[test]
fn init_reconciles_existing_model_and_empty_msbuild_package_id_falls_back() {
    let repo = Repo::new();
    fs::remove_file(repo.path().join(".intentional/config.yml")).unwrap();
    fs::remove_file(repo.path().join("package.json")).unwrap();
    repo.write("src/Sample.csproj", "<Project><PropertyGroup><PackageId> </PackageId><AssemblyName>sample-assembly</AssemblyName><Version>1.0.0</Version></PropertyGroup></Project>");
    repo.write(RELEASE_MODEL_PATH, "old model\n");
    resolve_init(&repo);
    repo.cli().arg("init").assert().success();
    let model: Value = serde_json::from_slice(&repo.read_model()).unwrap();
    assert_eq!(
        model["release_units"]["sample-assembly"]["projections"][0]["native_identity"],
        "sample-assembly"
    );
}

#[test]
fn model_projects_all_workspace_facts_and_preserves_projection_order() {
    let repo = Repo::new();
    let mut config = intentional_core::Config::load(repo.path()).unwrap();
    config.fixed = vec![vec!["sample".to_owned(), "other".to_owned()]];
    let mut other = config.release_units["sample"].clone();
    other.path = "other".into();
    other.projections.clear();
    other.tags.get_mut("primary").unwrap().template = "other@{version}".to_owned();
    other.release_record = intentional_core::ReleaseRecordPolicy::None;
    other.disposition = intentional_core::ReleaseUnitDisposition::Suspended;
    config.release_units.insert("other".to_owned(), other);
    config.release_units.get_mut("sample").unwrap().depends_on = vec!["other".to_owned()];
    let mut value = serde_yaml::to_value(&config).unwrap();
    value["discovery"] = serde_yaml::from_str("managed-paths:\n  - detector: npm\n    path: package.json\n    release-unit: sample\nexcluded-paths:\n  - detector: npm\n    path: excluded/package.json\n    evidence-digest: sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n").unwrap();
    value["workspace-tags"] = serde_yaml::from_str("release:\n  template: 'release-{version}'\n  require-phase: after-publication\n  tag-after: [release-unit/sample/primary]\n").unwrap();
    let config =
        intentional_core::Config::from_yaml(&serde_yaml::to_string(&value).unwrap()).unwrap();
    repo.write(".intentional/config.yml", &config.to_yaml().unwrap());
    let mut config = intentional_core::Config::load(repo.path()).unwrap();
    let cases = [
        (
            intentional_core::Adapter::Yaml,
            "version.yml",
            "version: 1.0.0\n",
            Some("/version"),
        ),
        (
            intentional_core::Adapter::Cargo,
            "Cargo.toml",
            "[package]\nname='sample-cargo'\nversion='1.0.0'\n",
            None,
        ),
        (
            intentional_core::Adapter::Pub,
            "pubspec.yaml",
            "name: sample-pub\nversion: 1.0.0\n",
            None,
        ),
        (
            intentional_core::Adapter::Python,
            "pyproject.toml",
            "[project]\nname='sample-python'\nversion='1.0.0'\n",
            None,
        ),
        (
            intentional_core::Adapter::Msbuild,
            "Sample.csproj",
            "<Project><Version>1.0.0</Version></Project>",
            None,
        ),
        (
            intentional_core::Adapter::Go,
            "go.mod",
            "module example.invalid/sample\n\ngo 1.22\n",
            None,
        ),
        (
            intentional_core::Adapter::Toml,
            "version.toml",
            "version='1.0.0'\n",
            Some("/version"),
        ),
        (
            intentional_core::Adapter::Json,
            "version.json",
            r#"{"version":"1.0.0"}"#,
            Some("/version"),
        ),
    ];
    for (adapter, file, text, pointer) in cases {
        repo.write(file, text);
        let mut projection = config.release_units["sample"].projections[0].clone();
        projection.adapter = adapter;
        projection.file = file.into();
        projection.pointer = pointer.map(str::to_owned);
        if adapter == intentional_core::Adapter::Go {
            projection.mode = intentional_core::ProjectionMode::None;
        }
        config
            .release_units
            .get_mut("sample")
            .unwrap()
            .projections
            .push(projection);
    }
    repo.write(".intentional/config.yml", &config.to_yaml().unwrap());
    repo.cli().arg("model").assert().success();
    let value: Value = serde_json::from_slice(&repo.read_model()).unwrap();
    let unit = &value["release_units"]["sample"];
    assert_eq!(unit["release_record"], "required");
    assert_eq!(unit["depends_on"], serde_json::json!(["other"]));
    assert_eq!(
        unit["release_group"],
        serde_json::json!({"kind":"fixed","members":["other","sample"]})
    );
    assert_eq!(
        unit["ecosystems"],
        serde_json::json!(["cargo", "go", "msbuild", "npm", "pub", "python"])
    );
    assert_eq!(
        unit["sources"],
        serde_json::json!([{"detector":"npm","path":"package.json"}])
    );
    assert_eq!(
        unit["projections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["adapter"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["npm", "yaml", "cargo", "pub", "python", "msbuild", "go", "toml", "json"]
    );
    for projection in unit["projections"].as_array().unwrap() {
        if projection["generic"] == true {
            assert!(projection.get("pointer").is_some());
            assert!(projection.get("native_identity").is_none());
        } else {
            assert!(projection.get("native_identity").is_some());
        }
    }
    assert_eq!(
        value["release_groups"]["fixed"],
        serde_json::json!([["other", "sample"]])
    );
    assert_eq!(value["release_units"]["other"]["release_record"], "none");
    assert_eq!(value["release_units"]["other"]["disposition"], "suspended");
    assert_eq!(
        value["release_units"]["other"]["projections"],
        serde_json::json!([])
    );
    assert_eq!(
        value["workspace_tags"]["release"],
        serde_json::json!({"template":"release-{version}", "require_phase":"after-publication", "tag_after":["release-unit/sample/primary"]})
    );
}

#[test]
fn proposed_config_is_validated_before_model_construction() {
    let repo = Repo::new();
    let mut config = intentional_core::Config::load(repo.path()).unwrap();
    config.contract = "invalid".to_owned();
    assert!(ReleaseModel::from_config(repo.path(), &config).is_err());
}

#[test]
fn unreadable_generic_manifest_in_none_mode_preserves_old_model() {
    for (adapter, file, invalid) in [
        ("json", "data.json", "{"),
        ("toml", "data.toml", "[broken"),
        ("yaml", "data.yml", "version: [broken"),
    ] {
        let repo = Repo::new();
        let config = fs::read_to_string(repo.path().join(".intentional/config.yml"))
            .unwrap()
            .replace(
                "adapter: npm",
                &format!("adapter: {adapter}\n        pointer: /version"),
            )
            .replace("file: package.json", &format!("file: {file}"))
            .replace("mode: committed", "mode: none");
        repo.write(".intentional/config.yml", &config);
        repo.write(file, invalid);
        repo.write(RELEASE_MODEL_PATH, "old model\n");
        repo.cli().arg("model").assert().code(1);
        assert_eq!(repo.read_model(), b"old model\n");
    }
}

fn checked_repo() -> Repo {
    let repo = Repo::new();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "fixture"]);
    repo.cli().args(["tag", "--baseline"]).assert().success();
    repo
}

fn assert_stale(repo: &Repo) {
    let before = repo.read_model();
    repo.cli().arg("check").assert().code(1).stderr(
        predicate::str::contains("[release-model-stale]")
            .and(predicate::str::contains("intentional model")),
    );
    repo.cli().arg("status").assert().success().stdout(
        predicate::str::contains("[release-model-stale]")
            .and(predicate::str::contains("intentional model")),
    );
    let output = repo
        .cli()
        .args(["status", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(status["release_model_stale"], true);
    assert_eq!(
        repo.read_model(),
        before,
        "read-only commands must not repair"
    );
}

#[test]
fn check_and_status_accept_absent_and_fresh_models_in_dirty_working_tree() {
    let repo = checked_repo();
    for present in [false, true] {
        if present {
            repo.cli().arg("model").assert().success();
        }
        repo.cli().arg("check").assert().success();
        repo.cli()
            .arg("status")
            .assert()
            .success()
            .stdout(predicate::str::contains("release-model-stale").not());
        let output = repo
            .cli()
            .args(["status", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        let status: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(status["release_model_stale"], false);
        assert_eq!(repo.path().join(RELEASE_MODEL_PATH).exists(), present);
    }
    // A config edit and a freshly regenerated model are both uncommitted.
    let config = fs::read_to_string(repo.path().join(".intentional/config.yml")).unwrap();
    repo.write(
        ".intentional/config.yml",
        &config.replace("sample@", "library@"),
    );
    repo.cli().arg("model").assert().success();
    git(repo.path(), &["tag", "library@1.0.0"]);
    repo.cli().arg("check").assert().success();
}

#[test]
fn check_and_status_report_model_stale_after_working_tree_config_edit() {
    let repo = checked_repo();
    repo.cli().arg("model").assert().success();
    let config = fs::read_to_string(repo.path().join(".intentional/config.yml")).unwrap();
    repo.write(
        ".intentional/config.yml",
        &format!("{config}workspace-tags:\n  snapshot:\n    template: 'snapshot@{{version}}'\n"),
    );
    assert_stale(&repo);
    repo.cli().arg("model").assert().success();
    repo.cli().arg("check").assert().success();
}

#[test]
fn check_and_status_report_hand_edited_malformed_and_wrong_schema_models() {
    let repo = checked_repo();
    repo.cli().arg("model").assert().success();
    let fresh = String::from_utf8(repo.read_model()).unwrap();
    for content in [
        fresh.replace("sample-library", "edited-library"),
        "not JSON".to_owned(),
        fresh.replace("schemas/release-model/1", "schemas/release-model/999"),
        // Semantic JSON equality is insufficient: the contract compares bytes.
        format!("{fresh} "),
        fresh.replace("\"paths\": {", "\"paths\": {\"model\": 123,"),
    ] {
        repo.write(RELEASE_MODEL_PATH, &content);
        assert_stale(&repo);
    }
}

#[test]
fn check_accepts_fresh_model_in_second_clone_with_different_remote_and_default_branch() {
    let repo = checked_repo();
    repo.cli().arg("model").assert().success();
    git(repo.path(), &["add", "."]);
    git(repo.path(), &["commit", "-qm", "model"]);
    git(repo.path(), &["branch", "-M", "first"]);
    let clone_dir = tempfile::tempdir().unwrap();
    git(
        clone_dir.path(),
        &["clone", "-q", repo.path().to_str().unwrap(), "second"],
    );
    let clone = clone_dir.path().join("second");
    git(&clone, &["branch", "-M", "second"]);
    git(
        &clone,
        &[
            "remote",
            "set-url",
            "origin",
            "https://example.invalid/other",
        ],
    );
    git(
        &clone,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/second",
        ],
    );
    git(&clone, &["config", "init.defaultBranch", "second"]);
    let mut command = Command::new(assert_cmd::cargo::cargo_bin!("intentional"));
    command
        .arg("-C")
        .arg(&clone)
        .arg("check")
        .assert()
        .success();
    assert_eq!(
        repo.read_model(),
        fs::read(clone.join(RELEASE_MODEL_PATH)).unwrap()
    );
}

#[test]
fn check_and_status_report_stale_model_after_native_identity_edit() {
    let repo = checked_repo();
    repo.cli().arg("model").assert().success();
    repo.write(
        "package.json",
        r#"{"name":"renamed-library","version":"1.0.0"}"#,
    );
    assert_stale(&repo);
}

#[test]
fn check_and_status_preserve_model_read_errors() {
    let repo = checked_repo();
    fs::create_dir(repo.path().join(RELEASE_MODEL_PATH)).unwrap();
    for command in ["check", "status"] {
        repo.cli().arg(command).assert().code(1).stderr(
            predicate::str::contains("release-model.json")
                .and(predicate::str::contains("release-model-stale").not()),
        );
    }
    assert!(repo.path().join(RELEASE_MODEL_PATH).is_dir());
}
