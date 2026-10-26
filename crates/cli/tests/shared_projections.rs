// ---
// relationships:
//   tests: intent-driven-polyglot-release
// ---

//! Projection files that several release units share.

use assert_cmd::Command;
use intentional_core::{initialize, CandidateResolution, InitPlan, InitState};
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

    fn intent(&self, slug: &str, bumps: &[(&str, &str)]) {
        let frontmatter = bumps
            .iter()
            .map(|(id, bump)| format!("{id}: {bump}\n"))
            .collect::<String>();
        self.write(
            &format!(".intentional/intents/{slug}.md"),
            &format!("---\n{frontmatter}---\n\nShip a change.\n"),
        );
        self.commit(&format!("add intent {slug}"));
    }

    fn tags(&self) -> Vec<String> {
        git(&self.root, &["tag", "--list"])
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The `write` lines that a command prints, in order.
    fn writes(&self, args: &[&str]) -> Vec<String> {
        let output = self.cli().args(args).output().expect("run intentional");
        assert!(output.status.success(), "{args:?} failed: {output:?}");
        String::from_utf8(output.stdout)
            .expect("UTF-8 output")
            .lines()
            .filter(|line| line.starts_with("write "))
            .map(str::to_owned)
            .collect()
    }

    fn cli(&self) -> Command {
        let mut command = Command::new(assert_cmd::cargo::cargo_bin!("intentional"));
        command.arg("-C").arg(&self.root);
        command
    }
}

/// `text` with each `/` replaced by the platform separator, as output
/// prints workspace paths.
fn native(text: &str) -> String {
    text.replace('/', std::path::MAIN_SEPARATOR_STR)
}

/// Make `link` a symlink to the file `target`, relative to the link.
fn symlink_file(target: &str, link: &Path) {
    if let Some(parent) = link.parent() {
        fs::create_dir_all(parent).expect("link parent");
    }
    let target = native(target);
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("file symlink");
    #[cfg(windows)]
    std::os::windows::fs::symlink_file(target, link).expect("file symlink");
}

/// Make `link` a symlink to the directory `target`, relative to the link.
fn symlink_dir(target: &str, link: &Path) {
    if let Some(parent) = link.parent() {
        fs::create_dir_all(parent).expect("link parent");
    }
    let target = native(target);
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("directory symlink");
    #[cfg(windows)]
    std::os::windows::fs::symlink_dir(target, link).expect("directory symlink");
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

    assert_eq!(
        repo.writes(&["stamp", "--dry-run"]),
        [native("write site/meta.yml")]
    );
    repo.cli().arg("stamp").assert().success();
    assert_eq!(repo.read("site/meta.yml"), "alpha: 0.0.1\nbeta: 0.0.1\n");
}

/// A code unit `alpha` at `packages/alpha` and a docs unit `beta` at
/// `site/beta`. Both write fields of the shared root file `/site/meta.yml`.
fn root_projection_config(mode: &str) -> String {
    format!(
        "$schema: https://intentional.foo/schemas/config.yml\ncontract: contract-1\nsettings:\n  internal-dependency-bump: patch\n  pre-1-0-bump-mapping: compatibility\nrelease-units:\n  alpha:\n    path: packages/alpha\n    projections:\n      - {{ adapter: json, file: manifest.json, pointer: /version, mode: {mode} }}\n      - {{ adapter: yaml, file: /site/meta.yml, pointer: /products/alpha/version, mode: {mode} }}\n    tags:\n      primary: {{ role: primary, template: '{{id}}@{{version}}' }}\n  beta:\n    path: site/beta\n    projections:\n      - {{ adapter: yaml, file: /site/meta.yml, pointer: products.alpha.docs-version, mode: {mode} }}\n    tags:\n      primary: {{ role: primary, template: '{{id}}@{{version}}' }}\n"
    )
}

fn meta(version: &str, docs_version: &str) -> String {
    format!("products:\n  alpha:\n    version: {version}\n    docs-version: {docs_version}\n")
}

/// The root-projection fixture, committed and baselined at 0.1.0.
fn root_projection_repository() -> TestRepo {
    let repo = TestRepo::new();
    repo.write(
        ".intentional/config.yml",
        &root_projection_config("committed"),
    );
    repo.write(
        "packages/alpha/manifest.json",
        "{\n  \"version\": \"0.1.0\"\n}\n",
    );
    repo.write("site/meta.yml", &meta("0.1.0", "0.1.0"));
    repo.write(".intentional/intents/.keep", "");
    repo.commit("add fixture");
    repo.cli().args(["tag", "--baseline"]).assert().success();
    repo
}

#[test]
fn apply_merges_two_units_into_one_root_projection_file() {
    let repo = root_projection_repository();
    repo.intent("joint-0001", &[("alpha", "patch"), ("beta", "patch")]);

    assert_eq!(
        repo.writes(&["apply", "--dry-run"]),
        [
            "write packages/alpha/CHANGELOG.md",
            "write packages/alpha/manifest.json",
            "write site/beta/CHANGELOG.md",
            "write site/meta.yml",
        ]
        .map(native)
    );
    repo.cli().arg("apply").assert().success();
    assert_eq!(repo.read("site/meta.yml"), meta("0.1.1", "0.1.1"));
    assert!(repo
        .read("packages/alpha/manifest.json")
        .contains("\"version\": \"0.1.1\""));
    repo.commit("apply release");

    repo.cli().arg("tag").assert().success();
    let tags = repo.tags();
    assert!(tags.contains(&"alpha@0.1.1".to_owned()), "{tags:?}");
    assert!(tags.contains(&"beta@0.1.1".to_owned()), "{tags:?}");
    repo.cli()
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("Drift: none"));
}

#[test]
fn apply_keeps_changelog_at_unit_path_with_root_projection() {
    let repo = root_projection_repository();
    repo.intent("joint-0001", &[("alpha", "patch"), ("beta", "patch")]);
    repo.cli().arg("apply").assert().success();
    assert!(repo
        .read("packages/alpha/CHANGELOG.md")
        .contains("## 0.1.1"));
    assert!(repo.read("site/beta/CHANGELOG.md").contains("## 0.1.1"));
    assert!(!repo.root.join("CHANGELOG.md").exists());
    assert!(!repo.root.join("site/CHANGELOG.md").exists());
}

#[test]
fn subset_release_edits_only_the_released_pointer() {
    let repo = root_projection_repository();
    repo.intent("docs-0001", &[("beta", "patch")]);
    repo.cli().arg("apply").assert().success();
    assert_eq!(repo.read("site/meta.yml"), meta("0.1.0", "0.1.1"));
    assert!(repo
        .read("packages/alpha/manifest.json")
        .contains("\"version\": \"0.1.0\""));
    repo.commit("apply release");
    repo.cli().arg("tag").assert().success();
    let tags = repo.tags();
    assert!(tags.contains(&"beta@0.1.1".to_owned()), "{tags:?}");
    assert!(!tags.contains(&"alpha@0.1.1".to_owned()), "{tags:?}");
}

#[test]
fn status_reports_drift_for_root_projection() {
    let repo = root_projection_repository();
    repo.write("site/meta.yml", &meta("0.9.0", "0.1.0"));
    repo.cli()
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains(native(
            "  alpha site/meta.yml: manifest 0.9.0 != tag 0.1.0",
        )));
}

#[test]
fn tag_refuses_drifted_root_projection() {
    let repo = root_projection_repository();
    repo.intent("alpha-0001", &[("alpha", "patch")]);
    repo.cli().arg("apply").assert().success();
    repo.write("site/meta.yml", &meta("0.1.0", "0.1.0"));
    repo.commit("apply release with a reverted field");
    repo.cli()
        .arg("tag")
        .assert()
        .failure()
        .stderr(predicate::str::contains(native(
            "release unit alpha projection site/meta.yml is 0.1.0, applied release is 0.1.1",
        )));
}

#[test]
fn baseline_reads_root_projection() {
    let repo = TestRepo::new();
    repo.write(
        ".intentional/config.yml",
        &root_projection_config("committed"),
    );
    repo.write(
        "packages/alpha/manifest.json",
        "{\n  \"version\": \"0.1.0\"\n}\n",
    );
    repo.write("site/meta.yml", &meta("0.2.0", "0.1.0"));
    repo.commit("add fixture");
    repo.cli()
        .args(["tag", "--baseline"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(native("baseline projections disagree for alpha: packages/alpha/manifest.json=0.1.0, site/meta.yml=0.2.0")));
}

#[test]
fn stamp_writes_injected_root_projection() {
    let repo = TestRepo::new();
    repo.write(
        ".intentional/config.yml",
        &root_projection_config("injected"),
    );
    repo.write(
        "packages/alpha/manifest.json",
        "{\n  \"version\": \"0.0.0\"\n}\n",
    );
    repo.write("site/meta.yml", &meta("0.0.0", "0.0.0"));
    repo.write(
        ".intentional/intents/joint-0001.md",
        "---\nalpha: patch\nbeta: patch\n---\n\nShip a change.\n",
    );
    repo.commit("add fixture");
    assert_eq!(
        repo.writes(&["stamp", "--dry-run"]),
        ["write packages/alpha/manifest.json", "write site/meta.yml"].map(native)
    );
    repo.cli().arg("stamp").assert().success();
    assert_eq!(repo.read("site/meta.yml"), meta("0.0.1", "0.0.1"));
}

#[test]
fn init_rerun_preserves_root_projection() {
    let repo = TestRepo::new();
    repo.write("pnpm-workspace.yaml", "packages:\n  - components/*\n");
    repo.write(
        "components/library/package.json",
        "{\n  \"name\": \"sample-library\",\n  \"version\": \"1.0.0\"\n}\n",
    );
    repo.commit("add discovery fixture");
    repo.cli().arg("init").assert().code(2);
    resolve_independent(&repo);
    repo.cli().arg("init").assert().success();

    // Add a root projection to the initialized unit by hand.
    let mut config = intentional_core::Config::load(&repo.root).expect("initialized config");
    let unit = config
        .release_units
        .get_mut("sample-library")
        .expect("initialized unit");
    unit.projections.push(
        serde_yaml::from_str(
            "{ adapter: yaml, file: /site/meta.yml, pointer: /library, mode: committed }",
        )
        .expect("root projection"),
    );
    repo.write(
        ".intentional/config.yml",
        &config.to_yaml().expect("config"),
    );
    repo.write("site/meta.yml", "library: 1.0.0\n");
    repo.commit("add a root projection");

    let no_op = initialize(&repo.root, false).expect("repeatable no-op");
    assert_eq!(no_op.state, InitState::Success);
    assert!(no_op.operations.is_empty());

    // A newly discovered manifest makes init rewrite the configuration.
    repo.write(
        "components/other/package.json",
        "{\n  \"name\": \"sample-other\",\n  \"version\": \"1.0.0\"\n}\n",
    );
    repo.commit("add another package");
    repo.cli().arg("init").assert().code(2);
    resolve_independent(&repo);
    repo.cli().arg("init").assert().success();
    let config = intentional_core::Config::load(&repo.root).expect("reconciled config");
    assert!(config.release_units.contains_key("sample-other"));
    let root = &config.release_units["sample-library"].projections;
    assert!(
        root.iter()
            .any(|projection| projection.file == "/site/meta.yml"
                && projection.pointer.as_deref() == Some("/library")),
        "{root:?}"
    );
}

/// Resolve every open discovery candidate as an independent release unit.
fn resolve_independent(repo: &TestRepo) {
    let path = repo.root.join(".intentional/init-plan.yml");
    let mut plan: InitPlan =
        serde_yaml::from_str(&fs::read_to_string(&path).expect("init plan")).expect("valid plan");
    for candidate in &mut plan.discovery_candidates {
        if candidate.resolution.is_none() {
            candidate.resolution = Some(CandidateResolution::Independent {
                release_unit: candidate.native_identity.clone().expect("native identity"),
            });
        }
    }
    fs::write(&path, serde_yaml::to_string(&plan).expect("plan yaml")).expect("write plan");
}

/// The reproduction from the symlink-alias issue: units `alpha` at `a` and
/// `beta` at `b` each reach the shared file through their own symlink.
fn symlinked_repository(alpha_pointer: &str, beta_pointer: &str) -> TestRepo {
    let repo = TestRepo::new();
    repo.write(
        ".intentional/config.yml",
        &config(&[
            yaml_unit("alpha", "a", "shared.yml", alpha_pointer, "committed"),
            yaml_unit("beta", "b", "shared.yml", beta_pointer, "committed"),
        ]),
    );
    repo.write("shared.yml", "alpha: 1.0.0\nbeta: 1.0.0\n");
    symlink_file("../shared.yml", &repo.root.join("a/shared.yml"));
    symlink_file("../shared.yml", &repo.root.join("b/shared.yml"));
    repo.write(".intentional/intents/.keep", "");
    repo.commit("add fixture");
    repo
}

#[test]
fn apply_merges_edits_reached_through_symlinks() {
    let repo = symlinked_repository("/alpha", "/beta");
    repo.cli().args(["tag", "--baseline"]).assert().success();
    repo.intent("joint-0001", &[("alpha", "patch"), ("beta", "patch")]);
    assert_eq!(
        repo.writes(&["apply", "--dry-run"]),
        [
            "write a/CHANGELOG.md",
            "write a/shared.yml",
            "write b/CHANGELOG.md"
        ]
        .map(native)
    );
    repo.cli().arg("apply").assert().success();
    assert_eq!(repo.read("shared.yml"), "alpha: 1.0.1\nbeta: 1.0.1\n");
    repo.commit("apply release");
    repo.cli().arg("tag").assert().success();
    let tags = repo.tags();
    assert!(tags.contains(&"alpha@1.0.1".to_owned()), "{tags:?}");
    assert!(tags.contains(&"beta@1.0.1".to_owned()), "{tags:?}");
}

#[test]
fn stamp_merges_edits_reached_through_symlinks() {
    let repo = TestRepo::new();
    repo.write(
        ".intentional/config.yml",
        &config(&[
            yaml_unit("alpha", "a", "shared.yml", "/alpha", "injected"),
            yaml_unit("beta", "b", "shared.yml", "/beta", "injected"),
        ]),
    );
    repo.write("shared.yml", "alpha: 0.0.0\nbeta: 0.0.0\n");
    symlink_file("../shared.yml", &repo.root.join("a/shared.yml"));
    symlink_file("../shared.yml", &repo.root.join("b/shared.yml"));
    repo.write(
        ".intentional/intents/joint-0001.md",
        "---\nalpha: patch\nbeta: patch\n---\n\nShip a shared change.\n",
    );
    repo.commit("add fixture");
    assert_eq!(
        repo.writes(&["stamp", "--dry-run"]),
        [native("write a/shared.yml")]
    );
    repo.cli().arg("stamp").assert().success();
    assert_eq!(repo.read("shared.yml"), "alpha: 0.0.1\nbeta: 0.0.1\n");
}

/// Whether the repository's filesystem ignores letter case. CI sets
/// `INTENTIONAL_TEST_CASE_INSENSITIVE` where it does, so that a case test
/// cannot pass there without running.
fn ignores_case(repo: &TestRepo) -> bool {
    repo.write("probe/case", "");
    let ignores = repo.root.join("PROBE/CASE").exists();
    fs::remove_dir_all(repo.root.join("probe")).expect("remove probe");
    assert!(
        ignores || std::env::var_os("INTENTIONAL_TEST_CASE_INSENSITIVE").is_none(),
        "expected a filesystem that ignores letter case"
    );
    ignores
}

#[test]
fn apply_merges_case_aliases() {
    let repo = TestRepo::new();
    if !ignores_case(&repo) {
        return;
    }
    repo.write(
        ".intentional/config.yml",
        &config(&[
            yaml_unit("alpha", "a", "/site/Meta.yml", "/alpha", "committed"),
            yaml_unit("beta", "b", "/site/meta.yml", "/beta", "committed"),
        ]),
    );
    repo.write("site/meta.yml", "alpha: 1.0.0\nbeta: 1.0.0\n");
    repo.write(".intentional/intents/.keep", "");
    repo.commit("add fixture");
    repo.cli().args(["tag", "--baseline"]).assert().success();
    repo.intent("joint-0001", &[("alpha", "patch"), ("beta", "patch")]);
    let writes = repo.writes(&["apply", "--dry-run"]);
    assert!(
        writes.contains(&native("write site/Meta.yml")),
        "{writes:?}"
    );
    assert!(
        !writes.contains(&native("write site/meta.yml")),
        "{writes:?}"
    );
    repo.cli().arg("apply").assert().success();
    assert_eq!(repo.read("site/meta.yml"), "alpha: 1.0.1\nbeta: 1.0.1\n");
}

/// The root-projection fixture with `/site/meta.yml` replaced by a symlink
/// to a file outside the repository.
fn outside_repository(mode: &str) -> TestRepo {
    let repo = TestRepo::new();
    repo.write(".intentional/config.yml", &root_projection_config(mode));
    repo.write(
        "packages/alpha/manifest.json",
        "{\n  \"version\": \"0.1.0\"\n}\n",
    );
    fs::write(
        repo.root
            .parent()
            .expect("temporary parent")
            .join("outside.yml"),
        meta("0.1.0", "0.1.0"),
    )
    .expect("outside file");
    symlink_file("../../outside.yml", &repo.root.join("site/meta.yml"));
    repo.write(
        ".intentional/intents/joint-0001.md",
        "---\nalpha: patch\nbeta: patch\n---\n\nShip a change.\n",
    );
    repo.commit("add fixture");
    repo
}

#[test]
fn refuses_projection_resolving_outside_repository() {
    let message = native("site/meta.yml resolves outside the repository");
    let repo = outside_repository("committed");
    for args in [
        &["status"][..],
        &["check"],
        &["apply", "--dry-run"],
        &["tag", "--baseline"],
    ] {
        repo.cli()
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains(message.clone()));
    }
    let repo = outside_repository("injected");
    repo.cli()
        .args(["stamp", "--dry-run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(message));
    let outside = repo
        .root
        .parent()
        .expect("temporary parent")
        .join("outside.yml");
    assert_eq!(
        fs::read_to_string(outside).expect("outside file"),
        meta("0.1.0", "0.1.0")
    );
}

#[test]
fn tag_refuses_projection_resolving_outside_repository() {
    let repo = root_projection_repository();
    repo.intent("alpha-0001", &[("alpha", "patch")]);
    repo.cli().arg("apply").assert().success();
    let outside = repo
        .root
        .parent()
        .expect("temporary parent")
        .join("outside.yml");
    fs::rename(repo.root.join("site/meta.yml"), &outside).expect("move file out");
    symlink_file("../../outside.yml", &repo.root.join("site/meta.yml"));
    repo.commit("apply release through an outside file");
    repo.cli()
        .arg("tag")
        .assert()
        .failure()
        .stderr(predicate::str::contains(native(
            "site/meta.yml resolves outside the repository",
        )));
    assert!(!repo.tags().contains(&"alpha@0.1.1".to_owned()));
}

#[test]
fn refuses_changelog_resolving_outside_repository() {
    let repo = root_projection_repository();
    let outside = repo
        .root
        .parent()
        .expect("temporary parent")
        .join("outside");
    fs::create_dir(&outside).expect("outside directory");
    fs::remove_dir_all(repo.root.join("site/beta")).ok();
    symlink_dir("../../outside", &repo.root.join("site/beta"));
    repo.intent("docs-0001", &[("beta", "patch")]);
    repo.cli()
        .arg("apply")
        .assert()
        .failure()
        .stderr(predicate::str::contains(native(
            "site/beta/CHANGELOG.md resolves outside the repository",
        )));
    assert!(!outside.join("CHANGELOG.md").exists());
    assert_eq!(repo.read("site/meta.yml"), meta("0.1.0", "0.1.0"));
}

#[test]
fn refuses_one_field_projected_through_symlink_aliases() {
    let message = format!(
        "release units alpha and beta both project /version in one file: {} and {} are the same file",
        native("a/shared.yml"),
        native("b/shared.yml")
    );
    let repo = symlinked_repository("/version", "/version");
    repo.write("shared.yml", "version: 1.0.0\n");
    repo.intent("joint-0001", &[("alpha", "patch"), ("beta", "patch")]);
    for args in [&["status"][..], &["check"], &["apply", "--dry-run"]] {
        repo.cli()
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains(message.clone()));
    }

    let repo = TestRepo::new();
    repo.write(
        ".intentional/config.yml",
        &config(&[
            yaml_unit("alpha", "a", "shared.yml", "/version", "injected"),
            yaml_unit("beta", "b", "shared.yml", "version", "injected"),
        ]),
    );
    repo.write("shared.yml", "version: 0.0.0\n");
    symlink_file("../shared.yml", &repo.root.join("a/shared.yml"));
    symlink_file("../shared.yml", &repo.root.join("b/shared.yml"));
    repo.write(
        ".intentional/intents/joint-0001.md",
        "---\nalpha: patch\nbeta: minor\n---\n\nShip a change.\n",
    );
    repo.commit("add fixture");
    repo.cli()
        .args(["stamp", "--dry-run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains(message));
}

#[test]
fn tag_refuses_changelog_resolving_outside_repository() {
    let repo = root_projection_repository();
    repo.intent("docs-0001", &[("beta", "patch")]);
    repo.cli().arg("apply").assert().success();
    let outside = repo
        .root
        .parent()
        .expect("temporary parent")
        .join("CHANGELOG.md");
    fs::rename(repo.root.join("site/beta/CHANGELOG.md"), &outside).expect("move changelog out");
    symlink_file(
        "../../../CHANGELOG.md",
        &repo.root.join("site/beta/CHANGELOG.md"),
    );
    repo.commit("apply release through an outside changelog");
    repo.cli()
        .arg("tag")
        .assert()
        .failure()
        .stderr(predicate::str::contains(native(
            "site/beta/CHANGELOG.md resolves outside the repository",
        )));
    assert!(!repo.tags().contains(&"beta@0.1.1".to_owned()));
}

#[test]
fn apply_refuses_a_non_canonical_json_array_index() {
    let repo = TestRepo::new();
    repo.write(
        ".intentional/config.yml",
        &config(&[
            "  alpha:\n    path: .\n    projections:\n      - { adapter: json, file: meta.json, pointer: /versions/1, mode: committed }\n    tags:\n      primary: { role: primary, template: '{id}@{version}' }\n".to_owned(),
            "  beta:\n    path: b\n    projections:\n      - { adapter: json, file: /meta.json, pointer: /versions/01, mode: committed }\n    tags:\n      primary: { role: primary, template: '{id}@{version}' }\n".to_owned(),
        ]),
    );
    let meta = "{\n  \"versions\": [\"0.0.0\", \"1.0.0\"]\n}\n";
    repo.write("meta.json", meta);
    repo.write(
        ".intentional/intents/joint-0001.md",
        "---\nalpha: patch\nbeta: minor\n---\n\nShip a change.\n",
    );
    repo.commit("add fixture");
    repo.cli()
        .arg("apply")
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "JSON pointer segment \"01\" was not found",
        ));
    assert_eq!(repo.read("meta.json"), meta);
    assert!(repo
        .root
        .join(".intentional/intents/joint-0001.md")
        .exists());
}

/// Unit `alpha` at `a` is a Cargo member that inherits its version from
/// the workspace manifest at the repository root.
fn inherited_cargo_repository(root_manifest: &str) -> TestRepo {
    let repo = TestRepo::new();
    repo.write(
        ".intentional/config.yml",
        &config(&[
            "  alpha:\n    path: a\n    projections:\n      - { adapter: cargo, file: Cargo.toml, mode: committed }\n    tags:\n      primary: { role: primary, template: '{id}@{version}' }\n".to_owned(),
        ]),
    );
    repo.write(
        "a/Cargo.toml",
        "[package]\nname = \"alpha\"\nversion.workspace = true\n",
    );
    repo.write("Cargo.toml", root_manifest);
    repo
}

#[test]
fn refuses_cargo_workspace_manifest_resolving_outside_repository() {
    // The second outside manifest is not a workspace: a search that read it
    // would go on and report a missing workspace instead of refusing.
    for outside_manifest in [
        "[workspace]\nmembers = [\"a\"]\n\n[workspace.package]\nversion = \"7.8.9\"\n",
        "[package]\nname = \"outside\"\nversion = \"7.8.9\"\n",
    ] {
        refuses_outside_cargo_manifest(outside_manifest);
    }
}

fn refuses_outside_cargo_manifest(outside_manifest: &str) {
    let repo = inherited_cargo_repository("");
    fs::remove_file(repo.root.join("Cargo.toml")).expect("remove manifest");
    fs::write(
        repo.root
            .parent()
            .expect("temporary parent")
            .join("outside.toml"),
        outside_manifest,
    )
    .expect("outside manifest");
    symlink_file("../outside.toml", &repo.root.join("Cargo.toml"));
    repo.write(
        ".intentional/intents/alpha-0001.md",
        "---\nalpha: patch\n---\n\nShip a change.\n",
    );
    repo.commit("add fixture");
    for args in [&["status"][..], &["check"], &["apply", "--dry-run"]] {
        repo.cli()
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains(
                "Cargo.toml resolves outside the repository",
            ))
            .stdout(predicate::str::contains("7.8.9").not());
    }
}

#[test]
fn refuses_a_projection_of_an_inherited_cargo_version() {
    let repo = inherited_cargo_repository(
        "[workspace]\nmembers = [\"a\"]\n\n[workspace.package]\nversion = \"1.0.0\"\n",
    );
    let mut config = repo.read(".intentional/config.yml");
    config.push_str("  beta:\n    path: b\n    projections:\n      - { adapter: toml, file: /Cargo.toml, pointer: /workspace/package/version, mode: committed }\n    tags:\n      primary: { role: primary, template: '{id}@{version}' }\n");
    repo.write(".intentional/config.yml", &config);
    repo.write(
        ".intentional/intents/joint-0001.md",
        "---\nalpha: patch\nbeta: minor\n---\n\nShip a change.\n",
    );
    repo.commit("add fixture");
    let message = format!(
        "release units alpha and beta both project /workspace/package/version in one file: Cargo.toml (inherited by {}) and Cargo.toml are the same file",
        native("a/Cargo.toml")
    );
    for args in [&["status"][..], &["check"], &["apply", "--dry-run"]] {
        repo.cli()
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains(message.clone()));
    }
    assert!(repo
        .root
        .join(".intentional/intents/joint-0001.md")
        .exists());
}

#[test]
fn allows_cargo_members_that_inherit_one_workspace_version() {
    let repo = inherited_cargo_repository(
        "[workspace]\nmembers = [\"a\", \"b\"]\n\n[workspace.package]\nversion = \"1.0.0\"\n",
    );
    let mut config = repo.read(".intentional/config.yml");
    config.push_str("  beta:\n    path: b\n    projections:\n      - { adapter: cargo, file: Cargo.toml, mode: committed }\n    tags:\n      primary: { role: primary, template: '{id}@{version}' }\n");
    repo.write(".intentional/config.yml", &config);
    repo.write(
        "b/Cargo.toml",
        "[package]\nname = \"beta\"\nversion.workspace = true\n",
    );
    repo.commit("add fixture");
    repo.cli().arg("status").assert().success();
}

#[test]
fn tag_refuses_one_field_projected_through_symlink_aliases() {
    let message = format!(
        "release units alpha and beta both project /version in one file: {} and {} are the same file",
        native("a/shared.yml"),
        native("b/shared.yml")
    );
    let repo = symlinked_repository("/version", "/version");
    repo.write("shared.yml", "version: 1.0.0\n");
    repo.commit("share one field");
    for args in [&["tag", "--baseline"][..], &["tag"]] {
        repo.cli()
            .args(args)
            .assert()
            .failure()
            .stderr(predicate::str::contains(message.clone()));
    }
    assert!(repo.tags().is_empty());
}

/// Replace `relative` with a symlink to a sentinel file outside the
/// repository, and return the sentinel's path.
fn swap_for_outside_sentinel(repo: &TestRepo, relative: &str, link_target: &str) -> PathBuf {
    let sentinel = repo
        .root
        .parent()
        .expect("temporary parent")
        .join("sentinel.yml");
    fs::write(&sentinel, "sentinel\n").expect("sentinel");
    fs::remove_file(repo.root.join(relative)).expect("remove planned file");
    symlink_file(link_target, &repo.root.join(relative));
    sentinel
}

#[test]
fn apply_refuses_a_target_that_leaves_the_repository_after_planning() {
    let repo = root_projection_repository();
    repo.intent("alpha-0001", &[("alpha", "patch")]);
    let result = intentional_core::ApplyResult::build(&repo.root, None).expect("planned release");
    let sentinel = swap_for_outside_sentinel(&repo, "site/meta.yml", "../../sentinel.yml");
    let error = result
        .apply(&repo.root, false)
        .expect_err("swapped target refused")
        .to_string();
    assert!(
        error.contains(&native("site/meta.yml resolves outside the repository")),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(&sentinel).expect("sentinel"),
        "sentinel\n"
    );
    assert!(!repo.root.join("packages/alpha/CHANGELOG.md").exists());
    assert!(repo
        .root
        .join(".intentional/intents/alpha-0001.md")
        .exists());
}

#[test]
fn apply_refuses_a_directory_that_leaves_the_repository_after_planning() {
    let repo = root_projection_repository();
    repo.intent("docs-0001", &[("beta", "patch")]);
    let result = intentional_core::ApplyResult::build(&repo.root, None).expect("planned release");
    let outside = repo
        .root
        .parent()
        .expect("temporary parent")
        .join("outside");
    fs::create_dir(&outside).expect("outside directory");
    symlink_dir("../../outside", &repo.root.join("site/beta"));
    let error = result
        .apply(&repo.root, false)
        .expect_err("swapped directory refused")
        .to_string();
    assert!(
        error.contains(&native(
            "site/beta/CHANGELOG.md resolves outside the repository"
        )),
        "{error}"
    );
    assert!(!outside.join("CHANGELOG.md").exists());
}

#[test]
fn stamp_refuses_a_target_that_leaves_the_repository_after_planning() {
    let repo = TestRepo::new();
    repo.write(
        ".intentional/config.yml",
        &root_projection_config("injected"),
    );
    repo.write(
        "packages/alpha/manifest.json",
        "{\n  \"version\": \"0.0.0\"\n}\n",
    );
    repo.write("site/meta.yml", &meta("0.0.0", "0.0.0"));
    repo.write(
        ".intentional/intents/joint-0001.md",
        "---\nalpha: patch\nbeta: patch\n---\n\nShip a change.\n",
    );
    repo.commit("add fixture");
    let result = intentional_core::StampResult::build(&repo.root, None).expect("planned stamp");
    let sentinel = swap_for_outside_sentinel(&repo, "site/meta.yml", "../../sentinel.yml");
    let error = result
        .apply(&repo.root, false)
        .expect_err("swapped target refused")
        .to_string();
    assert!(
        error.contains(&native("site/meta.yml resolves outside the repository")),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(&sentinel).expect("sentinel"),
        "sentinel\n"
    );
    assert!(repo
        .read("packages/alpha/manifest.json")
        .contains("\"version\": \"0.0.0\""));
}

#[test]
fn apply_merges_case_aliases_of_new_directories() {
    let repo = TestRepo::new();
    if !ignores_case(&repo) {
        return;
    }
    repo.write(
        ".intentional/config.yml",
        &config(&[
            yaml_unit("lower", "widgets", "/meta.yml", "/lower", "committed"),
            yaml_unit("upper", "Widgets", "/meta.yml", "/upper", "committed"),
        ]),
    );
    repo.write("meta.yml", "lower: 0.1.0\nupper: 0.2.0\n");
    repo.write(".intentional/intents/.keep", "");
    repo.commit("add fixture");
    repo.cli().args(["tag", "--baseline"]).assert().success();
    repo.write(
        ".intentional/intents/lower-0001.md",
        "---\nlower: patch\n---\n\nFix the lower widget.\n",
    );
    repo.write(
        ".intentional/intents/upper-0001.md",
        "---\nupper: patch\n---\n\nFix the upper widget.\n",
    );
    repo.commit("add intents");
    assert!(!repo.root.join("widgets").exists());
    let writes = repo.writes(&["apply", "--dry-run"]);
    assert_eq!(
        writes,
        ["write Widgets/CHANGELOG.md", "write meta.yml"].map(native),
        "{writes:?}"
    );
    repo.cli().arg("apply").assert().success();
    let changelog = repo.read("widgets/CHANGELOG.md");
    assert!(changelog.contains("Fix the lower widget."), "{changelog}");
    assert!(changelog.contains("Fix the upper widget."), "{changelog}");
}

#[test]
fn apply_keeps_case_variant_directories_apart_beside_case_variant_links() {
    let repo = TestRepo::new();
    if ignores_case(&repo) {
        // Such a directory cannot hold two entries that differ only in case.
        return;
    }
    repo.write(
        ".intentional/config.yml",
        &config(&[
            yaml_unit("lower", "empty/widgets", "/meta.yml", "/lower", "committed"),
            yaml_unit("upper", "empty/Widgets", "/meta.yml", "/upper", "committed"),
        ]),
    );
    repo.write("meta.yml", "lower: 0.1.0\nupper: 0.2.0\n");
    repo.write("empty/probe", "x");
    fs::hard_link(repo.root.join("empty/probe"), repo.root.join("empty/PROBE"))
        .expect("case-variant hard link");
    repo.write(".intentional/intents/.keep", "");
    repo.commit("add fixture");
    repo.cli().args(["tag", "--baseline"]).assert().success();
    repo.write(
        ".intentional/intents/joint-0001.md",
        "---\nlower: patch\nupper: patch\n---\n\nFix both widgets.\n",
    );
    repo.commit("add intent");
    assert_eq!(
        repo.writes(&["apply", "--dry-run"]),
        [
            "write empty/Widgets/CHANGELOG.md",
            "write empty/widgets/CHANGELOG.md",
            "write meta.yml",
        ]
        .map(native)
    );
    repo.cli().arg("apply").assert().success();
    assert!(repo.read("empty/widgets/CHANGELOG.md").contains("## 0.1.1"));
    assert!(repo.read("empty/Widgets/CHANGELOG.md").contains("## 0.2.1"));
}
