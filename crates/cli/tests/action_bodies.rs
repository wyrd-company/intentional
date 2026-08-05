// ---
// relationships:
//   tests: github-release-executor
// ---

//! Execute a shipped composite Action's shell body and assert the command line
//! it builds.
//!
//! A published Action is emitted shell that some other runtime executes, so a
//! test that reads its text proves the text is present and nothing else. It
//! cannot show that an optional input reaches the command as an *absent* option
//! rather than an empty one, and that distinction is load-bearing: an omitted
//! selector is what selects an adapter's configured primary destination, and an
//! empty `--draft-handoff` would make `verify publication` refuse every
//! publication whose consumer path reads no draft asset.
//!
//! These tests therefore run the body under `bash` with a stub `intentional`
//! ahead of it on `PATH`, and assert against the arguments the stub recorded.
//! The process environment is built from the Action's own parsed `env:` block
//! and `inputs:` defaults rather than from literals, so a renamed input or a
//! deleted default reaches these assertions by construction.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Repository root, from the crate this test belongs to.
fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root")
}

/// One shipped Action document, parsed.
fn action_document(action: &str) -> serde_yaml::Value {
    let path = repository_root()
        .join("actions")
        .join(action)
        .join("action.yml");
    let text =
        fs::read_to_string(&path).unwrap_or_else(|_| panic!("{} is readable", path.display()));
    serde_yaml::from_str(&text).expect("the Action document parses")
}

/// The step in `action` carrying `id`.
fn action_step(document: &serde_yaml::Value, id: &str) -> serde_yaml::Value {
    document["runs"]["steps"]
        .as_sequence()
        .expect("the Action declares steps")
        .iter()
        .find(|step| step["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("the Action declares a step with id {id}"))
        .clone()
}

/// The step's process environment, resolved from the Action's own declarations.
///
/// Each `env:` value is an `${{ inputs.<name> }}` expression. The value comes
/// from `supplied` when the caller names it and from the input's declared
/// `default:` otherwise, so an input the Action requires and the caller omits
/// fails here rather than reaching the body as an empty string. Building the
/// environment this way is what binds these assertions to the document: rename
/// an input, drop a default, or spell an `env:` key differently, and the
/// resolution fails instead of quietly testing a stale shape.
fn step_environment(
    document: &serde_yaml::Value,
    step: &serde_yaml::Value,
    supplied: &BTreeMap<&str, String>,
) -> BTreeMap<String, String> {
    let declared = document["inputs"]
        .as_mapping()
        .expect("the Action declares inputs");
    step["env"]
        .as_mapping()
        .expect("the step declares its inputs as environment")
        .iter()
        .map(|(key, value)| {
            let key = key.as_str().expect("environment keys are names").to_owned();
            let expression = value.as_str().expect("environment values are expressions");
            let input = expression
                .trim()
                .strip_prefix("${{")
                .and_then(|rest| rest.strip_suffix("}}"))
                .map(str::trim)
                .and_then(|reference| reference.strip_prefix("inputs."))
                .unwrap_or_else(|| panic!("{key} reads an Action input: {expression}"));
            let resolved = supplied.get(input).cloned().unwrap_or_else(|| {
                declared[input]["default"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("input {input} declares no default, so this test must supply it")
                    })
                    .to_owned()
            });
            (key, resolved)
        })
        .collect()
}

/// A stub `intentional` that records its arguments and satisfies the projection.
///
/// The body pipes the command into `project-identities.sh`, which refuses an
/// `evidence-path` naming no file, so the stub writes the fragment it reports.
const STUB: &str = r#"#!/usr/bin/env bash
set -euo pipefail
printf '%s\0' "$@" >> "$INTENTIONAL_ARGV_LOG"
OUTPUT=""
PREVIOUS=""
for ARG in "$@"; do
  if [[ "$PREVIOUS" == "--output" ]]; then
    OUTPUT="$ARG"
  fi
  PREVIOUS="$ARG"
done
: > "$OUTPUT"
printf 'evidence-path: %s\n' "$OUTPUT"
"#;

/// Run the verify-publication body with the named inputs and return its argv.
fn recorded_invocation(supplied: &BTreeMap<&str, String>) -> Vec<String> {
    let document = action_document("verify-publication");
    let step = action_step(&document, "verify");
    let body = step["run"].as_str().expect("the step declares a body");

    let temp = tempfile::tempdir().expect("temporary directory");
    let bin = temp.path().join("bin");
    fs::create_dir_all(&bin).expect("stub directory");
    let stub = bin.join("intentional");
    fs::write(&stub, STUB).expect("stub written");
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).expect("stub executable");

    // The projection refuses an `evidence-path` naming no file, so the fragment
    // the command reports is written inside this run's own directory.
    let mut supplied = supplied.clone();
    supplied.insert(
        "output",
        temp.path().join("fragment.yml").display().to_string(),
    );

    let argv_log = temp.path().join("argv");
    let github_output = temp.path().join("github-output");
    fs::write(&github_output, "").expect("step output file");

    let mut environment = step_environment(&document, &step, &supplied);
    environment.insert(
        "INTENTIONAL_ARGV_LOG".to_owned(),
        argv_log.display().to_string(),
    );
    environment.insert(
        "GITHUB_ACTION_PATH".to_owned(),
        repository_root()
            .join("actions/verify-publication")
            .display()
            .to_string(),
    );
    environment.insert(
        "GITHUB_OUTPUT".to_owned(),
        github_output.display().to_string(),
    );
    environment.insert(
        "PATH".to_owned(),
        format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );

    let status = Command::new("bash")
        .arg("-c")
        .arg(body)
        .env_clear()
        .envs(&environment)
        .status()
        .expect("the Action body runs");
    assert!(status.success(), "the Action body succeeds: {status}");

    let recorded = fs::read(&argv_log).expect("the stub recorded an invocation");
    let mut arguments: Vec<String> = recorded
        .split(|byte| *byte == 0)
        .map(|token| String::from_utf8(token.to_vec()).expect("arguments are text"))
        .collect();
    // The record ends with the separator that terminates the final argument.
    let trailing = arguments.pop();
    assert_eq!(
        trailing.as_deref(),
        Some(""),
        "the record is NUL-terminated"
    );
    arguments
}

/// Inputs every verification supplies, whatever its publisher consumes.
///
/// `output` is not among them: the run supplies its own, because the projection
/// checks that the reported fragment exists.
fn required() -> BTreeMap<&'static str, String> {
    BTreeMap::from([
        ("release-unit", "example-unit".to_owned()),
        ("package", "example-package".to_owned()),
        ("publisher", "example-publisher".to_owned()),
        ("observation", "observation.yml".to_owned()),
        ("working-directory", ".".to_owned()),
    ])
}

/// The pair `option value`, when the command carries that option.
fn option_value<'a>(arguments: &'a [String], option: &str) -> Option<&'a str> {
    arguments
        .iter()
        .position(|argument| argument == option)
        .map(|index| {
            arguments
                .get(index + 1)
                .unwrap_or_else(|| panic!("{option} carries a value"))
                .as_str()
        })
}

#[test]
fn omits_an_absent_selector_and_handoff_from_the_verification_command() {
    // Neither optional input is supplied, so each resolves to the empty default
    // the Action declares — the shape every non-draft-dependent publication has
    // on the live path.
    let arguments = recorded_invocation(&required());

    assert_eq!(
        option_value(&arguments, "--package"),
        Some("example-package"),
        "the required package reaches the command: {arguments:?}"
    );

    assert_eq!(
        option_value(&arguments, "--target"),
        None,
        "an omitted selector reaches the command as an absent option: {arguments:?}"
    );
    assert_eq!(
        option_value(&arguments, "--draft-handoff"),
        None,
        "an omitted handoff reaches the command as an absent option: {arguments:?}"
    );
    // The distinction that matters is absent-versus-empty, not absent-versus-set:
    // an empty value would be accepted by the option checks above while still
    // making the command refuse the publication.
    assert!(
        !arguments.iter().any(String::is_empty),
        "no empty argument reaches the command: {arguments:?}"
    );
}

#[test]
fn passes_a_supplied_selector_and_handoff_to_the_verification_command() {
    let mut supplied = required();
    supplied.insert("target", "example-target".to_owned());
    supplied.insert(
        "draft-handoff",
        "handoff/draft-release-asset-handoff.yml".to_owned(),
    );
    let arguments = recorded_invocation(&supplied);

    assert_eq!(
        option_value(&arguments, "--target"),
        Some("example-target"),
        "a supplied selector reaches the command: {arguments:?}"
    );
    assert_eq!(
        option_value(&arguments, "--draft-handoff"),
        Some("handoff/draft-release-asset-handoff.yml"),
        "a supplied handoff reaches the command: {arguments:?}"
    );
}

/// Run the verify-release-tag body against a stub reporting these identities.
///
/// The Action's own projector decides what reaches a step output, so the body
/// is executed rather than read: the stub reports identity lines, the projector
/// validates them, and the step's exit status and `$GITHUB_OUTPUT` are what the
/// assertions read.
fn projected_identities(reported: &str) -> Result<BTreeMap<String, String>, String> {
    let document = action_document("verify-release-tag");
    let step = action_step(&document, "verify");
    let body = step["run"].as_str().expect("the step declares a body");

    let temp = tempfile::tempdir().expect("temporary directory");
    let bin = temp.path().join("bin");
    fs::create_dir_all(&bin).expect("stub directory");
    let stub = bin.join("intentional");
    fs::write(
        &stub,
        format!("#!/usr/bin/env bash\nset -euo pipefail\ncat <<'REPORTED'\n{reported}REPORTED\n"),
    )
    .expect("stub written");
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).expect("stub executable");

    let github_output = temp.path().join("github-output");
    fs::write(&github_output, "").expect("step output file");

    let mut environment = step_environment(&document, &step, &BTreeMap::new());
    environment.insert(
        "GITHUB_ACTION_PATH".to_owned(),
        repository_root()
            .join("actions/verify-release-tag")
            .display()
            .to_string(),
    );
    environment.insert(
        "GITHUB_OUTPUT".to_owned(),
        github_output.display().to_string(),
    );
    environment.insert(
        "PATH".to_owned(),
        format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );

    let status = Command::new("bash")
        .arg("-c")
        .arg(body)
        .env_clear()
        .envs(&environment)
        .status()
        .expect("the Action body runs");
    let written = fs::read_to_string(&github_output).expect("the step output file is readable");
    if !status.success() {
        return Err(written);
    }
    Ok(written
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect())
}

/// Identity lines a verified release tag reports, with one value replaced.
fn reported_identities(object: &str) -> String {
    format!(
        "source-sha: {}\nrelease-sha: {}\nglobal-tag: release/1.0.0\nglobal-tag-object: {object}\nplan-digest: sha256:{}\n",
        "1".repeat(40),
        "2".repeat(40),
        "4".repeat(64),
    )
}

/// The projected tag object is held to the shape a Git object identity has.
///
/// The projector is what stands between the command's report and a privileged
/// step's `needs` expression, so the tag object gets the same check the other
/// two Git identities get. Without it a malformed value would reach a consumer
/// as an output, which is the failure the script exists to prevent.
#[test]
fn refuses_to_project_a_tag_object_that_is_not_a_git_object_identity() {
    let projected = projected_identities(&reported_identities(&"3".repeat(40)))
        .expect("well-formed identities");
    assert_eq!(
        projected.get("global-tag-object").map(String::as_str),
        Some("3".repeat(40).as_str()),
        "a complete Git object identity reaches the step output: {projected:?}"
    );

    for malformed in ["not-an-object", "333333", ""] {
        let refused = projected_identities(&reported_identities(malformed));
        assert!(
            refused.is_err(),
            "{malformed:?} is refused rather than projected: {refused:?}"
        );
    }
}
