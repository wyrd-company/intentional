// ---
// relationships:
//   tests: intent-driven-polyglot-release
// ---

//! Releases in which only a strict subset of release units moved.

use assert_cmd::Command;
use predicates::prelude::*;
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

    fn intent(&self, slug: &str, bumps: &[(&str, &str)], message: &str) {
        let frontmatter = bumps
            .iter()
            .map(|(id, bump)| format!("{id}: {bump}\n"))
            .collect::<String>();
        self.write(
            &format!(".intentional/intents/{slug}.md"),
            &format!("---\n{frontmatter}---\n\n{message}\n"),
        );
        self.commit(&format!("add intent {slug}"));
    }

    /// Plan, apply, commit, and tag one release; return the applied diff.
    fn release(&self, use_plan: bool) -> String {
        let plan = self.cli().arg("plan").output().expect("plan command");
        assert!(plan.status.success(), "plan failed: {plan:?}");
        self.cli().arg("apply").assert().success();
        let diff = git_raw(&self.root, &["diff", "--", "docs/docs.yml"]);
        self.commit("apply release");
        let mut tag = self.cli();
        tag.arg("tag");
        if use_plan {
            let path = self.root.join(".git/release-plan.json");
            fs::write(&path, &plan.stdout).expect("write sealed plan");
            tag.arg("--plan").arg(&path);
        }
        tag.assert().success();
        diff
    }

    fn tags(&self) -> Vec<String> {
        git(&self.root, &["tag", "--list"])
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn tag_target(&self, name: &str) -> String {
        git(&self.root, &["rev-parse", &format!("{name}^{{commit}}")])
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
    String::from_utf8(output.stdout).expect("UTF-8 git output")
}

fn yaml_unit(id: &str, path: &str, file: &str, pointer: &str, depends_on: &str) -> String {
    format!(
        "  {id}:\n    path: {path}\n{depends_on}    projections:\n      - adapter: yaml\n        file: {file}\n        pointer: {pointer}\n        mode: committed\n    tags:\n      primary:\n        role: primary\n        template: '{{id}}@{{version}}'\n"
    )
}

fn config(units: &[String]) -> String {
    format!(
        "$schema: https://intentional.foo/schemas/config.yml\ncontract: contract-2\nsettings:\n  internal-dependency-bump: patch\nrelease-units:\n{}",
        units.concat()
    )
}

/// Changed lines of a unified diff, without file headers.
fn changed_lines(diff: &str) -> Vec<&str> {
    diff.lines()
        .filter(|line| {
            (line.starts_with('+') || line.starts_with('-'))
                && !line.starts_with("+++")
                && !line.starts_with("---")
        })
        .collect()
}

/// Two independent release units at `.` and `docs` that project into one
/// shared `docs/docs.yml`, released together once and baselined at 0.1.0.
fn shared_yaml_repository() -> TestRepo {
    let repo = TestRepo::new();
    repo.write(
        ".intentional/config.yml",
        &config(&[
            yaml_unit("alpha", ".", "docs/docs.yml", "/products/alpha/version", ""),
            yaml_unit("beta", "docs", "docs.yml", "/products/beta/version", ""),
        ]),
    );
    repo.write(
        "docs/docs.yml",
        "products:\n  alpha:\n    version: 0.1.0\n  beta:\n    version: 0.1.0\n",
    );
    repo.write(".intentional/intents/.keep", "");
    repo.commit("add fixture");
    repo.cli().args(["tag", "--baseline"]).assert().success();
    repo.intent(
        "joint-release-0001",
        &[("alpha", "patch"), ("beta", "patch")],
        "Correct a shared defect.",
    );
    repo.release(false);
    assert!(repo.read("CHANGELOG.md").contains("## 0.1.1"));
    assert!(repo.read("docs/CHANGELOG.md").contains("## 0.1.1"));
    repo
}

#[test]
fn subset_release_into_a_shared_yaml_file_changes_and_tags_only_the_moved_unit() {
    let repo = shared_yaml_repository();
    let alpha_release = repo.tag_target("alpha@0.1.1");
    let alpha_changelog = repo.read("CHANGELOG.md");

    repo.intent(
        "beta-only-0002",
        &[("beta", "patch")],
        "Correct a beta defect.",
    );
    repo.cli()
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("alpha: 0.1.1 -> 0.1.1 (none)"))
        .stdout(predicate::str::contains("beta: 0.1.1 -> 0.1.2 (patch)"));
    let diff = repo.release(false);
    assert_eq!(
        changed_lines(&diff),
        ["-    version: 0.1.1", "+    version: 0.1.2"]
    );
    assert_eq!(
        repo.read("docs/docs.yml"),
        "products:\n  alpha:\n    version: 0.1.1\n  beta:\n    version: 0.1.2\n"
    );
    assert_eq!(repo.read("CHANGELOG.md"), alpha_changelog);
    let tags = repo.tags();
    assert!(tags.contains(&"beta@0.1.2".to_owned()));
    assert!(!tags.iter().any(|tag| tag == "alpha@0.1.2"));
    assert_eq!(repo.tag_target("alpha@0.1.1"), alpha_release);

    // The same unit releases again through the supplied-plan path while the
    // other unit stays put.
    repo.intent(
        "beta-only-0003",
        &[("beta", "minor")],
        "Add a beta capability.",
    );
    let diff = repo.release(true);
    assert_eq!(
        changed_lines(&diff),
        ["-    version: 0.1.2", "+    version: 0.1.3"]
    );
    assert!(repo.tags().contains(&"beta@0.1.3".to_owned()));
    assert_eq!(repo.tag_target("alpha@0.1.1"), alpha_release);
    assert_eq!(repo.read("CHANGELOG.md"), alpha_changelog);

    // A retry at the release commit is idempotent on both paths.
    let before = repo.tags();
    repo.cli()
        .arg("tag")
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
    repo.cli()
        .arg("tag")
        .arg("--plan")
        .arg(repo.root.join(".git/release-plan.json"))
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
    assert_eq!(repo.tags(), before);
    repo.cli()
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("Drift: none"));
}

#[test]
fn subset_release_still_refuses_a_plan_for_a_different_release_set() {
    let repo = shared_yaml_repository();
    repo.intent(
        "beta-only-0002",
        &[("beta", "patch")],
        "Correct a beta defect.",
    );
    let plan = repo.cli().arg("plan").output().expect("plan command");
    assert!(plan.status.success());
    repo.cli().arg("apply").assert().success();
    // Hand-advance the unmoved unit as if it were part of the release.
    repo.write(
        "docs/docs.yml",
        "products:\n  alpha:\n    version: 0.1.2\n  beta:\n    version: 0.1.2\n",
    );
    let changelog = repo.read("CHANGELOG.md");
    repo.write(
        "CHANGELOG.md",
        &changelog.replacen(
            "## 0.1.1",
            "## 0.1.2\n\n### Fixes\n\n- Unplanned.\n\n## 0.1.1",
            1,
        ),
    );
    repo.commit("apply release with an unplanned unit");
    let path = repo.root.join(".git/release-plan.json");
    fs::write(&path, &plan.stdout).expect("write sealed plan");
    repo.cli()
        .arg("tag")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "applied release does not match recovered release plan",
        ));
    repo.cli()
        .arg("tag")
        .arg("--plan")
        .arg(&path)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "applied release does not match recovered release plan",
        ));
    assert!(!repo.tags().iter().any(|tag| tag.ends_with("@0.1.2")));
}

#[test]
fn docs_unit_depending_on_its_code_unit_releases_alone() {
    let repo = TestRepo::new();
    repo.write(
        ".intentional/config.yml",
        &config(&[
            yaml_unit("alpha", ".", "docs/docs.yml", "/products/alpha/version", ""),
            yaml_unit(
                "alpha-docs",
                "docs",
                "docs.yml",
                "/products/alpha/docs-version",
                "    depends-on: [alpha]\n",
            ),
        ]),
    );
    repo.write(
        "docs/docs.yml",
        "products:\n  alpha:\n    version: 0.1.0\n    docs-version: 0.1.0\n",
    );
    repo.write(".intentional/intents/.keep", "");
    repo.commit("add fixture");
    repo.cli().args(["tag", "--baseline"]).assert().success();
    repo.intent(
        "joint-release-0001",
        &[("alpha", "minor"), ("alpha-docs", "minor")],
        "Add a capability and document it.",
    );
    repo.release(false);
    let code_release = repo.tag_target("alpha@0.1.1");
    let code_changelog = repo.read("CHANGELOG.md");

    repo.intent(
        "docs-only-0002",
        &[("alpha-docs", "patch")],
        "Clarify the guide.",
    );
    repo.cli()
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("alpha: 0.1.1 -> 0.1.1 (none)"))
        .stdout(predicate::str::contains(
            "alpha-docs: 0.1.1 -> 0.1.2 (patch)",
        ));
    let diff = repo.release(false);
    assert_eq!(
        changed_lines(&diff),
        ["-    docs-version: 0.1.1", "+    docs-version: 0.1.2"]
    );
    assert_eq!(
        repo.read("docs/docs.yml"),
        "products:\n  alpha:\n    version: 0.1.1\n    docs-version: 0.1.2\n"
    );
    assert_eq!(repo.read("CHANGELOG.md"), code_changelog);
    assert!(repo.read("docs/CHANGELOG.md").contains("## 0.1.2"));
    let tags = repo.tags();
    assert!(tags.contains(&"alpha-docs@0.1.2".to_owned()));
    assert!(!tags.iter().any(|tag| tag == "alpha@0.1.2"));
    assert_eq!(repo.tag_target("alpha@0.1.1"), code_release);

    // The code unit can then release on its own as well.
    repo.intent("code-only-0003", &[("alpha", "patch")], "Correct a defect.");
    let diff = repo.release(true);
    assert_eq!(
        changed_lines(&diff),
        ["-    version: 0.1.1", "+    version: 0.1.2"]
    );
    assert!(repo.tags().contains(&"alpha@0.1.2".to_owned()));
}

#[test]
fn recordless_primary_tag_at_head_is_still_refused() {
    let repo = shared_yaml_repository();
    repo.intent(
        "beta-only-0002",
        &[("beta", "patch")],
        "Correct a beta defect.",
    );
    repo.cli().arg("apply").assert().success();
    repo.commit("apply release");
    // A lightweight tag already occupies the moved unit's release version.
    git(&repo.root, &["tag", "beta@0.1.2"]);
    // It advances the unit's current version, so the recovered plan moves
    // past the applied tree and tagging fails closed.
    repo.cli()
        .arg("tag")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            r#"expected {"beta": "0.1.3"}, found {"beta": "0.1.2"}"#,
        ));
    assert!(!repo.tags().iter().any(|tag| tag == "alpha@0.1.2"));
}

fn record_field(repo: &TestRepo, tag: &str, field: &str) -> String {
    git(&repo.root, &["tag", "-l", "--format=%(contents)", tag])
        .lines()
        .find_map(|line| line.strip_prefix(&format!("{field}: ")).map(str::to_owned))
        .unwrap_or_else(|| panic!("tag {tag} has no {field} field"))
}

/// Add release unit `gamma` beside the released fixture units.
fn add_gamma(repo: &TestRepo) {
    repo.write(
        ".intentional/config.yml",
        &config(&[
            yaml_unit("alpha", ".", "docs/docs.yml", "/products/alpha/version", ""),
            yaml_unit("beta", "docs", "docs.yml", "/products/beta/version", ""),
            yaml_unit("gamma", "gamma", "gamma.yml", "/version", ""),
        ]),
    );
    repo.write("gamma/gamma.yml", "version: 0.0.0\n");
    repo.commit("add gamma release unit");
}

#[test]
fn baseline_for_a_unit_added_after_others_released_tags_only_that_unit() {
    let repo = shared_yaml_repository();
    let alpha_release = repo.tag_target("alpha@0.1.1");
    let beta_release = repo.tag_target("beta@0.1.1");
    add_gamma(&repo);
    let before = repo.tags();

    repo.cli()
        .args(["tag", "--baseline", "--dry-run"])
        .assert()
        .success()
        .stdout(predicate::str::contains("gamma@0.0.0"))
        .stdout(predicate::str::contains("alpha@").not())
        .stdout(predicate::str::contains("beta@").not());
    assert_eq!(repo.tags(), before);

    repo.cli().args(["tag", "--baseline"]).assert().success();
    let mut created = repo.tags();
    created.retain(|tag| !before.contains(tag));
    assert_eq!(created, ["gamma@0.0.0"]);
    assert_eq!(
        repo.tag_target("gamma@0.0.0"),
        git(&repo.root, &["rev-parse", "HEAD"])
    );
    assert_eq!(record_field(&repo, "gamma@0.0.0", "baseline"), "true");
    assert_eq!(repo.tag_target("alpha@0.1.1"), alpha_release);
    assert_eq!(repo.tag_target("beta@0.1.1"), beta_release);

    // A retry at the same commit is idempotent.
    repo.cli()
        .args(["tag", "--baseline"])
        .assert()
        .success()
        .stdout(predicate::str::is_empty());
    assert_eq!(repo.tags().len(), before.len() + 1);

    // The new unit then releases alone while the others stay put.
    repo.intent("gamma-first-0004", &[("gamma", "patch")], "Ship gamma.");
    repo.release(false);
    assert!(repo.tags().contains(&"gamma@0.0.1".to_owned()));
    assert!(!repo.tags().iter().any(|tag| tag == "alpha@0.1.2"));
    assert_eq!(repo.tag_target("alpha@0.1.1"), alpha_release);
}

#[test]
fn baseline_for_a_unit_added_after_an_earlier_baseline_tags_only_that_unit() {
    let repo = TestRepo::new();
    repo.write(
        ".intentional/config.yml",
        &config(&[yaml_unit(
            "alpha",
            ".",
            "docs/docs.yml",
            "/products/alpha/version",
            "",
        )]),
    );
    repo.write("docs/docs.yml", "products:\n  alpha:\n    version: 0.1.0\n");
    repo.write(".intentional/intents/.keep", "");
    repo.commit("add fixture");
    repo.cli().args(["tag", "--baseline"]).assert().success();
    let alpha_baseline = repo.tag_target("alpha@0.1.0");

    repo.write(
        ".intentional/config.yml",
        &config(&[
            yaml_unit("alpha", ".", "docs/docs.yml", "/products/alpha/version", ""),
            yaml_unit("gamma", "gamma", "gamma.yml", "/version", ""),
        ]),
    );
    repo.write("gamma/gamma.yml", "version: 0.0.0\n");
    repo.commit("add gamma release unit");
    repo.cli().args(["tag", "--baseline"]).assert().success();
    let mut tags = repo.tags();
    tags.sort();
    assert_eq!(tags, ["alpha@0.1.0", "gamma@0.0.0"]);
    assert_eq!(repo.tag_target("alpha@0.1.0"), alpha_baseline);
}

#[test]
fn baseline_with_every_unit_released_reports_nothing_to_baseline() {
    let repo = shared_yaml_repository();
    let before = repo.tags();
    repo.cli()
        .args(["tag", "--baseline"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nothing to baseline"));
    assert_eq!(repo.tags(), before);
}

#[test]
fn baseline_for_a_unit_added_later_leaves_an_established_workspace_tag_alone() {
    let repo = TestRepo::new();
    let alpha = yaml_unit("alpha", ".", "docs/docs.yml", "/products/alpha/version", "");
    let workspace = |units: &[String]| {
        config(units).replace(
            "release-units:",
            "workspace-tags:\n  release:\n    template: 'release-{version}'\nrelease-units:",
        )
    };
    repo.write(
        ".intentional/config.yml",
        &workspace(std::slice::from_ref(&alpha)),
    );
    repo.write("docs/docs.yml", "products:\n  alpha:\n    version: 0.1.0\n");
    repo.write(".intentional/intents/.keep", "");
    repo.commit("add fixture");
    repo.cli()
        .args(["tag", "--baseline", "--version", "workspace/release=1.0.0"])
        .assert()
        .success();
    let workspace_baseline = repo.tag_target("release-1.0.0");

    repo.write(
        ".intentional/config.yml",
        &workspace(&[
            alpha,
            yaml_unit("gamma", "gamma", "gamma.yml", "/version", ""),
        ]),
    );
    repo.write("gamma/gamma.yml", "version: 0.0.0\n");
    repo.commit("add gamma release unit");
    repo.cli().args(["tag", "--baseline"]).assert().success();
    let mut tags = repo.tags();
    tags.sort();
    assert_eq!(tags, ["alpha@0.1.0", "gamma@0.0.0", "release-1.0.0"]);
    assert_eq!(repo.tag_target("release-1.0.0"), workspace_baseline);
}

/// Rewrite annotated tag `tag` in place with `field` replaced by `value`.
fn forge_record_field(repo: &TestRepo, tag: &str, field: &str, value: &str) {
    let target = repo.tag_target(tag);
    let message = git(&repo.root, &["tag", "-l", "--format=%(contents)", tag])
        .lines()
        .map(|line| match line.strip_prefix(&format!("{field}: ")) {
            Some(_) => format!("{field}: {value}"),
            None => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    git(
        &repo.root,
        &["tag", "-f", "-a", tag, &target, "-m", &message],
    );
}

#[test]
fn baseline_refuses_an_established_record_with_a_foreign_identity() {
    for (field, value) in [
        ("tag-id", "release-unit/other/primary"),
        ("version", "8.8.8"),
        ("contract", "contract-other"),
        ("baseline", "maybe"),
    ] {
        let repo = shared_yaml_repository();
        forge_record_field(&repo, "alpha@0.1.1", field, value);
        add_gamma(&repo);
        let before = repo.tags();
        repo.cli()
            .args(["tag", "--baseline"])
            .assert()
            .failure()
            .stderr(predicate::str::contains(format!(
                "release tag alpha@0.1.1 has unexpected {field}"
            )));
        assert_eq!(repo.tags(), before, "{field} forgery created tags");
    }
}

#[test]
fn baseline_refuses_an_established_workspace_record_with_a_foreign_identity() {
    let repo = TestRepo::new();
    let alpha = yaml_unit("alpha", ".", "docs/docs.yml", "/products/alpha/version", "");
    let workspace = |units: &[String]| {
        config(units).replace(
            "release-units:",
            "workspace-tags:\n  release:\n    template: 'release-{version}'\nrelease-units:",
        )
    };
    repo.write(
        ".intentional/config.yml",
        &workspace(std::slice::from_ref(&alpha)),
    );
    repo.write("docs/docs.yml", "products:\n  alpha:\n    version: 0.1.0\n");
    repo.write(".intentional/intents/.keep", "");
    repo.commit("add fixture");
    repo.cli()
        .args(["tag", "--baseline", "--version", "workspace/release=1.0.0"])
        .assert()
        .success();
    forge_record_field(&repo, "release-1.0.0", "tag-id", "workspace/other");

    repo.write(
        ".intentional/config.yml",
        &workspace(&[
            alpha,
            yaml_unit("gamma", "gamma", "gamma.yml", "/version", ""),
        ]),
    );
    repo.write("gamma/gamma.yml", "version: 0.0.0\n");
    repo.commit("add gamma release unit");
    let before = repo.tags();
    repo.cli()
        .args(["tag", "--baseline"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "release tag release-1.0.0 has unexpected tag-id",
        ));
    assert_eq!(repo.tags(), before);
}
