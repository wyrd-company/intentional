// ---
// relationships:
//   tests: intent-driven-polyglot-release
// ---

//! Projection files that several release units share.

use assert_cmd::Command;
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

/// One release unit with one yaml projection.
fn yaml_unit(id: &str, path: &str, file: &str, pointer: &str, mode: &str) -> String {
    format!(
        "  {id}:\n    path: {path}\n    projections:\n      - adapter: yaml\n        file: {file}\n        pointer: {pointer}\n        mode: {mode}\n    tags:\n      primary:\n        role: primary\n        template: '{{id}}@{{version}}'\n"
    )
}

fn config(units: &[String]) -> String {
    format!(
        "$schema: https://intentional.foo/schemas/config.yml\ncontract: contract-1\nsettings:\n  internal-dependency-bump: patch\n  pre-1-0-bump-mapping: compatibility\nrelease-units:\n{}",
        units.concat()
    )
}

#[test]
fn stamp_merges_equivalent_paths_in_shared_file() {
    let repo = TestRepo::new();
    repo.write(
        ".intentional/config.yml",
        &config(&[
            yaml_unit("alpha", ".", "site/meta.yml", "/alpha", "injected"),
            yaml_unit("beta", "site", "meta.yml", "/beta", "injected"),
        ]),
    );
    repo.write("site/meta.yml", "alpha: 0.0.0\nbeta: 0.0.0\n");
    repo.write(
        ".intentional/intents/joint-0001.md",
        "---\nalpha: patch\nbeta: patch\n---\n\nShip a shared change.\n",
    );
    repo.commit("add fixture");

    repo.cli()
        .args(["stamp", "--dry-run"])
        .assert()
        .success()
        .stdout("write site/meta.yml\n");
    repo.cli().arg("stamp").assert().success();
    assert_eq!(repo.read("site/meta.yml"), "alpha: 0.0.1\nbeta: 0.0.1\n");
}
