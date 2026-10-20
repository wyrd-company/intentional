// ---
// relationships:
//   tests: release-planning-and-records
// ---

//! The release protocol from plan to tags: the members a plan carries, phase
//! selection, tag rendering, recovery without the plan, and idempotent reruns.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use tempfile::TempDir;

const RELEASE_PLAN_ID: &str = "https://intentional.foo/schemas/0.3/release-plan";

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

    fn cli(&self) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("intentional"));
        command.arg("-C").arg(&self.root);
        command
    }

    fn plan(&self, args: &[&str]) -> Value {
        let output = self
            .cli()
            .arg("plan")
            .args(args)
            .output()
            .expect("plan command");
        assert!(
            output.status.success(),
            "plan failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("plan JSON")
    }

    /// Plan, apply, and commit one release; return the emitted plan.
    fn apply_release(&self, args: &[&str]) -> Value {
        let plan = self.plan(args);
        self.cli().arg("apply").args(args).assert().success();
        self.commit("apply release");
        plan
    }

    fn tags(&self) -> Vec<String> {
        git(&self.root, &["tag", "--list"])
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn record_field(&self, tag: &str, field: &str) -> String {
        let object = git(&self.root, &["cat-file", "tag", tag]);
        let prefix = format!("{field}: ");
        object
            .lines()
            .find_map(|line| line.strip_prefix(&prefix))
            .unwrap_or_else(|| panic!("{tag} has no {field}"))
            .to_owned()
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

/// One Release Unit with an unphased primary tag and a projection tag after
/// it, and a workspace tag that requires `after-publication` after the
/// primary tag. The unit stands at 1.2.3 with one pending minor intent.
fn release_repo(extra_tags: &str) -> TestRepo {
    let repo = TestRepo::new();
    repo.write(
        "package.json",
        "{\n  \"name\": \"sample-library\",\n  \"version\": \"1.2.3\"\n}\n",
    );
    repo.write(
        ".intentional/config.yml",
        &format!(
            r#"contract: contract-1
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
        template: '{{id}}@{{version}}'
      mirror:
        role: projection
        template: 'mirror@{{version}}'
        tag-after:
          - release-unit/sample-library/primary
{extra_tags}workspace-tags:
  release:
    template: '{{version}}'
    require-phase: after-publication
    tag-after:
      - release-unit/sample-library/primary
"#
        ),
    );
    repo.write(".intentional/intents/.gitkeep", "");
    repo.commit("add fixture");
    git(&repo.root, &["tag", "sample-library@1.2.3"]);
    repo.write(
        ".intentional/intents/sample-otter-0001.md",
        "---\nsample-library: minor\n---\n\nAdd a sample capability.\n",
    );
    repo.commit("add intent");
    repo
}

#[test]
fn emitted_plan_carries_every_release_plan_member() {
    let repo = release_repo("");
    let plan = repo.plan(&["--channel", "rc"]);

    let mut members: Vec<&str> = plan
        .as_object()
        .expect("plan object")
        .keys()
        .map(String::as_str)
        .collect();
    members.sort_unstable();
    assert_eq!(
        members,
        [
            "$schema",
            "channel",
            "contract",
            "digest",
            "generator",
            "release_units",
            "source",
            "tag_order",
            "tags"
        ]
    );
    assert_eq!(plan["$schema"], RELEASE_PLAN_ID);
    assert_eq!(
        plan["generator"],
        json!({ "tool": "intentional", "version": env!("CARGO_PKG_VERSION") })
    );
    assert_eq!(
        plan["source"],
        json!({
            "commit": git(&repo.root, &["rev-parse", "HEAD"]),
            "tree": git(&repo.root, &["rev-parse", "HEAD^{tree}"]),
        })
    );
    assert_eq!(plan["channel"], "rc");
    assert_eq!(
        plan["release_units"],
        json!([{
            "id": "sample-library",
            "old_version": "1.2.3",
            "new_version": "1.3.0-rc.1",
            "bump": "minor",
            "contributing_intent_ids": ["sample-otter-0001"],
            "tag_ids": [
                "release-unit/sample-library/mirror",
                "release-unit/sample-library/primary"
            ],
            "release_notes": "## 1.3.0-rc.1\n\n### Features\n\n- Add a sample capability.\n"
        }])
    );
    // `require_phase` and `tag_after` appear exactly where configured; a
    // workspace tag carries no Release Unit or role.
    assert_eq!(
        plan["tags"],
        json!([
            {
                "id": "release-unit/sample-library/mirror",
                "name": "mirror@1.3.0-rc.1",
                "version": "1.3.0-rc.1",
                "release_unit": "sample-library",
                "role": "projection",
                "tag_after": ["release-unit/sample-library/primary"]
            },
            {
                "id": "release-unit/sample-library/primary",
                "name": "sample-library@1.3.0-rc.1",
                "version": "1.3.0-rc.1",
                "release_unit": "sample-library",
                "role": "primary"
            },
            {
                "id": "workspace/release",
                "name": "0.0.1-rc.1",
                "version": "0.0.1-rc.1",
                "require_phase": "after-publication",
                "tag_after": ["release-unit/sample-library/primary"]
            }
        ])
    );
    assert_eq!(
        plan["tag_order"],
        json!([
            "release-unit/sample-library/primary",
            "release-unit/sample-library/mirror",
            "workspace/release"
        ])
    );
}

#[test]
fn tag_without_a_phase_creates_only_tags_that_require_none() {
    let repo = release_repo("");
    repo.apply_release(&[]);
    repo.cli()
        .arg("tag")
        .assert()
        .success()
        .stdout("create annotated tag sample-library@1.3.0\ncreate annotated tag mirror@1.3.0\n");
    assert!(!repo.tags().contains(&"0.0.1".to_owned()));
}

#[test]
fn tag_with_a_phase_creates_only_tags_that_require_it() {
    let repo = release_repo("");
    repo.apply_release(&[]);
    let before = repo.tags();
    repo.cli()
        .args(["tag", "--phase", "before-publication"])
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "no release tags require --phase before-publication",
        ));
    assert_eq!(repo.tags(), before);

    repo.cli().arg("tag").assert().success();
    repo.cli()
        .args(["tag", "--phase", "after-publication"])
        .assert()
        .success()
        .stdout("create annotated tag 0.0.1\n");
}

#[test]
fn projection_tags_render_their_v_prefix_and_other_tags_stay_bare() {
    let repo = release_repo(
        "      go:\n        role: projection\n        template: 'v{version}'\n      module:\n        role: projection\n        template: 'modules/sample/v{version}'\n",
    );
    let plan = repo.apply_release(&[]);
    let names: Vec<&str> = plan["tags"]
        .as_array()
        .expect("tags")
        .iter()
        .map(|tag| tag["name"].as_str().expect("tag name"))
        .collect();
    assert_eq!(
        names,
        [
            "v1.3.0",
            "mirror@1.3.0",
            "modules/sample/v1.3.0",
            "sample-library@1.3.0",
            "0.0.1"
        ]
    );
    repo.cli().arg("tag").assert().success();
    repo.cli()
        .args(["tag", "--phase", "after-publication"])
        .assert()
        .success();
    for name in names {
        assert_eq!(git(&repo.root, &["cat-file", "-t", name]), "tag", "{name}");
    }
}

#[test]
fn tags_recovered_after_later_commits_record_the_emitted_plan_digest() {
    for args in [&[][..], &["--channel", "rc"][..]] {
        let repo = release_repo("");
        let plan = repo.apply_release(args);
        repo.write("release-notes.md", "Release executor notes.\n");
        repo.commit("record executor notes");

        repo.cli().arg("tag").args(args).assert().success();
        let primary = plan["tags"][1]["name"].as_str().expect("primary tag name");
        assert_eq!(
            repo.record_field(primary, "plan-digest"),
            plan["digest"].as_str().expect("digest"),
            "{args:?}"
        );
    }
}

#[test]
fn rerun_after_partial_tag_creation_creates_only_the_remaining_tags() {
    let repo = release_repo("");
    let plan = repo.apply_release(&[]);
    fs::write(
        repo.root.join(".git/release-plan.json"),
        serde_json::to_string(&plan).expect("plan JSON"),
    )
    .expect("write plan");
    repo.cli().arg("tag").assert().success();
    let primary_object = git(&repo.root, &["rev-parse", "sample-library@1.3.0"]);
    git(&repo.root, &["tag", "-d", "mirror@1.3.0"]);

    // The unit's leading changelog section equals its tag on the commit being
    // tagged, so it stays in the release.
    for plan_args in [&[][..], &["--plan", ".git/release-plan.json"][..]] {
        repo.cli()
            .arg("tag")
            .args(plan_args)
            .arg("--dry-run")
            .assert()
            .success()
            .stdout("create annotated tag mirror@1.3.0\n");
    }
    repo.cli()
        .args(["tag", "--plan", ".git/release-plan.json"])
        .assert()
        .success()
        .stdout("create annotated tag mirror@1.3.0\n");
    repo.cli()
        .args(["tag", "--phase", "after-publication"])
        .assert()
        .success()
        .stdout("create annotated tag 0.0.1\n");
    assert_eq!(
        git(&repo.root, &["rev-parse", "sample-library@1.3.0"]),
        primary_object
    );
    for name in ["mirror@1.3.0", "0.0.1"] {
        assert_eq!(
            repo.record_field(name, "plan-digest"),
            plan["digest"].as_str().expect("digest")
        );
    }
}

#[test]
fn an_existing_tag_with_another_target_is_a_conflict_and_stays_unchanged() {
    let repo = release_repo("");
    repo.apply_release(&[]);
    git(
        &repo.root,
        &[
            "tag",
            "-a",
            "mirror@1.3.0",
            "HEAD^",
            "-m",
            "intentional release record\n\ncontract: contract-1\ngenerator: intentional 0.2.0\nplan-digest: sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd\ntag-id: release-unit/sample-library/mirror\nversion: 1.3.0\nbaseline: false\n",
        ],
    );
    let before = git(&repo.root, &["rev-parse", "mirror@1.3.0"]);

    repo.cli()
        .arg("tag")
        .assert()
        .code(1)
        .stderr(predicate::str::contains(
            "mirror@1.3.0 targets a different commit",
        ));
    assert_eq!(git(&repo.root, &["rev-parse", "mirror@1.3.0"]), before);
    assert!(!repo.tags().contains(&"sample-library@1.3.0".to_owned()));
}
