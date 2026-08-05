// ---
// relationships:
//   implements: github-release-executor
// ---

//! Syntax gates for the workflows Intentional derives.
//!
//! This crate already enables core's `test-support` feature for its generated
//! invocation tests. Keeping the external-tool gate on that same test boundary
//! reuses the fixture without exposing it in a release build.

use intentional_core::config::WorkflowRole;
use intentional_core::executor::{
    fixture::{derived_recipe_workflows, derived_recipes},
    OWNERSHIP_SENTINEL,
};
use serde_yaml::Value;
use std::collections::BTreeSet;
use std::io::Write;
use std::process::{Command, Output, Stdio};

const HOSTED_ACTIONLINT_VERSION: &str = "v1.7.7";

/// Collect every `run:` body from a parsed workflow without naming its jobs.
fn collect_run_bodies(value: &Value, bodies: &mut Vec<String>) {
    match value {
        Value::Mapping(mapping) => {
            for (key, child) in mapping {
                if key.as_str() == Some("run") {
                    if let Some(body) = child.as_str() {
                        bodies.push(body.to_owned());
                    }
                }
                collect_run_bodies(child, bodies);
            }
        }
        Value::Sequence(items) => {
            for item in items {
                collect_run_bodies(item, bodies);
            }
        }
        _ => {}
    }
}

/// Count authored `run:` keys independently of the YAML tree walk.
fn textual_run_count(workflow: &str) -> usize {
    workflow
        .lines()
        .filter(|line| {
            let line = line.trim_start();
            line.starts_with("run:") || line.starts_with("- run:")
        })
        .count()
}

/// External parser and the generated surface it owns.
#[derive(Clone, Copy)]
enum SyntaxTool {
    Actionlint,
    Shellcheck,
}

impl SyntaxTool {
    const ALL: [Self; 2] = [Self::Actionlint, Self::Shellcheck];

    const fn command(self) -> &'static str {
        match self {
            Self::Actionlint => "actionlint",
            Self::Shellcheck => "shellcheck",
        }
    }

    const fn unchecked(self) -> &'static str {
        match self {
            Self::Actionlint => "derived workflow syntax",
            Self::Shellcheck => "derived run-body shell syntax",
        }
    }

    fn unavailable(self, error: std::io::Error) -> String {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!(
                "{} is missing; {} was not checked",
                self.command(),
                self.unchecked()
            )
        } else {
            format!("cannot run {}: {error}", self.command())
        }
    }
}

fn actionlint(path: &std::path::Path) -> Result<Output, String> {
    Command::new(SyntaxTool::Actionlint.command())
        // Shell bodies are checked one by one below, so this invocation owns
        // workflow syntax and cannot mask a skipped body extraction.
        .arg("-shellcheck=")
        .arg(path)
        .output()
        .map_err(|error| SyntaxTool::Actionlint.unavailable(error))
}

fn shellcheck(body: &str) -> Result<Output, String> {
    let mut child = Command::new(SyntaxTool::Shellcheck.command())
        .args(["--shell=bash", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| SyntaxTool::Shellcheck.unavailable(error))?;
    child
        .stdin
        .take()
        .expect("shellcheck stdin")
        .write_all(body.as_bytes())
        .expect("write shell body");
    child
        .wait_with_output()
        .map_err(|error| format!("cannot finish shellcheck: {error}"))
}

#[test]
fn every_required_tool_names_the_surface_its_absence_leaves_unchecked() {
    let missing = || std::io::Error::from(std::io::ErrorKind::NotFound);
    let messages = SyntaxTool::ALL
        .map(|tool| tool.unavailable(missing()))
        .into_iter()
        .collect::<Vec<_>>();
    assert_eq!(
        messages,
        [
            "actionlint is missing; derived workflow syntax was not checked",
            "shellcheck is missing; derived run-body shell syntax was not checked",
        ]
    );
}

#[test]
fn every_ci_job_running_workspace_tests_installs_the_actionlint_version_the_tests_require() {
    let workflow =
        std::fs::read_to_string("../../.github/workflows/ci.yml").expect("CI workflow is readable");
    let document: Value = serde_yaml::from_str(&workflow).expect("CI workflow parses");
    let installation = format!(
        "go install github.com/rhysd/actionlint/cmd/actionlint@{HOSTED_ACTIONLINT_VERSION}"
    );
    for job in ["minimum-rust", "test"] {
        let steps = document["jobs"][job]["steps"]
            .as_sequence()
            .unwrap_or_else(|| panic!("{job} job declares steps"));
        let install_position = steps
            .iter()
            .position(|step| {
                step["run"].as_str().is_some_and(|run| {
                    run.lines().any(|line| line == installation)
                        && run
                            .lines()
                            .any(|line| line == "echo \"$(go env GOPATH)/bin\" >> \"$GITHUB_PATH\"")
                })
            })
            .unwrap_or_else(|| {
                panic!("{job} installs the pinned actionlint required by the test suite")
            });
        let test_position = steps
            .iter()
            .position(|step| {
                step["run"]
                    .as_str()
                    .is_some_and(|run| run.contains("cargo test --workspace"))
            })
            .unwrap_or_else(|| panic!("{job} runs the workspace tests"));
        assert!(
            install_position < test_position,
            "actionlint is available before {job} tests require it"
        );
    }
}

#[test]
fn hosted_gate_queries_every_check_for_the_selected_pull_request() {
    let taskfile = std::fs::read_to_string("../../Taskfile.yml").expect("Taskfile is readable");
    let document: Value = serde_yaml::from_str(&taskfile).expect("Taskfile parses");
    let gate = &document["tasks"]["hosted:check"];
    assert_eq!(
        gate["requires"]["vars"].as_sequence(),
        Some(&vec![Value::String("PR".to_owned())]),
        "hosted status requires an explicit pull request"
    );
    assert_eq!(
        gate["cmds"][0].as_str(),
        Some("gh pr checks {{ .PR }}"),
        "hosted status reads every check instead of trusting the local gate set"
    );
}

/// Recipe identity carried by a derived publisher's shell environment.
fn recipe_identity(step: &Value) -> Option<(String, String, String)> {
    let environment = step["env"].as_mapping()?;
    let value = |suffix: &str| {
        environment.iter().find_map(|(key, value)| {
            key.as_str()?
                .ends_with(suffix)
                .then(|| value.as_str().map(str::to_owned))?
        })
    };
    Some((
        value("RELEASE_UNIT")?,
        value("PUBLISHER")?,
        value("TARGET")?,
    ))
}

/// Parse every workflow and every shell body the workflow derivation emits.
#[test]
fn derived_workflows_and_their_shell_bodies_parse() {
    let workflows = derived_recipe_workflows("derived-workflow-syntax");
    let directory = tempfile::tempdir().expect("temporary workflow directory");
    let mut managed_jobs = Vec::new();
    let mut shell_bodies = Vec::new();
    let mut reached_recipes = BTreeSet::new();

    for (role, workflow) in &workflows {
        let path = directory.path().join(format!("{role}.yml"));
        std::fs::write(&path, workflow).expect("write derived workflow");
        let document: Value = serde_yaml::from_str(workflow).expect("derived workflow parses");
        let before = shell_bodies.len();
        collect_run_bodies(&document, &mut shell_bodies);
        assert_eq!(
            shell_bodies.len() - before,
            textual_run_count(workflow),
            "the {role} tree walk reads every run key the derived text carries"
        );

        for (job, body) in document["jobs"]
            .as_mapping()
            .expect("derived workflow jobs")
        {
            let Some(steps) = body["steps"].as_sequence() else {
                continue;
            };
            if steps
                .iter()
                .any(|step| step["id"].as_str() == Some(OWNERSHIP_SENTINEL))
            {
                let run_count = steps.iter().filter(|step| step["run"].is_string()).count();
                managed_jobs.push((*role, job.as_str().expect("job id").to_owned(), run_count));
                reached_recipes.extend(steps.iter().filter_map(recipe_identity));
            }
        }

        let output = actionlint(&path).unwrap_or_else(|error| panic!("{error}"));
        assert!(
            output.status.success(),
            "actionlint rejected the derived {role} workflow:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let expected_recipes = derived_recipes()
        .iter()
        .map(|recipe| {
            (
                recipe.capability.as_str().to_owned(),
                recipe.publisher.as_str().to_owned(),
                recipe.target.to_owned(),
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(
        reached_recipes, expected_recipes,
        "derived managed shell reaches every maintained recipe"
    );
    assert!(
        managed_jobs.iter().any(|(role, job, bodies)| {
            *role == WorkflowRole::Publish && job.contains("publish_") && *bodies > 0
        }),
        "the publish workflow derives publisher shell: {managed_jobs:?}"
    );
    assert!(
        managed_jobs.iter().any(|(role, job, bodies)| {
            *role == WorkflowRole::Publish && job.ends_with("close_release") && *bodies > 0
        }),
        "the publish workflow derives closure shell: {managed_jobs:?}"
    );
    assert!(
        shell_bodies.iter().all(|body| !body.contains("${{")),
        "derived run bodies route runner expressions through env before shell parsing"
    );

    for (index, body) in shell_bodies.iter().enumerate() {
        let output = shellcheck(body).unwrap_or_else(|error| panic!("{error}"));
        assert!(
            output.status.success(),
            "shellcheck rejected derived run body {index}:\n{}{}\n{body}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
