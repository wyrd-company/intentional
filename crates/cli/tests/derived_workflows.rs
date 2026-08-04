// ---
// relationships:
//   implements: github-release-executor
// ---

//! Syntax gates for the workflows Intentional derives.
//!
//! These tests sit at the command-line crate boundary because they execute
//! repository tools against generated files. `cargo test` reaches them without
//! putting tool-process concerns into workflow derivation itself.

use intentional_core::executor::{fixture::derived_workflows, OWNERSHIP_SENTINEL};
use serde_yaml::Value;
use std::io::Write;
use std::process::{Command, Stdio};

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

/// Replace expressions the Actions runner resolves before invoking a shell.
///
/// `shellcheck` has no GitHub Actions expression evaluator. Replacing the
/// complete `${{ ... }}` span with one shell word preserves the surrounding
/// shell grammar while leaving expression semantics to `actionlint`, which
/// parses them in their native context.
fn resolve_workflow_expressions(body: &str) -> String {
    let mut resolved = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(start) = rest.find("${{") {
        resolved.push_str(&rest[..start]);
        let expression = &rest[start + 3..];
        let Some(end) = expression.find("}}") else {
            resolved.push_str(&rest[start..]);
            return resolved;
        };
        resolved.push_str("WORKFLOW_EXPRESSION");
        rest = &expression[end + 2..];
    }
    resolved.push_str(rest);
    resolved
}

#[test]
fn workflow_expressions_are_runner_values_during_shell_parsing() {
    assert_eq!(
        resolve_workflow_expressions("printf '%s\\n' '${{ github.sha }}'"),
        "printf '%s\\n' 'WORKFLOW_EXPRESSION'"
    );
}

/// Require one local syntax tool, naming the surface its absence leaves open.
fn require_tool(tool: &str, unchecked: &str) -> Result<(), String> {
    match Command::new(tool)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(format!("{tool} is missing; {unchecked} was not checked"))
        }
        Err(error) => Err(format!("cannot check whether {tool} is available: {error}")),
    }
}

#[test]
fn missing_syntax_tool_names_the_unchecked_surface() {
    let error = require_tool("missing-local-checker", "generated scripts")
        .expect_err("the absent checker is reported");
    assert_eq!(
        error,
        "missing-local-checker is missing; generated scripts was not checked"
    );
}

/// Parse every workflow and every shell body the workflow derivation emits.
///
/// The shared fixture's configured publication makes the publisher and closure
/// observable. Workflow roles, jobs, and `run:` bodies are extracted from the
/// derivation rather than named in a roster.
#[test]
fn derived_workflows_and_their_shell_bodies_parse() {
    let workflows = derived_workflows("derived-workflow-syntax");
    let directory = tempfile::tempdir().expect("temporary workflow directory");
    require_tool("actionlint", "derived workflow syntax").unwrap_or_else(|error| panic!("{error}"));
    require_tool("shellcheck", "derived run-body shell syntax")
        .unwrap_or_else(|error| panic!("{error}"));
    let mut managed_jobs = Vec::new();
    let mut shell_bodies = Vec::new();

    for (role, workflow) in &workflows {
        let path = directory.path().join(format!("{role}.yml"));
        std::fs::write(&path, workflow).expect("write derived workflow");
        let document: Value = serde_yaml::from_str(workflow).expect("derived workflow parses");
        for (job, body) in document["jobs"]
            .as_mapping()
            .expect("derived workflow jobs")
        {
            let managed = body["steps"].as_sequence().is_some_and(|steps| {
                steps
                    .iter()
                    .any(|step| step["id"].as_str() == Some(OWNERSHIP_SENTINEL))
            });
            if managed {
                managed_jobs.push((*role, job.as_str().expect("job id").to_owned()));
            }
        }
        collect_run_bodies(&document, &mut shell_bodies);

        let output = Command::new("actionlint")
            // Shell bodies are checked one by one below, so this invocation
            // owns workflow syntax and cannot mask a skipped body extraction.
            .arg("-shellcheck=")
            .arg(&path)
            .output()
            .expect("run actionlint");
        assert!(
            output.status.success(),
            "actionlint rejected the derived {role} workflow:\n{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    assert!(
        managed_jobs.iter().any(|(_, job)| job.contains("publish_")),
        "the fixture derives a publisher job: {managed_jobs:?}"
    );
    assert!(
        managed_jobs
            .iter()
            .any(|(_, job)| job.ends_with("close_release")),
        "the fixture derives the closure job: {managed_jobs:?}"
    );
    assert!(
        !shell_bodies.is_empty(),
        "the derived workflows contribute shell bodies to parse"
    );

    for (index, body) in shell_bodies.iter().enumerate() {
        let mut child = Command::new("shellcheck")
            .args(["--shell=bash", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("run shellcheck");
        child
            .stdin
            .take()
            .expect("shellcheck stdin")
            .write_all(resolve_workflow_expressions(body).as_bytes())
            .expect("write shell body");
        let output = child.wait_with_output().expect("finish shellcheck");
        assert!(
            output.status.success(),
            "shellcheck rejected derived run body {index}:\n{}{}\n{body}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
