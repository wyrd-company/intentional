// ---
// relationships:
//   tests: intent-driven-polyglot-release
// ---

use assert_cmd::Command;
use intentional_core::{InitPlan, InitState};
use predicates::prelude::*;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use tempfile::TempDir;

struct Repository {
    _temp: TempDir,
    root: PathBuf,
}

impl Repository {
    fn new() -> Self {
        let temp = tempfile::tempdir().expect("temporary directory");
        let root = temp.path().join("workspace");
        fs::create_dir(&root).expect("workspace directory");
        git(&root, &["init", "-q"]);
        git(&root, &["config", "user.name", "Fixture Author"]);
        git(&root, &["config", "user.email", "fixture@example.invalid"]);
        Self { _temp: temp, root }
    }

    fn write(&self, path: &str, contents: &str) {
        let path = self.root.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent");
        }
        fs::write(path, contents).expect("fixture write");
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

fn copy_file(source_root: &Path, target_root: &Path, relative: &Path) {
    let source = source_root.join(relative);
    if !source.is_file() {
        return;
    }
    let target = target_root.join(relative);
    fs::create_dir_all(target.parent().expect("copied file parent")).expect("copy parent");
    fs::copy(source, target).expect("copy fixture evidence");
}

fn copy_tree(source_root: &Path, target_root: &Path, relative: &Path) {
    let source = source_root.join(relative);
    if !source.is_dir() {
        return;
    }
    for entry in fs::read_dir(source).expect("fixture directory") {
        let entry = entry.expect("fixture entry");
        let child = relative.join(entry.file_name());
        if entry.file_type().expect("fixture type").is_dir() {
            copy_tree(source_root, target_root, &child);
        } else {
            copy_file(source_root, target_root, &child);
        }
    }
}

fn resolved_fixture_copy(source: &Path, plan: &mut InitPlan) -> Repository {
    let repository = Repository::new();
    for path in [
        Path::new("package.json"),
        Path::new("pnpm-workspace.yaml"),
        Path::new("Cargo.toml"),
        Path::new("scripts/release-contract-profile.json"),
    ] {
        copy_file(source, &repository.root, path);
    }
    copy_tree(source, &repository.root, Path::new(".changeset"));
    for package in plan.inferred_config.packages.values() {
        for projection in &package.projections {
            copy_file(
                source,
                &repository.root,
                &package.path.join(&projection.file),
            );
        }
    }
    for diagnostic in &plan.diagnostics {
        for evidence in &diagnostic.evidence {
            copy_file(source, &repository.root, &evidence.path);
        }
    }
    for diagnostic in &mut plan.diagnostics {
        diagnostic.resolution = match diagnostic.code.as_str() {
            "ignored-package-disposition" | "unmapped-package-disposition" => {
                Some("excluded".to_owned())
            }
            "repository-integration" => Some("removed".to_owned()),
            "repository-publication-sequencing" => Some("external".to_owned()),
            _ => diagnostic.recommended.clone(),
        };
        if diagnostic.code == "repository-integration" {
            let path = repository.root.join(&diagnostic.evidence[0].path);
            let text = fs::read_to_string(&path).expect("integration text");
            let edited = text
                .replace("Changesets", "Release_tool")
                .replace("changesets", "release_tool")
                .replace("Changeset", "Release_tool")
                .replace("changeset", "release_tool");
            fs::write(path, edited).expect("remove Changesets integration");
        }
    }
    repository.write(
        ".intentional/init-plan.yml",
        &plan.to_yaml().expect("resolved init plan"),
    );
    repository
}

#[test]
fn changesets_plan_reconciles_verified_edits_and_takes_over_atomically() {
    let repository = Repository::new();
    repository.write(
        "package.json",
        r#"{
  "name": "fixture-root",
  "private": true,
  "workspaces": ["packages/*"],
  "scripts": { "changeset": "changeset" },
  "devDependencies": { "@changesets/cli": "1.0.0" }
}
"#,
    );
    repository.write(
        "packages/package-a/package.json",
        "{\n  \"name\": \"package-a\",\n  \"version\": \"0.1.0\"\n}\n",
    );
    repository.write(
        "packages/package-b/package.json",
        "{\n  \"name\": \"package-b\",\n  \"version\": \"0.1.0\"\n}\n",
    );
    repository.write(
        ".changeset/config.json",
        r#"{
  "$schema": "https://unpkg.com/@changesets/config/schema.json",
  "changelog": false,
  "commit": false,
  "fixed": [["package-a", "package-b"]],
  "linked": [],
  "access": "public",
  "baseBranch": "main",
  "updateInternalDependencies": "patch",
  "ignore": []
}
"#,
    );
    repository.write(
        ".changeset/useful-change.md",
        "---\n\"package-a\": minor\n---\n\nAdd a useful capability.\n",
    );
    git(&repository.root, &["add", "-A"]);
    git(&repository.root, &["commit", "-q", "-m", "add fixture"]);

    repository.cli().args(["init"]).assert().code(2);
    let plan_path = repository.root.join(".intentional/init-plan.yml");
    let mut plan: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).unwrap()).expect("init plan");
    assert_eq!(plan.state, InitState::NeedsInput);
    assert_eq!(plan.parity.status, "equivalent");
    assert_eq!(plan.parity.packages.len(), 2);
    for diagnostic in &mut plan.diagnostics {
        if diagnostic.code == "repository-integration" {
            diagnostic.resolution = Some("removed".to_owned());
        }
    }
    fs::write(&plan_path, plan.to_yaml().expect("render edited plan")).unwrap();
    repository.write(
        "package.json",
        "{\n  \"name\": \"fixture-root\",\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"]\n}\n",
    );

    repository.cli().arg("init").assert().success();
    let ready: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).unwrap()).expect("ready plan");
    assert_eq!(ready.state, InitState::Ready);
    assert!(ready
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "repository-integration")
        .all(|diagnostic| diagnostic.verified));

    repository
        .cli()
        .args(["init", "--take-over"])
        .assert()
        .success();
    assert!(repository.root.join(".intentional/config.yml").is_file());
    assert!(!plan_path.exists());
    assert!(!repository.root.join(".changeset").exists());
    let converted = fs::read_to_string(
        repository
            .root
            .join(".intentional/intents/useful-change.md"),
    )
    .unwrap();
    assert!(converted.contains("package-a: minor"));
    assert!(converted.ends_with("Add a useful capability.\n"));

    git(&repository.root, &["add", "-A"]);
    git(
        &repository.root,
        &["commit", "-q", "-m", "take over release intent"],
    );
    repository
        .cli()
        .args(["tag", "--baseline"])
        .assert()
        .success();
    assert_eq!(
        git(&repository.root, &["cat-file", "-t", "package-a@0.1.0"]),
        "tag"
    );
    repository.cli().arg("check").assert().success();
}

#[test]
fn real_migration_fixtures_produce_parity_plans_without_directory_collisions() {
    for fixture in [
        Path::new("/workspaces/shared/the-wyrding-way/design-system"),
        Path::new("/workspaces/shared/the-wyrding-way/catalog"),
    ] {
        if !fixture.is_dir() {
            continue;
        }
        let output = Command::new(assert_cmd::cargo::cargo_bin!("intentional"))
            .arg("-C")
            .arg(fixture)
            .args(["init", "--dry-run", "--json"])
            .output()
            .expect("fixture init");
        assert_eq!(
            output.status.code(),
            Some(2),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut typed_plan: InitPlan =
            serde_json::from_slice(&output.stdout).expect("structured init plan");
        let plan: Value = serde_json::from_slice(&output.stdout).expect("structured init plan");
        assert_eq!(plan["state"], "needs-input");
        if fixture.ends_with("design-system") {
            assert_eq!(plan["parity"]["status"], "blocked");
            assert!(plan["parity"]["packages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|package| package["source"].is_null() && !package["proposed"].is_null()));
        } else {
            assert_eq!(plan["parity"]["status"], "equivalent");
            assert!(plan["parity"]["packages"]
                .as_array()
                .unwrap()
                .iter()
                .all(|package| package["source"] == package["proposed"]));
        }
        assert!(!plan["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["message"]
                .as_str()
                .is_some_and(|message| message.contains("directory"))));
        assert!(plan["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == "repository-integration"));
        assert!(!plan["converted-intents"].as_array().unwrap().is_empty());

        let converted = typed_plan.converted_intents.clone();
        let repository = resolved_fixture_copy(fixture, &mut typed_plan);
        let plan_path = repository.root.join(".intentional/init-plan.yml");
        let first_reconciliation = repository
            .cli()
            .arg("init")
            .output()
            .expect("reconcile edited evidence");
        assert_eq!(first_reconciliation.status.code(), Some(2));
        let mut reconciled: InitPlan =
            serde_yaml::from_str(&fs::read_to_string(&plan_path).unwrap())
                .expect("reconciled plan");
        assert!(reconciled
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.invalidated_resolution));
        for diagnostic in &mut reconciled.diagnostics {
            if diagnostic.resolution.is_none() {
                diagnostic.resolution = diagnostic.recommended.clone();
            }
        }
        fs::write(&plan_path, reconciled.to_yaml().unwrap()).expect("resolve stale evidence");
        let resolved_output = repository
            .cli()
            .arg("init")
            .output()
            .expect("resolved init");
        assert!(
            resolved_output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&resolved_output.stdout),
            fs::read_to_string(&plan_path).unwrap()
        );
        let ready: InitPlan = serde_yaml::from_str(&fs::read_to_string(&plan_path).unwrap())
            .expect("ready fixture plan");
        assert_eq!(ready.state, InitState::Ready);
        assert_eq!(ready.parity.status, "equivalent");
        assert_eq!(ready.converted_intents, converted);
        assert!(ready
            .parity
            .packages
            .iter()
            .all(|package| package.source == package.proposed));
    }
}

#[test]
fn baseline_requires_agreeing_projection_evidence_and_explicit_tag_only_versions() {
    let repository = Repository::new();
    repository.write(
        ".intentional/config.yml",
        r#"contract: contract-1
packages:
  package-a:
    path: .
    projections:
      - { adapter: npm, file: package.json, mode: committed }
      - { adapter: json, file: metadata.json, pointer: /version, mode: committed }
    tags:
      primary: { role: primary, template: 'package-a@{version}' }
  package-b:
    path: package-b
    tags:
      primary: { role: primary, template: 'package-b@{version}' }
"#,
    );
    repository.write(
        "package.json",
        "{\n  \"name\": \"package-a\",\n  \"version\": \"1.0.0\"\n}\n",
    );
    repository.write("metadata.json", "{\n  \"version\": \"1.1.0\"\n}\n");
    git(&repository.root, &["add", "-A"]);
    git(&repository.root, &["commit", "-q", "-m", "add fixture"]);

    repository
        .cli()
        .args(["tag", "--baseline"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "baseline projections disagree for package-a",
        ));
    repository.write("metadata.json", "{\n  \"version\": \"1.0.0\"\n}\n");
    git(&repository.root, &["add", "metadata.json"]);
    git(
        &repository.root,
        &["commit", "-q", "-m", "align projections"],
    );
    repository
        .cli()
        .args(["tag", "--baseline"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "tag-only package package-b requires --version package-b=X.Y.Z",
        ));
    repository
        .cli()
        .args(["tag", "--baseline", "--version", "package-b=2.0.0"])
        .assert()
        .success();
    assert_eq!(
        git(&repository.root, &["cat-file", "-t", "package-b@2.0.0"]),
        "tag"
    );
}

#[test]
fn phased_tags_are_created_only_by_matching_declarations_and_honor_tag_order() {
    let repository = Repository::new();
    repository.write(
        ".intentional/config.yml",
        r#"contract: contract-1
packages:
  package-a:
    path: .
    projections:
      - { adapter: npm, file: package.json, mode: committed }
    tags:
      primary:
        role: primary
        template: 'package-a@{version}'
        require-phase: before-publication
      mirror:
        role: projection
        template: 'mirror@{version}'
        require-phase: after-publication
        tag-after: [package/package-a/primary]
"#,
    );
    repository.write(
        "package.json",
        "{\n  \"name\": \"package-a\",\n  \"version\": \"1.1.0\"\n}\n",
    );
    repository.write("CHANGELOG.md", "# Changelog\n\n## 1.1.0\n\nNotes.\n");
    git(&repository.root, &["add", "-A"]);
    git(
        &repository.root,
        &["commit", "-q", "-m", "add applied fixture"],
    );
    git(&repository.root, &["tag", "package-a@1.0.0"]);

    repository
        .cli()
        .arg("tag")
        .assert()
        .failure()
        .stderr(predicate::str::contains("no unphased release tags"));
    repository
        .cli()
        .args(["tag", "--phase", "after-publication"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("missing prerequisite tag"));
    repository
        .cli()
        .args(["tag", "--phase", "before-publication"])
        .assert()
        .success();
    repository
        .cli()
        .args(["tag", "--phase", "after-publication"])
        .assert()
        .success();
    assert_eq!(
        git(&repository.root, &["cat-file", "-t", "mirror@1.1.0"]),
        "tag"
    );
    repository.cli().arg("check").assert().success();
}
