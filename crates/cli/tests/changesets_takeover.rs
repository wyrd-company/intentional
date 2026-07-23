// ---
// relationships:
//   tests: intent-driven-polyglot-release
// ---

use assert_cmd::Command;
use intentional_core::{initialize, Adapter, CandidateResolution, InitPlan, InitState};
use predicates::prelude::*;
use std::collections::BTreeSet;
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

fn replace_path_references(root: &Path, relative: &Path, source_path: &str, target_path: &str) {
    let directory = root.join(relative);
    if !directory.is_dir() {
        return;
    }
    for entry in fs::read_dir(&directory).expect("fixture directory") {
        let entry = entry.expect("fixture entry");
        let child = relative.join(entry.file_name());
        if child.starts_with(".git")
            || child.starts_with(".changeset")
            || child.starts_with(".intentional")
            || child == Path::new(source_path)
        {
            continue;
        }
        if entry.file_type().expect("fixture type").is_dir() {
            replace_path_references(root, &child, source_path, target_path);
            continue;
        }
        let path = root.join(&child);
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        if text.contains(source_path) {
            fs::write(path, text.replace(source_path, target_path))
                .expect("replace proxy path reference");
        }
    }
}

fn resolved_fixture_copy(source: &Path, plan: &mut InitPlan) -> Repository {
    let repository = Repository::new();
    for path in [
        Path::new("package.json"),
        Path::new("pnpm-workspace.yaml"),
        Path::new("Cargo.toml"),
    ] {
        copy_file(source, &repository.root, path);
    }
    copy_tree(source, &repository.root, Path::new(".changeset"));
    for package in plan.inferred_config.release_units.values() {
        for projection in &package.projections {
            copy_file(
                source,
                &repository.root,
                &package.path.join(&projection.file),
            );
        }
    }
    for candidate in &plan.discovery_candidates {
        for evidence in &candidate.evidence {
            copy_file(source, &repository.root, &evidence.path);
        }
    }
    for diagnostic in &plan.diagnostics {
        for evidence in &diagnostic.evidence {
            copy_file(source, &repository.root, &evidence.path);
        }
    }
    for diagnostic in &mut plan.diagnostics {
        diagnostic.resolution = match diagnostic.code.as_str() {
            "ignored-release-unit-disposition" | "unmapped-release-unit-disposition" => {
                Some("excluded".to_owned())
            }
            "repository-integration" => Some("removed".to_owned()),
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
    resolve_discovery_candidates(plan);
    repository.write(
        ".intentional/init-plan.yml",
        &plan.to_yaml().expect("resolved init plan"),
    );
    repository
}

fn resolve_discovery_candidates(plan: &mut InitPlan) {
    for candidate in &mut plan.discovery_candidates {
        let disposition_excluded = candidate.native_identity.as_ref().is_some_and(|identity| {
            plan.diagnostics.iter().any(|diagnostic| {
                matches!(
                    diagnostic.code.as_str(),
                    "ignored-release-unit-disposition" | "unmapped-release-unit-disposition"
                ) && diagnostic.id.ends_with(&format!(":{identity}"))
                    && diagnostic.resolution.as_deref() == Some("excluded")
            })
        });
        if disposition_excluded {
            candidate.resolution = Some(CandidateResolution::Excluded);
            continue;
        }
        let release_unit = plan
            .inferred_config
            .release_units
            .iter()
            .find_map(|(id, unit)| {
                unit.projections
                    .iter()
                    .any(|projection| unit.path.join(&projection.file) == candidate.path)
                    .then(|| id.clone())
            });
        candidate.resolution = Some(match release_unit {
            Some(release_unit) => CandidateResolution::Projection {
                release_unit,
                target_candidate: None,
            },
            None => CandidateResolution::Excluded,
        });
    }
}

#[test]
fn changesets_resolution_materializes_a_devcontainer_projection() {
    let repository = Repository::new();
    repository.write("pnpm-workspace.yaml", "packages:\n  - components/*\n");
    repository.write(
        "components/library/package.json",
        "{\n  \"name\": \"sample-library\",\n  \"version\": \"1.0.0\"\n}\n",
    );
    repository.write(
        "components/library/devcontainer-feature.json",
        "{\n  \"id\": \"sample-feature\",\n  \"version\": \"1.0.0\"\n}\n",
    );
    repository.write(
        ".changeset/config.json",
        "{\n  \"changelog\": false,\n  \"commit\": false,\n  \"fixed\": [],\n  \"linked\": [],\n  \"access\": \"public\",\n  \"baseBranch\": \"main\",\n  \"updateInternalDependencies\": \"patch\",\n  \"ignore\": []\n}\n",
    );
    repository.write(
        ".changeset/sample-change.md",
        "---\n\"sample-library\": patch\n---\n\nCorrect a user-visible defect.\n",
    );
    git(&repository.root, &["add", "-A"]);
    git(
        &repository.root,
        &["commit", "-q", "-m", "add projection fixture"],
    );

    repository.cli().arg("init").assert().code(2);
    let plan_path = repository.root.join(".intentional/init-plan.yml");
    let mut plan: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).expect("initialization plan"))
            .expect("valid initialization plan");
    assert_eq!(plan.discovery_candidates.len(), 2);
    for candidate in &mut plan.discovery_candidates {
        candidate.resolution = Some(CandidateResolution::Projection {
            release_unit: "sample-library".to_owned(),
            target_candidate: None,
        });
    }
    fs::write(&plan_path, plan.to_yaml().expect("resolved plan")).expect("write resolved plan");

    let resolved = repository
        .cli()
        .arg("init")
        .output()
        .expect("resolved init");
    assert!(
        resolved.status.success(),
        "stdout: {}\nstderr: {}\nplan:\n{}",
        String::from_utf8_lossy(&resolved.stdout),
        String::from_utf8_lossy(&resolved.stderr),
        fs::read_to_string(&plan_path).expect("unresolved plan")
    );
    let ready: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).expect("ready plan"))
            .expect("valid ready plan");
    assert_eq!(ready.state, InitState::Ready);
    assert!(ready.inferred_config.release_units["sample-library"]
        .projections
        .iter()
        .any(|projection| {
            projection.file == Path::new("devcontainer-feature.json")
                && projection.pointer.as_deref() == Some("/version")
        }));
}

#[test]
fn repository_complete_discovery_resolves_duplicate_native_identities_before_takeover() {
    let repository = Repository::new();
    repository.write(
        "package.json",
        "{\n  \"name\": \"sample-library\",\n  \"version\": \"1.0.0\"\n}\n",
    );
    repository.write(
        "components/dart/pubspec.yaml",
        "name: sample_dart\nversion: 1.0.0\n",
    );
    repository.write(
        "fixtures/dart-copy/pubspec.yaml",
        "name: sample_dart\nversion: 2.0.0\n",
    );
    repository.write(
        "fixtures/alpha/pubspec.yaml",
        "name: sample_fixture\nversion: 1.0.0\n",
    );
    repository.write(
        "fixtures/beta/pubspec.yaml",
        "name: sample_fixture\nversion: 1.0.0\n",
    );
    repository.write(
        ".changeset/config.json",
        "{\n  \"changelog\": false,\n  \"commit\": false,\n  \"fixed\": [],\n  \"linked\": [],\n  \"access\": \"public\",\n  \"baseBranch\": \"main\",\n  \"updateInternalDependencies\": \"patch\",\n  \"ignore\": []\n}\n",
    );
    repository.write(
        ".changeset/sample-change.md",
        "---\n\"sample-library\": patch\n---\n\nCorrect a user-visible defect.\n",
    );
    git(&repository.root, &["add", "-A"]);
    git(
        &repository.root,
        &["commit", "-q", "-m", "add generic fixtures"],
    );

    repository.cli().arg("init").assert().code(2);
    let plan_path = repository.root.join(".intentional/init-plan.yml");
    let mut plan: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).expect("initialization plan"))
            .expect("valid initialization plan");
    assert!(plan
        .discovery_candidates
        .iter()
        .any(|candidate| candidate.path == Path::new("components/dart/pubspec.yaml")));
    assert_eq!(
        plan.discovery_candidates
            .iter()
            .filter(|candidate| candidate.native_identity.as_deref() == Some("sample_fixture"))
            .count(),
        2
    );
    assert_eq!(
        plan.discovery_candidates
            .iter()
            .filter(|candidate| candidate.native_identity.as_deref() == Some("sample_fixture"))
            .map(|candidate| &candidate.id)
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
    for candidate in &mut plan.discovery_candidates {
        candidate.resolution = Some(match candidate.path.to_string_lossy().as_ref() {
            "fixtures/alpha/pubspec.yaml"
            | "fixtures/beta/pubspec.yaml"
            | "fixtures/dart-copy/pubspec.yaml" => CandidateResolution::Excluded,
            _ => CandidateResolution::Projection {
                release_unit: "sample-library".to_owned(),
                target_candidate: None,
            },
        });
    }
    fs::write(&plan_path, plan.to_yaml().expect("resolved plan")).expect("write resolved plan");

    let resolved = repository
        .cli()
        .arg("init")
        .output()
        .expect("resolved init");
    assert!(
        resolved.status.success(),
        "stdout: {}\nstderr: {}\nplan:\n{}",
        String::from_utf8_lossy(&resolved.stdout),
        String::from_utf8_lossy(&resolved.stderr),
        fs::read_to_string(&plan_path).expect("unresolved plan")
    );
    let ready: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).expect("ready plan"))
            .expect("valid ready plan");
    assert_eq!(ready.state, InitState::Ready);
    assert_eq!(ready.parity.status, "equivalent");
    assert_eq!(
        ready.inferred_config.release_units["sample-library"]
            .projections
            .len(),
        2
    );
    assert_eq!(ready.inferred_config.discovery.excluded_paths.len(), 3);

    repository
        .cli()
        .args(["init", "--take-over"])
        .assert()
        .success();
    assert!(!repository.root.join(".changeset").exists());
    let completed = initialize(&repository.root, false).expect("repeatable initialization");
    assert_eq!(completed.state, InitState::Success);
    assert!(completed.operations.is_empty());
}

#[test]
fn structurally_unused_private_npm_identity_is_an_explicit_removal_choice() {
    let repository = Repository::new();
    repository.write(".gitignore", "generated/\n");
    repository.write(
        "components/feature/package.json",
        "{\n  \"name\": \"sample-placeholder\",\n  \"version\": \"1.0.0\",\n  \"private\": true,\n  \"description\": \"Words do not affect structural evidence.\"\n}\n",
    );
    repository.write(
        "components/feature/devcontainer-feature.json",
        "{\n  \"id\": \"sample-feature\",\n  \"version\": \"1.0.0\"\n}\n",
    );
    repository.write(
        ".changeset/config.json",
        "{\n  \"changelog\": false,\n  \"commit\": false,\n  \"fixed\": [],\n  \"linked\": [],\n  \"access\": \"public\",\n  \"baseBranch\": \"main\",\n  \"updateInternalDependencies\": \"patch\",\n  \"privatePackages\": { \"version\": true, \"tag\": true },\n  \"ignore\": []\n}\n",
    );
    repository.write(
        ".changeset/sample-change.md",
        "---\n\"sample-placeholder\": patch\n---\n\nCorrect a user-visible defect.\n",
    );
    repository.write(
        ".changeset/README.md",
        "---\nnotes:\n  - prose is not an intent\n---\n",
    );
    repository.write(
        "generated/reference.json",
        "{\"manifest\":\"components/feature/package.json\"}\n",
    );
    git(&repository.root, &["add", "-A"]);
    git(
        &repository.root,
        &["commit", "-q", "-m", "add generic fixtures"],
    );

    repository.cli().arg("init").assert().code(2);
    let plan_path = repository.root.join(".intentional/init-plan.yml");
    let mut plan: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).expect("initialization plan"))
            .expect("valid initialization plan");
    for candidate in &mut plan.discovery_candidates {
        candidate.resolution = Some(CandidateResolution::Projection {
            release_unit: "sample-feature".to_owned(),
            target_candidate: None,
        });
    }
    fs::write(&plan_path, plan.to_yaml().expect("resolved candidates"))
        .expect("write candidate resolutions");

    repository.cli().arg("init").assert().code(2);
    let mut assessed: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).expect("assessed plan"))
            .expect("valid assessed plan");
    let diagnostic = assessed
        .diagnostics
        .iter_mut()
        .find(|diagnostic| diagnostic.code == "release-tool-proxy-disposition")
        .expect("proxy assessment");
    assert_eq!(diagnostic.recommended.as_deref(), Some("remove"));
    assert!(!diagnostic.supporting_evidence.is_empty());
    assert!(diagnostic.contradictory_evidence.is_empty());
    assert!(diagnostic.uncertainty.is_some());
    diagnostic.resolution = Some("remove".to_owned());
    fs::write(&plan_path, assessed.to_yaml().expect("authorized removal"))
        .expect("write proxy resolution");

    repository.cli().arg("init").assert().success();
    let ready: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).expect("ready plan"))
            .expect("valid ready plan");
    let verified_removal = ready
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "release-tool-proxy-disposition")
        .expect("verified proxy removal");
    assert_eq!(verified_removal.resolution.as_deref(), Some("remove"));
    assert!(verified_removal.verified);
    assert!(ready
        .planned_operations
        .iter()
        .any(|operation| { operation == "delete components/feature/package.json" }));
    assert_eq!(
        ready.inferred_config.release_units["sample-feature"]
            .projections
            .len(),
        1
    );

    repository
        .cli()
        .args(["init", "--take-over"])
        .assert()
        .success();
    assert!(!repository
        .root
        .join("components/feature/package.json")
        .exists());
    assert!(repository
        .root
        .join("components/feature/devcontainer-feature.json")
        .exists());
}

#[test]
fn genuine_npm_projection_and_conflicting_proxy_evidence_do_not_recommend_removal() {
    let repository = Repository::new();
    repository.write("pnpm-workspace.yaml", "packages:\n  - components/*\n");
    repository.write(
        "components/library/package.json",
        "{\n  \"name\": \"sample-library\",\n  \"version\": \"1.0.0\",\n  \"exports\": { \".\": \"./index.js\" },\n  \"scripts\": { \"test\": \"node --test\" }\n}\n",
    );
    repository.write(
        "components/library/pubspec.yaml",
        "name: sample_dart\nversion: 1.0.0\n",
    );
    repository.write(
        "components/feature/package.json",
        "{\n  \"name\": \"sample-placeholder\",\n  \"version\": \"1.0.0\",\n  \"private\": true\n}\n",
    );
    repository.write(
        "components/feature/devcontainer-feature.json",
        "{\n  \"id\": \"sample-feature\",\n  \"version\": \"1.0.0\"\n}\n",
    );
    repository.write(
        "scripts/use-feature.mjs",
        "export const manifest = \"components/feature/package.json\";\n",
    );
    repository.write(
        ".changeset/config.json",
        "{\n  \"changelog\": false,\n  \"commit\": false,\n  \"fixed\": [],\n  \"linked\": [],\n  \"access\": \"public\",\n  \"baseBranch\": \"main\",\n  \"updateInternalDependencies\": \"patch\",\n  \"privatePackages\": { \"version\": true, \"tag\": true },\n  \"ignore\": []\n}\n",
    );
    repository.write(
        ".changeset/sample-change.md",
        "---\n\"sample-library\": patch\n\"sample-placeholder\": patch\n---\n\nCorrect two user-visible defects.\n",
    );
    git(&repository.root, &["add", "-A"]);
    git(
        &repository.root,
        &["commit", "-q", "-m", "add generic fixtures"],
    );

    repository.cli().arg("init").assert().code(2);
    let plan_path = repository.root.join(".intentional/init-plan.yml");
    let mut plan: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).expect("initialization plan"))
            .expect("valid initialization plan");
    for candidate in &mut plan.discovery_candidates {
        let release_unit = if candidate.path.starts_with("components/feature") {
            "sample-feature"
        } else {
            "sample-library"
        };
        candidate.resolution = Some(CandidateResolution::Projection {
            release_unit: release_unit.to_owned(),
            target_candidate: None,
        });
    }
    fs::write(&plan_path, plan.to_yaml().expect("resolved candidates"))
        .expect("write candidate resolutions");

    repository.cli().arg("init").assert().code(2);
    let mut assessed: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).expect("assessed plan"))
            .expect("valid assessed plan");
    assert!(!assessed.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "release-tool-proxy-disposition"
            && diagnostic
                .message
                .contains("components/library/package.json")
    }));
    assert_eq!(
        assessed.inferred_config.release_units["sample-library"]
            .projections
            .len(),
        2
    );
    let conflicting = assessed
        .diagnostics
        .iter_mut()
        .find(|diagnostic| {
            diagnostic.code == "release-tool-proxy-disposition"
                && diagnostic
                    .message
                    .contains("components/feature/package.json")
        })
        .expect("conflicting proxy assessment");
    assert!(conflicting.recommended.is_none());
    assert!(conflicting.resolution.is_none());
    assert!(!conflicting.contradictory_evidence.is_empty());
    conflicting.resolution = Some("retain".to_owned());
    fs::write(&plan_path, assessed.to_yaml().expect("retained proxy"))
        .expect("write retain resolution");

    repository.cli().arg("init").assert().success();
    let mut retained: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).expect("retained plan"))
            .expect("valid retained plan");
    let retained_proxy = retained
        .diagnostics
        .iter_mut()
        .find(|diagnostic| diagnostic.code == "release-tool-proxy-disposition")
        .expect("retained proxy assessment");
    assert_eq!(retained_proxy.resolution.as_deref(), Some("retain"));
    assert!(retained_proxy.verified);
    assert_eq!(
        retained.inferred_config.release_units["sample-feature"]
            .projections
            .len(),
        2
    );
    retained_proxy.resolution = Some("remove".to_owned());
    fs::write(&plan_path, retained.to_yaml().expect("requested removal"))
        .expect("write removal resolution");

    repository.cli().arg("init").assert().code(2);
    let blocked: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).expect("blocked plan"))
            .expect("valid blocked plan");
    let blocked_removal = blocked
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "release-tool-proxy-disposition")
        .expect("blocked proxy assessment");
    assert_eq!(blocked_removal.resolution.as_deref(), Some("remove"));
    assert!(!blocked_removal.verified);
    repository
        .cli()
        .args(["init", "--take-over"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "takeover requires a ready initialization plan",
        ));
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
    repository.write(
        ".changeset/README.md",
        "# Changesets\n\nThis folder contains pending release descriptions.\n",
    );
    git(&repository.root, &["add", "-A"]);
    git(&repository.root, &["commit", "-q", "-m", "add fixture"]);

    let interrupted = repository
        .root
        .join(".intentional/.takeover-transaction/original");
    fs::create_dir_all(&interrupted).unwrap();
    repository.cli().args(["init"]).assert().code(2);
    assert!(!repository
        .root
        .join(".intentional/.takeover-transaction")
        .exists());
    let plan_path = repository.root.join(".intentional/init-plan.yml");
    let mut plan: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).unwrap()).expect("init plan");
    assert_eq!(plan.state, InitState::NeedsInput);
    assert_eq!(plan.parity.status, "equivalent");
    assert_eq!(plan.parity.release_units.len(), 2);
    for diagnostic in &mut plan.diagnostics {
        if diagnostic.code == "repository-integration" {
            diagnostic.resolution = Some("removed".to_owned());
        } else if diagnostic.code == "unmapped-release-unit-disposition" {
            diagnostic.resolution = Some("excluded".to_owned());
        }
    }
    resolve_discovery_candidates(&mut plan);
    fs::write(&plan_path, plan.to_yaml().expect("render edited plan")).unwrap();
    repository.write(
        "package.json",
        "{\n  \"name\": \"fixture-root\",\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"]\n}\n",
    );

    repository.cli().arg("init").assert().code(2);
    let mut refreshed: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).expect("refreshed plan"))
            .expect("valid refreshed plan");
    for candidate in &mut refreshed.discovery_candidates {
        if candidate.path == Path::new("package.json") {
            candidate.resolution = Some(CandidateResolution::Excluded);
        }
    }
    for diagnostic in &mut refreshed.diagnostics {
        if diagnostic.code == "unmapped-release-unit-disposition" {
            diagnostic.resolution = Some("excluded".to_owned());
        }
    }
    fs::write(
        &plan_path,
        refreshed.to_yaml().expect("refreshed resolutions"),
    )
    .expect("write refreshed plan");
    repository.cli().arg("init").assert().success();
    let ready: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).unwrap()).expect("ready plan");
    assert_eq!(ready.state, InitState::Ready);
    assert!(ready
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "repository-integration")
        .all(|diagnostic| diagnostic.verified));

    let takeover = initialize(&repository.root, true).expect("planned takeover");
    let intent_path = repository.root.join(".changeset/useful-change.md");
    let original_intent = fs::read_to_string(&intent_path).unwrap();
    fs::write(
        &intent_path,
        format!("{original_intent}\nChanged after planning.\n"),
    )
    .unwrap();
    let stale = takeover
        .apply(&repository.root, false)
        .expect_err("stale takeover must fail");
    assert!(stale.to_string().contains("source evidence became stale"));
    fs::write(&intent_path, original_intent).unwrap();
    repository.write(
        ".changeset/added-after-planning.md",
        "---\n\"package-a\": patch\n---\n\nLate release intent.\n",
    );
    let added = takeover
        .apply(&repository.root, false)
        .expect_err("new source file must make takeover stale");
    assert!(added.to_string().contains("source evidence became stale"));
    fs::remove_file(repository.root.join(".changeset/added-after-planning.md")).unwrap();

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

    let interrupted = repository
        .root
        .join(".intentional/.takeover-transaction/original/.changeset");
    fs::create_dir_all(&interrupted).unwrap();
    fs::write(interrupted.join("config.json"), "stale backup").unwrap();
    repository.write(
        ".intentional/.takeover-transaction/manifest.yml",
        "- .intentional/config.yml\n- .changeset/config.json\n",
    );
    repository.write(".intentional/.takeover-state", "committed");
    let completed = initialize(&repository.root, false)
        .expect("completed takeover remains repeatably initialized");
    assert_eq!(completed.state, InitState::Success);
    assert!(completed.operations.is_empty());
    assert!(repository.root.join(".intentional/config.yml").is_file());
    assert!(!repository.root.join(".changeset").exists());
    assert!(!repository
        .root
        .join(".intentional/.takeover-transaction")
        .exists());
    assert!(!repository
        .root
        .join(".intentional/.takeover-state")
        .exists());

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
fn workspace_inventory_participates_in_changesets_dependency_propagation() {
    let repository = Repository::new();
    repository.write(
        "package.json",
        "{\n  \"name\": \"fixture-root\",\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"]\n}\n",
    );
    repository.write(
        "packages/package-a/package.json",
        "{\n  \"name\": \"package-a\",\n  \"version\": \"1.0.0\"\n}\n",
    );
    repository.write(
        "packages/package-b/package.json",
        "{\n  \"name\": \"package-b\",\n  \"version\": \"1.0.0\",\n  \"dependencies\": { \"package-a\": \"~1.0.0\" }\n}\n",
    );
    repository.write(
        ".changeset/config.json",
        r#"{
  "changelog": false,
  "commit": false,
  "fixed": [],
  "linked": [],
  "updateInternalDependencies": "patch",
  "ignore": []
}
"#,
    );
    repository.write(
        ".changeset/useful-change.md",
        "---\n\"package-a\": minor\n---\n\nAdd a useful capability.\n",
    );

    let output = repository
        .cli()
        .args(["init", "--dry-run", "--json"])
        .output()
        .expect("workspace parity plan");
    assert_eq!(output.status.code(), Some(2));
    let plan: InitPlan = serde_json::from_slice(&output.stdout).expect("structured plan");
    assert_eq!(plan.parity.status, "equivalent");
    assert!(plan.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "unmapped-release-unit-disposition"
            && diagnostic.id.ends_with(":fixture-root")
    }));
    assert!(plan
        .discovery_candidates
        .iter()
        .any(|candidate| candidate.path == Path::new("package.json")));
    let dependent = plan
        .parity
        .release_units
        .iter()
        .find(|package| package.release_unit == "package-b")
        .expect("dependency-propagated package");
    assert_eq!(dependent.source, dependent.proposed);
    assert_eq!(dependent.source.as_ref().unwrap().next_version, "1.0.1");
}

#[test]
fn changesets_dependency_ranges_and_peer_edges_are_independently_compared() {
    let repository = Repository::new();
    repository.write(
        "package.json",
        "{\n  \"name\": \"fixture-root\",\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"]\n}\n",
    );
    repository.write(
        "packages/package-a/package.json",
        "{\n  \"name\": \"package-a\",\n  \"version\": \"1.0.0\"\n}\n",
    );
    repository.write(
        "packages/package-b/package.json",
        "{\n  \"name\": \"package-b\",\n  \"version\": \"1.0.0\",\n  \"peerDependencies\": { \"package-a\": \"^1.0.0\" }\n}\n",
    );
    repository.write(
        "packages/package-c/package.json",
        "{\n  \"name\": \"package-c\",\n  \"version\": \"1.0.0\",\n  \"dependencies\": { \"package-a\": \"^1.0.0\" }\n}\n",
    );
    repository.write(
        ".changeset/config.json",
        r#"{
  "changelog": false,
  "commit": false,
  "fixed": [],
  "linked": [],
  "updateInternalDependencies": "patch",
  "ignore": []
}
"#,
    );
    repository.write(
        ".changeset/useful-change.md",
        "---\n\"package-a\": minor\n---\n\nAdd a useful capability.\n",
    );

    let output = repository
        .cli()
        .args(["init", "--dry-run", "--json"])
        .output()
        .expect("dependency parity plan");
    assert_eq!(output.status.code(), Some(2));
    let plan: InitPlan = serde_json::from_slice(&output.stdout).expect("structured plan");
    assert_eq!(plan.parity.status, "blocked");
    let peer = plan
        .parity
        .release_units
        .iter()
        .find(|package| package.release_unit == "package-b")
        .expect("peer dependent");
    assert_eq!(
        peer.source.as_ref().unwrap().requested_bump,
        intentional_core::Bump::Major
    );
    assert_eq!(
        peer.proposed.as_ref().unwrap().requested_bump,
        intentional_core::Bump::Patch
    );
    let in_range = plan
        .parity
        .release_units
        .iter()
        .find(|package| package.release_unit == "package-c")
        .expect("in-range dependent");
    assert!(in_range.source.is_none());
    assert_eq!(
        in_range.proposed.as_ref().unwrap().requested_bump,
        intentional_core::Bump::Patch
    );
}

#[test]
fn changesets_minor_internal_dependency_policy_is_preserved() {
    let repository = Repository::new();
    repository.write(
        "package.json",
        "{\n  \"name\": \"fixture-root\",\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"]\n}\n",
    );
    repository.write(
        "packages/package-a/package.json",
        "{\n  \"name\": \"package-a\",\n  \"version\": \"1.0.0\"\n}\n",
    );
    repository.write(
        "packages/package-b/package.json",
        "{\n  \"name\": \"package-b\",\n  \"version\": \"1.0.0\",\n  \"dependencies\": { \"package-a\": \"~1.0.0\" }\n}\n",
    );
    repository.write(
        ".changeset/config.json",
        r#"{
  "changelog": false,
  "commit": false,
  "fixed": [],
  "linked": [],
  "updateInternalDependencies": "minor",
  "ignore": []
}
"#,
    );
    repository.write(
        ".changeset/useful-change.md",
        "---\n\"package-a\": minor\n---\n\nAdd a useful capability.\n",
    );

    let output = repository
        .cli()
        .args(["init", "--dry-run", "--json"])
        .output()
        .expect("minor dependency policy plan");
    assert_eq!(output.status.code(), Some(2));
    let plan: InitPlan = serde_json::from_slice(&output.stdout).expect("structured plan");
    assert_eq!(plan.parity.status, "equivalent");
    let dependent = plan
        .parity
        .release_units
        .iter()
        .find(|package| package.release_unit == "package-b")
        .expect("dependency-propagated package");
    assert_eq!(dependent.source, dependent.proposed);
    assert_eq!(dependent.source.as_ref().unwrap().next_version, "1.1.0");
}

#[test]
fn conditional_peer_dependency_policy_is_an_actionable_takeover_choice() {
    let repository = Repository::new();
    repository.write(
        "package.json",
        "{\n  \"name\": \"fixture-root\",\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"]\n}\n",
    );
    repository.write(
        "packages/package-a/package.json",
        "{\n  \"name\": \"package-a\",\n  \"version\": \"1.0.0\"\n}\n",
    );
    repository.write(
        "packages/package-b/package.json",
        "{\n  \"name\": \"package-b\",\n  \"version\": \"1.0.0\",\n  \"peerDependencies\": { \"package-a\": \"^1.0.0\" }\n}\n",
    );
    repository.write(
        ".changeset/config.json",
        r#"{
  "changelog": false,
  "commit": false,
  "fixed": [],
  "linked": [],
  "updateInternalDependencies": "patch",
  "ignore": [],
  "___experimentalUnsafeOptions_WILL_CHANGE_IN_PATCH": {
    "onlyUpdatePeerDependentsWhenOutOfRange": true
  }
}
"#,
    );
    repository.write(
        ".changeset/useful-change.md",
        "---\n\"package-a\": minor\n---\n\nAdd a useful capability.\n",
    );

    let output = repository
        .cli()
        .args(["init", "--dry-run", "--json"])
        .output()
        .expect("conditional peer policy plan");
    assert_eq!(output.status.code(), Some(2));
    let plan: InitPlan = serde_json::from_slice(&output.stdout).expect("structured plan");
    let diagnostic = plan
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "changesets-peer-dependent-policy")
        .expect("actionable peer policy diagnostic");
    assert_eq!(diagnostic.choices, vec!["intentional"]);
    assert_eq!(diagnostic.recommended.as_deref(), Some("intentional"));
    let dependent = plan
        .parity
        .release_units
        .iter()
        .find(|package| package.release_unit == "package-b")
        .expect("peer dependent divergence");
    assert!(dependent.source.is_none());
    assert_eq!(
        dependent.proposed.as_ref().unwrap().requested_bump,
        intentional_core::Bump::Patch
    );
}

#[test]
fn dev_dependency_cycles_produce_an_actionable_plan() {
    let repository = Repository::new();
    repository.write(
        "package.json",
        "{\n  \"name\": \"fixture-root\",\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"]\n}\n",
    );
    repository.write(
        "packages/package-a/package.json",
        "{\n  \"name\": \"package-a\",\n  \"version\": \"1.0.0\",\n  \"devDependencies\": { \"package-b\": \"^1.0.0\" }\n}\n",
    );
    repository.write(
        "packages/package-b/package.json",
        "{\n  \"name\": \"package-b\",\n  \"version\": \"1.0.0\",\n  \"devDependencies\": { \"package-a\": \"^1.0.0\" }\n}\n",
    );
    repository.write(
        ".changeset/config.json",
        r#"{
  "changelog": false,
  "commit": false,
  "fixed": [],
  "linked": [],
  "updateInternalDependencies": "patch",
  "ignore": []
}
"#,
    );

    let output = repository
        .cli()
        .args(["init", "--dry-run", "--json"])
        .output()
        .expect("dev dependency cycle plan");
    assert_eq!(output.status.code(), Some(2));
    let plan: InitPlan = serde_json::from_slice(&output.stdout).expect("structured plan");
    let diagnostic = plan
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "changesets-dev-dependency-policy")
        .expect("actionable dev dependency diagnostic");
    assert_eq!(diagnostic.choices, vec!["intentional"]);
    assert_eq!(diagnostic.recommended.as_deref(), Some("intentional"));
}

fn write_cross_projection_fixture(repository: &Repository) {
    repository.write(
        "package.json",
        "{\n  \"name\": \"fixture-root\",\n  \"private\": true,\n  \"workspaces\": [\"packages\", \"packages/*\"]\n}\n",
    );
    repository.write(
        "packages/package.json",
        "{\n  \"name\": \"alpha\",\n  \"version\": \"1.0.0\"\n}\n",
    );
    repository.write(
        "packages/beta/package.json",
        "{\n  \"name\": \"beta\",\n  \"version\": \"1.0.0\"\n}\n",
    );
    repository.write(
        ".changeset/config.json",
        r#"{
  "changelog": false,
  "commit": false,
  "fixed": [],
  "linked": [],
  "updateInternalDependencies": "patch",
  "ignore": []
}
"#,
    );
    repository.write(
        ".changeset/useful-change.md",
        "---\n\"beta\": minor\n---\n\nAdd a useful capability.\n",
    );
}

#[test]
fn arbitrary_release_profile_content_cannot_change_inferred_configuration() {
    let repository = Repository::new();
    write_cross_projection_fixture(&repository);

    let baseline = repository
        .cli()
        .args(["init", "--dry-run", "--json"])
        .output()
        .expect("baseline plan");
    assert_eq!(baseline.status.code(), Some(2));
    let baseline: InitPlan = serde_json::from_slice(&baseline.stdout).expect("baseline plan");

    repository.write(
        "scripts/release-contract-profile.json",
        r#"{
  "packages": [
    {
      "name": "beta",
      "versionSource": "alpha",
      "tagBeforePublish": true,
      "publishAfter": ["alpha"],
      "versionProjections": ["arbitrary"]
    }
  ],
  "publicationOrder": ["beta", "alpha"]
}
"#,
    );

    let output = repository
        .cli()
        .args(["init", "--dry-run", "--json"])
        .output()
        .expect("plan with external release metadata");
    assert_eq!(output.status.code(), Some(2));
    let plan: InitPlan = serde_json::from_slice(&output.stdout).expect("structured plan");
    assert_eq!(plan.inferred_config, baseline.inferred_config);
    assert_eq!(plan.converted_intents, baseline.converted_intents);
    assert_eq!(plan.parity.status, "equivalent");
    assert_eq!(
        plan.converted_intents[0]
            .release_units
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["beta"]
    );
    assert!(plan.inferred_config.release_units.contains_key("alpha"));
    assert!(plan.inferred_config.release_units.contains_key("beta"));
    assert!(plan.inferred_config.workspace_tags.is_empty());
    let diagnostic = plan
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "external-release-evidence")
        .expect("external release evidence");
    assert_eq!(
        diagnostic.evidence[0].path,
        Path::new("scripts/release-contract-profile.json")
    );
}

#[test]
fn explicit_candidate_resolution_supplies_cross_projection_identity() {
    let repository = Repository::new();
    write_cross_projection_fixture(&repository);
    repository.cli().arg("init").assert().code(2);
    let plan_path = repository.root.join(".intentional/init-plan.yml");
    let mut plan: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).unwrap()).expect("initial plan");
    let candidate = plan
        .discovery_candidates
        .iter_mut()
        .find(|candidate| candidate.native_identity.as_deref() == Some("beta"))
        .expect("beta candidate");
    candidate.resolution = Some(CandidateResolution::Projection {
        release_unit: "alpha".to_owned(),
        target_candidate: None,
    });
    fs::write(&plan_path, plan.to_yaml().expect("resolved plan")).expect("write resolution");

    let output = repository
        .cli()
        .args(["init", "--dry-run", "--json"])
        .output()
        .expect("candidate-resolved plan");
    assert_eq!(output.status.code(), Some(2));
    let plan: InitPlan = serde_json::from_slice(&output.stdout).expect("structured plan");
    assert_eq!(plan.parity.status, "equivalent");
    assert_eq!(
        plan.converted_intents[0]
            .release_units
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        vec!["alpha"]
    );
    let release_unit = &plan.inferred_config.release_units["alpha"];
    assert_eq!(release_unit.projections.len(), 2);
    assert!(!plan.inferred_config.release_units.contains_key("beta"));
    assert!(plan.inferred_config.workspace_tags.is_empty());
}

#[test]
fn ignored_projected_identity_blocks_source_parity_without_dropping_target() {
    let repository = Repository::new();
    write_cross_projection_fixture(&repository);
    repository.write(
        ".changeset/config.json",
        r#"{
  "changelog": false,
  "commit": false,
  "fixed": [],
  "linked": [],
  "updateInternalDependencies": "patch",
  "ignore": ["beta"]
}
"#,
    );
    repository.cli().arg("init").assert().code(2);
    let plan_path = repository.root.join(".intentional/init-plan.yml");
    let mut plan: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).unwrap()).expect("initial plan");
    let candidate = plan
        .discovery_candidates
        .iter_mut()
        .find(|candidate| candidate.native_identity.as_deref() == Some("beta"))
        .expect("beta candidate");
    candidate.resolution = Some(CandidateResolution::Projection {
        release_unit: "alpha".to_owned(),
        target_candidate: None,
    });
    fs::write(&plan_path, plan.to_yaml().expect("resolved plan")).expect("write resolution");

    let output = repository
        .cli()
        .args(["init", "--dry-run", "--json"])
        .output()
        .expect("candidate-resolved ignored plan");
    assert_eq!(output.status.code(), Some(2));
    let plan: InitPlan = serde_json::from_slice(&output.stdout).expect("structured plan");
    assert_eq!(plan.parity.status, "blocked");
    assert!(plan.parity.release_units.is_empty());
    assert!(plan.inferred_config.release_units.contains_key("alpha"));
    assert!(!plan.inferred_config.release_units.contains_key("beta"));
    let ignored_diagnostics = plan
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "ignored-release-unit-disposition")
        .collect::<Vec<_>>();
    assert_eq!(ignored_diagnostics.len(), 1);
    assert_eq!(
        ignored_diagnostics[0].id,
        "ignored-release-unit-disposition:beta"
    );
    assert!(ignored_diagnostics[0]
        .message
        .contains("Changesets-ignored package beta"));
    assert!(!ignored_diagnostics[0]
        .message
        .contains("Changesets-ignored package alpha"));
    let diagnostic = plan
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "changesets-release-invalid")
        .expect("source authority blocker");
    assert!(diagnostic
        .message
        .contains("source parity cannot apply a package-scoped ignore"));
    assert!(diagnostic.message.contains("beta onto alpha"));
}

#[test]
fn private_package_suppression_and_suspension_are_actionable_parity_blockers() {
    let repository = Repository::new();
    repository.write(
        "package.json",
        "{\n  \"name\": \"fixture-root\",\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"]\n}\n",
    );
    repository.write(
        "packages/package-a/package.json",
        "{\n  \"name\": \"package-a\",\n  \"version\": \"0.1.0\",\n  \"private\": true\n}\n",
    );
    repository.write(
        ".changeset/config.json",
        r#"{
  "changelog": false,
  "commit": false,
  "fixed": [],
  "linked": [],
  "updateInternalDependencies": "patch",
  "ignore": [],
  "privatePackages": false
}
"#,
    );
    repository.write(
        ".changeset/useful-change.md",
        "---\n\"package-a\": minor\n---\n\nAdd a useful capability.\n",
    );

    let output = repository
        .cli()
        .args(["init", "--dry-run", "--json"])
        .output()
        .expect("private package plan");
    assert_eq!(output.status.code(), Some(2));
    let plan: InitPlan = serde_json::from_slice(&output.stdout).expect("structured plan");
    assert_eq!(plan.parity.status, "blocked");
    let package = plan
        .parity
        .release_units
        .iter()
        .find(|package| package.release_unit == "package-a")
        .expect("private package parity");
    assert!(package.source.is_none());
    assert_eq!(package.proposed.as_ref().unwrap().next_version, "0.2.0");

    repository.write(
        ".changeset/config.json",
        r#"{
  "changelog": false,
  "commit": false,
  "fixed": [],
  "linked": [],
  "updateInternalDependencies": "patch",
  "ignore": ["package-a"]
}
"#,
    );
    repository.cli().arg("init").assert().code(2);
    let plan_path = repository.root.join(".intentional/init-plan.yml");
    let mut plan: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).unwrap()).expect("initial plan");
    plan.diagnostics
        .iter_mut()
        .find(|diagnostic| diagnostic.code == "ignored-release-unit-disposition")
        .expect("ignored disposition")
        .resolution = Some("suspended".to_owned());
    fs::write(&plan_path, plan.to_yaml().unwrap()).expect("resolved plan");
    repository.cli().arg("init").assert().code(2);
    let rerun: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).unwrap()).expect("rerun plan");
    assert!(rerun.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "proposed-release-invalid"
            && diagnostic
                .message
                .contains("suspended release unit package-a")
    }));
}

#[test]
fn factual_diagnostic_verification_survives_init_rerun() {
    let repository = Repository::new();
    repository.write(
        "package.json",
        "{\n  \"name\": \"fixture-root\",\n  \"version\": \"0.1.0\",\n  \"private\": true\n}\n",
    );
    repository.write(
        ".changeset/config.json",
        r#"{
  "changelog": false,
  "commit": false,
  "fixed": [],
  "linked": [],
  "updateInternalDependencies": "patch",
  "ignore": [],
  "privatePackages": { "version": true, "tag": false }
}
"#,
    );

    let plan_path = repository.root.join(".intentional/init-plan.yml");
    for run in ["initial", "rerun"] {
        repository.cli().arg("init").assert().code(2);
        let plan: InitPlan = serde_yaml::from_str(&fs::read_to_string(&plan_path).unwrap())
            .unwrap_or_else(|error| panic!("{run} plan: {error}"));
        assert_eq!(plan.state, InitState::NeedsInput, "{run} readiness");

        let factual = plan
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "private-package-versioning")
            .unwrap_or_else(|| panic!("{run} factual diagnostic"));
        assert!(factual.choices.is_empty(), "{run} factual choices");
        assert!(factual.resolution.is_none(), "{run} factual resolution");
        assert!(factual.verified, "{run} factual verification");

        let unresolved = plan
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == "private-package-tagging")
            .unwrap_or_else(|| panic!("{run} choice-bearing diagnostic"));
        assert!(!unresolved.choices.is_empty(), "{run} unresolved choices");
        assert!(
            unresolved.resolution.is_none(),
            "{run} unresolved resolution"
        );
        assert!(!unresolved.verified, "{run} unresolved verification");
    }
}

#[test]
fn releasing_versionless_package_has_a_stable_parity_diagnostic() {
    let repository = Repository::new();
    repository.write(
        "package.json",
        "{\n  \"name\": \"fixture-root\",\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"]\n}\n",
    );
    repository.write(
        "packages/package-a/package.json",
        "{\n  \"name\": \"package-a\"\n}\n",
    );
    repository.write(
        ".changeset/config.json",
        r#"{
  "changelog": false,
  "commit": false,
  "fixed": [],
  "linked": [],
  "updateInternalDependencies": "patch",
  "ignore": []
}
"#,
    );
    repository.write(
        ".changeset/useful-change.md",
        "---\n\"package-a\": patch\n---\n\nCorrect a user-visible defect.\n",
    );

    let output = repository
        .cli()
        .args(["init", "--dry-run", "--json"])
        .output()
        .expect("versionless package plan");
    assert_eq!(output.status.code(), Some(2));
    let plan: InitPlan = serde_json::from_slice(&output.stdout).expect("structured plan");
    assert!(plan.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "proposed-release-invalid" && diagnostic.message.contains("package-a")
    }));
}

#[test]
#[ignore = "requires read-only Design System and catalog source fixtures"]
fn real_migration_fixtures_emit_repository_complete_plans_without_collisions() {
    for fixture in [
        Path::new("/workspaces/shared/the-wyrding-way/design-system"),
        Path::new("/workspaces/shared/the-wyrding-way/catalog"),
    ] {
        assert!(
            fixture.is_dir(),
            "required read-only migration fixture is unavailable: {}",
            fixture.display()
        );
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
        let plan: InitPlan = serde_json::from_slice(&output.stdout).expect("structured init plan");
        assert_eq!(plan.state, InitState::NeedsInput);
        assert!(!plan.converted_intents.is_empty());
        assert!(!plan
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("directory")));
        if fixture.ends_with("design-system") {
            assert!(plan.discovery_candidates.iter().any(|candidate| {
                candidate.path
                    == Path::new("packages/runtime/pub/the_wyrding_way_runtime/pubspec.yaml")
            }));
            assert_eq!(
                plan.discovery_candidates
                    .iter()
                    .filter(|candidate| {
                        candidate.native_identity.as_deref() == Some("wyrd_flutter")
                    })
                    .count(),
                2
            );
        } else {
            assert_eq!(plan.parity.status, "equivalent");
        }
    }
}

#[test]
#[ignore = "requires the read-only Design System source fixture"]
fn design_system_topology_rehearsal_resolves_and_removes_only_the_authorized_proxy() {
    let fixture = Path::new("/workspaces/shared/the-wyrding-way/design-system");
    assert!(
        fixture.is_dir(),
        "required read-only Design System fixture is unavailable: {}",
        fixture.display()
    );
    let output = Command::new(assert_cmd::cargo::cargo_bin!("intentional"))
        .arg("-C")
        .arg(fixture)
        .args(["init", "--dry-run", "--json"])
        .output()
        .expect("fixture init");
    assert_eq!(output.status.code(), Some(2));
    let mut source_plan: InitPlan =
        serde_json::from_slice(&output.stdout).expect("structured source plan");
    let converted = source_plan.converted_intents.clone();
    let repository = resolved_fixture_copy(fixture, &mut source_plan);
    replace_path_references(
        &repository.root,
        Path::new(""),
        "src/flutter/package.json",
        "src/flutter/devcontainer-feature.json",
    );

    let plan_path = repository.root.join(".intentional/init-plan.yml");
    let first = repository
        .cli()
        .arg("init")
        .output()
        .expect("reconcile disposable copy");
    assert!(
        matches!(first.status.code(), Some(0 | 2)),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let mut reconciled: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).expect("reconciled plan"))
            .expect("valid reconciled plan");
    resolve_discovery_candidates(&mut reconciled);
    for candidate in &mut reconciled.discovery_candidates {
        candidate.resolution = Some(match candidate.path.to_string_lossy().as_ref() {
            "packages/runtime/package.json"
            | "packages/runtime/pub/the_wyrding_way_runtime/pubspec.yaml" => {
                CandidateResolution::Projection {
                    release_unit: "@the-wyrding-way/runtime".to_owned(),
                    target_candidate: None,
                }
            }
            "src/flutter/package.json" | "src/flutter/devcontainer-feature.json" => {
                CandidateResolution::Projection {
                    release_unit: "flutter".to_owned(),
                    target_candidate: None,
                }
            }
            "fixtures/flutter/pubspec.yaml"
            | "fixtures/catalog-repo/ui/catalog/packages/flutter/pubspec.yaml" => {
                CandidateResolution::Excluded
            }
            _ => candidate
                .resolution
                .clone()
                .expect("generic fixture resolution"),
        });
    }
    for diagnostic in &mut reconciled.diagnostics {
        if diagnostic.resolution.is_none() {
            diagnostic.resolution = diagnostic.recommended.clone();
        }
    }
    fs::write(&plan_path, reconciled.to_yaml().expect("resolved topology"))
        .expect("write topology resolutions");

    let assessed_run = repository
        .cli()
        .arg("init")
        .output()
        .expect("assess proxy disposition");
    assert!(
        matches!(assessed_run.status.code(), Some(0 | 2)),
        "{}",
        String::from_utf8_lossy(&assessed_run.stderr)
    );
    let mut assessed: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).expect("assessed plan"))
            .expect("valid assessed plan");
    let proxy = assessed
        .diagnostics
        .iter_mut()
        .find(|diagnostic| {
            diagnostic.code == "release-tool-proxy-disposition"
                && diagnostic.message.contains("src/flutter/package.json")
        })
        .unwrap_or_else(|| {
            panic!(
                "feature proxy choice missing:\n{}",
                fs::read_to_string(&plan_path).expect("assessment plan")
            )
        });
    assert!(!proxy.supporting_evidence.is_empty());
    assert!(proxy.contradictory_evidence.is_empty());
    assert_eq!(proxy.recommended.as_deref(), Some("remove"));
    proxy.resolution = Some("remove".to_owned());
    for diagnostic in &mut assessed.diagnostics {
        if diagnostic.resolution.is_none() {
            diagnostic.resolution = diagnostic.recommended.clone();
        }
    }
    fs::write(&plan_path, assessed.to_yaml().expect("authorized topology"))
        .expect("write proxy authorization");

    let resolved = repository
        .cli()
        .arg("init")
        .output()
        .expect("resolved init");
    assert!(
        resolved.status.success(),
        "stdout: {}\nstderr: {}\nplan:\n{}",
        String::from_utf8_lossy(&resolved.stdout),
        String::from_utf8_lossy(&resolved.stderr),
        fs::read_to_string(&plan_path).expect("unresolved plan")
    );
    let ready: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&plan_path).expect("ready plan"))
            .expect("valid ready plan");
    assert_eq!(ready.state, InitState::Ready);
    assert_eq!(ready.parity.status, "equivalent");
    assert_eq!(ready.converted_intents, converted);
    assert!(ready
        .parity
        .release_units
        .iter()
        .all(|release| release.source == release.proposed));
    let runtime = &ready.inferred_config.release_units["@the-wyrding-way/runtime"];
    assert!(runtime
        .projections
        .iter()
        .any(|projection| projection.adapter == Adapter::Npm));
    assert!(runtime
        .projections
        .iter()
        .any(|projection| projection.adapter == Adapter::Pub));
    let feature = &ready.inferred_config.release_units["flutter"];
    assert_eq!(feature.projections.len(), 1);
    assert_eq!(
        feature.projections[0].file,
        Path::new("devcontainer-feature.json")
    );
    assert!(ready
        .planned_operations
        .iter()
        .any(|operation| operation == "delete src/flutter/package.json"));

    repository
        .cli()
        .args(["init", "--take-over"])
        .assert()
        .success();
    assert!(!repository.root.join("src/flutter/package.json").exists());
    assert!(repository
        .root
        .join("src/flutter/devcontainer-feature.json")
        .exists());
    assert!(repository
        .root
        .join("packages/runtime/pub/the_wyrding_way_runtime/pubspec.yaml")
        .exists());
    let repeated = initialize(&repository.root, false).expect("repeatable init");
    assert_eq!(repeated.state, InitState::Success);
    assert!(repeated.operations.is_empty());
}

#[test]
fn baseline_requires_agreeing_projection_evidence_and_explicit_tag_only_versions() {
    let repository = Repository::new();
    repository.write(
        ".intentional/config.yml",
        r#"contract: contract-1
release-units:
  package-a:
    path: .
    projections:
      - { adapter: npm, file: package.json, mode: committed }
      - { adapter: json, file: metadata.json, pointer: /version, mode: committed }
    tags:
      primary: { role: primary, template: 'package-a@{version}' }
      witness: { role: projection, template: 'witness@{version}' }
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
            "tag-only release unit package-b requires --version package-b=X.Y.Z",
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
    repository
        .cli()
        .args(["tag", "--baseline", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
    repository
        .cli()
        .args(["tag", "--baseline"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
    git(&repository.root, &["tag", "-d", "witness@1.0.0"]);
    repository
        .cli()
        .args(["tag", "--baseline", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "create annotated tag witness@1.0.0",
        ))
        .stdout(predicate::str::contains("package-a@1.0.0").not());
    repository
        .cli()
        .args(["tag", "--baseline"])
        .assert()
        .success();
    assert_eq!(
        git(&repository.root, &["cat-file", "-t", "witness@1.0.0"]),
        "tag"
    );
}

#[test]
fn phased_tags_are_created_only_by_matching_declarations_and_honor_tag_order() {
    let repository = Repository::new();
    repository.write(
        ".intentional/config.yml",
        r#"contract: contract-1
release-units:
  package-a:
    path: .
    projections:
      - { adapter: npm, file: package.json, mode: committed }
    tags:
      primary:
        role: primary
        template: 'package-a@{version}'
        require-phase: before-publication
      witness:
        role: projection
        template: 'witness@{version}'
        require-phase: before-publication
      mirror:
        role: projection
        template: 'mirror@{version}'
        require-phase: after-publication
        tag-after: [release-unit/package-a/primary]
"#,
    );
    repository.write(
        "package.json",
        "{\n  \"name\": \"package-a\",\n  \"version\": \"1.0.0\"\n}\n",
    );
    repository.write(
        ".intentional/intents/useful-capability.md",
        "---\npackage-a: minor\n---\n\nAdd a useful capability.\n",
    );
    git(&repository.root, &["add", "-A"]);
    git(
        &repository.root,
        &["commit", "-q", "-m", "add release intent"],
    );
    git(&repository.root, &["tag", "package-a@1.0.0"]);
    repository.cli().arg("apply").assert().success();
    git(&repository.root, &["add", "-A"]);
    git(
        &repository.root,
        &["commit", "-q", "-m", "apply release intent"],
    );
    repository.write("release-notes.md", "Release executor notes.\n");
    git(&repository.root, &["add", "release-notes.md"]);
    git(
        &repository.root,
        &["commit", "-q", "-m", "record executor notes"],
    );
    repository.write("release-audit.md", "Release executor audit.\n");
    git(&repository.root, &["add", "release-audit.md"]);
    git(
        &repository.root,
        &["commit", "-q", "-m", "record executor audit"],
    );

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
    repository
        .cli()
        .args(["tag", "--phase", "before-publication", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
    repository
        .cli()
        .args(["tag", "--phase", "before-publication"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
    repository
        .cli()
        .args(["tag", "--phase", "after-publication"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
    git(&repository.root, &["tag", "-d", "witness@1.1.0"]);
    repository
        .cli()
        .args(["tag", "--phase", "before-publication", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "create annotated tag witness@1.1.0",
        ))
        .stdout(predicate::str::contains("package-a@1.1.0").not());
    repository
        .cli()
        .args(["tag", "--phase", "before-publication"])
        .assert()
        .success();
    assert_eq!(
        git(&repository.root, &["cat-file", "-t", "witness@1.1.0"]),
        "tag"
    );
    repository.cli().arg("check").assert().success();
    git(&repository.root, &["tag", "-d", "witness@1.1.0"]);
    git(
        &repository.root,
        &[
            "tag",
            "-a",
            "witness@1.1.0",
            "-m",
            "intentional release record\n\ncontract: contract-1\ngenerator: intentional 0.1.4\nplan-digest: sha256:different\ntag-id: release-unit/package-a/witness\nversion: 1.1.0\nbaseline: false\n",
        ],
    );
    repository
        .cli()
        .args(["tag", "--phase", "before-publication"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "existing release tags at HEAD disagree on plan-digest",
        ));
    assert_eq!(
        git(&repository.root, &["cat-file", "-t", "mirror@1.1.0"]),
        "tag"
    );
}

#[test]
fn phased_multi_package_release_resumes_after_one_package_is_fully_tagged() {
    let repository = Repository::new();
    repository.write(
        ".intentional/config.yml",
        r#"contract: contract-1
release-units:
  package-a:
    path: packages/package-a
    projections:
      - { adapter: npm, file: package.json, mode: committed }
    tags:
      primary:
        role: primary
        template: 'package-a@{version}'
        require-phase: before-publication
  package-b:
    path: packages/package-b
    projections:
      - { adapter: npm, file: package.json, mode: committed }
    tags:
      primary:
        role: primary
        template: 'package-b@{version}'
        require-phase: after-publication
"#,
    );
    for id in ["package-a", "package-b"] {
        repository.write(
            &format!("packages/{id}/package.json"),
            &format!("{{\n  \"name\": \"{id}\",\n  \"version\": \"1.0.0\"\n}}\n"),
        );
    }
    repository.write(
        ".intentional/intents/useful-capability.md",
        "---\npackage-a: minor\npackage-b: minor\n---\n\nAdd a useful capability.\n",
    );
    git(&repository.root, &["add", "-A"]);
    git(
        &repository.root,
        &["commit", "-q", "-m", "add release intent"],
    );
    git(&repository.root, &["tag", "package-a@1.0.0"]);
    git(&repository.root, &["tag", "package-b@1.0.0"]);

    repository.cli().arg("apply").assert().success();
    git(&repository.root, &["add", "-A"]);
    git(
        &repository.root,
        &["commit", "-q", "-m", "apply release intent"],
    );
    repository
        .cli()
        .args(["tag", "--phase", "before-publication"])
        .assert()
        .success();
    assert_eq!(
        git(&repository.root, &["cat-file", "-t", "package-a@1.1.0"]),
        "tag"
    );
    repository
        .cli()
        .args(["tag", "--phase", "after-publication"])
        .assert()
        .success();
    assert_eq!(
        git(&repository.root, &["cat-file", "-t", "package-b@1.1.0"]),
        "tag"
    );
    repository.cli().arg("check").assert().success();
}
