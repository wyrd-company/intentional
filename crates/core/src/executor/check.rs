// ---
// relationships:
//   implements: github-release-executor
// ---

//! Locally observable GitHub executor conformance.

use crate::config::{Config, GithubConfig, WorkflowRole, CONFIG_PATH};
use crate::error::{Error, Result};
use crate::executor::recipe::resolve_publications;
use std::collections::BTreeSet;
use std::path::Path;

/// Result of validating the locally observable executor contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutorCheck {
    /// Publications resolved to a maintained recipe.
    pub publications: Vec<String>,
    /// Nonconformance findings; an empty list means the executor conforms.
    pub findings: Vec<String>,
}

impl ExecutorCheck {
    /// Whether every locally observable executor requirement is satisfied.
    pub fn conforms(&self) -> bool {
        self.findings.is_empty()
    }
}

/// Validate configuration, recipes, native packagers, and locally observable executor setup.
pub fn check_executor(root: &Path) -> Result<ExecutorCheck> {
    let config = Config::load(root)?;
    let Some(github) = &config.github else {
        return Err(Error::Validation(format!(
            "{CONFIG_PATH} has no github executor configuration; run intentional executor init"
        )));
    };
    let mut publications = Vec::new();
    // Report every unresolved target in one run rather than making the user
    // rediscover them one command at a time.
    let selection = resolve_publications(root, &config)?;
    let mut findings = selection.diagnostics;
    for publication in selection.selected {
        let unit = &config.release_units[&publication.release_unit];
        let configured = publication
            .packager
            .configuration_paths()
            .iter()
            .any(|relative| root.join(&unit.path).join(relative).is_file());
        if !configured {
            findings.push(format!(
                "{} requires {} configuration in {}; expected one of {}",
                publication.identity(),
                publication.packager,
                unit.path.display(),
                publication.packager.configuration_paths().join(", ")
            ));
        }
        publications.push(publication.identity());
    }
    findings.extend(workflow_findings(root, github)?);
    Ok(ExecutorCheck {
        publications,
        findings,
    })
}

fn workflow_findings(root: &Path, github: &GithubConfig) -> Result<Vec<String>> {
    let mut findings = Vec::new();
    for role in WorkflowRole::ALL {
        let workflow = github.workflow(role);
        let path = root.join(&workflow.path);
        let Ok(text) = std::fs::read_to_string(&path) else {
            findings.push(format!(
                "configured {role} workflow {} does not exist",
                workflow.path.display()
            ));
            continue;
        };
        let document = match serde_yaml::from_str::<serde_yaml::Value>(&text) {
            Ok(document) => document,
            Err(error) => {
                findings.push(format!(
                    "configured {role} workflow {} is not valid YAML: {error}",
                    workflow.path.display()
                ));
                continue;
            }
        };
        let jobs = document
            .get("jobs")
            .and_then(serde_yaml::Value::as_mapping)
            .map(|jobs| {
                jobs.keys()
                    .filter_map(serde_yaml::Value::as_str)
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>()
            });
        let Some(jobs) = jobs else {
            findings.push(format!(
                "configured {role} workflow {} declares no jobs",
                workflow.path.display()
            ));
            continue;
        };
        for gate in &workflow.gates {
            if !jobs.contains(gate) {
                findings.push(format!(
                    "configured {role} workflow gate {gate} is not a job in {}",
                    workflow.path.display()
                ));
            }
        }
    }
    Ok(findings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::fixture::Workspace;

    const CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-1
github:
  workflows:
    release: { path: .github/workflows/release.yml, gates: [ candidate_check ] }
    publish: { path: .github/workflows/publish.yml }
release-units:
  component:
    path: component
    tags:
      primary: { role: primary, template: '{id}@{version}' }
"#;

    const RELEASE_WORKFLOW: &str = "name: release\non: { workflow_dispatch: {} }\njobs:\n  candidate_check:\n    runs-on: ubuntu-latest\n    steps: [ { run: 'true' } ]\n";

    const PUBLISH_WORKFLOW: &str =
        "name: publish\non: { push: { tags: [ '*' ] } }\njobs:\n  placeholder:\n    runs-on: ubuntu-latest\n    steps: [ { run: 'true' } ]\n";

    fn workspace(label: &str, publisher: &str) -> Workspace {
        let workspace = Workspace::new(label);
        workspace
            .write(
                ".intentional/config.yml",
                &CONFIG.replace(
                    "    path: component\n",
                    &format!("    path: component\n{publisher}"),
                ),
            )
            .write(".github/workflows/release.yml", RELEASE_WORKFLOW)
            .write(".github/workflows/publish.yml", PUBLISH_WORKFLOW);
        workspace
    }

    #[test]
    fn requires_github_executor_configuration() {
        let workspace = Workspace::new("check-unconfigured");
        workspace.write(
            ".intentional/config.yml",
            &CONFIG
                .replace("github:\n", "")
                .replace(
                    "  workflows:\n    release: { path: .github/workflows/release.yml, gates: [ candidate_check ] }\n    publish: { path: .github/workflows/publish.yml }\n",
                    "",
                ),
        );
        assert!(check_executor(workspace.root())
            .expect_err("unconfigured executor rejected")
            .to_string()
            .contains("no github executor configuration"));
    }

    #[test]
    fn accepts_a_conforming_workspace() {
        let workspace = workspace("check-conforming", "    npm: {}\n");
        workspace.write(
            "component/package.json",
            r#"{"name":"example-component","version":"1.0.0"}"#,
        );
        let result = check_executor(workspace.root()).expect("check runs");
        assert_eq!(
            result.publications,
            vec!["component/npm/primary".to_owned()]
        );
        assert!(result.conforms(), "{:?}", result.findings);
    }

    #[test]
    fn reports_missing_native_packager_configuration() {
        let workspace = workspace("check-packager", "    rpm: {}\n");
        workspace
            .write("component/go.mod", "module example.test/component\n")
            .write("component/main.go", "package main\n\nfunc main() {}\n");
        let result = check_executor(workspace.root()).expect("check runs");
        assert!(!result.conforms());
        assert!(
            result.findings[0].contains("requires goreleaser configuration"),
            "{:?}",
            result.findings
        );
    }

    #[test]
    fn reports_every_unresolved_target_in_one_run() {
        let workspace = workspace(
            "check-multiple",
            "    cargo: {}\n    oci:\n      ghcr: {}\n",
        );
        let result = check_executor(workspace.root()).expect("check runs");
        assert_eq!(
            result
                .findings
                .iter()
                .filter(|finding| finding
                    .contains("no maintained publication recipe matches the configured target"))
                .count(),
            2,
            "both unresolved targets are reported: {:?}",
            result.findings
        );
    }

    #[test]
    fn reports_unmatched_recipes_and_missing_workflow_gates() {
        let workspace = workspace("check-findings", "    cargo: {}\n");
        std::fs::remove_file(workspace.root().join(".github/workflows/publish.yml"))
            .expect("remove publish workflow");
        workspace.write(
            ".github/workflows/release.yml",
            "name: release\non: { workflow_dispatch: {} }\njobs:\n  other:\n    runs-on: ubuntu-latest\n    steps: [ { run: 'true' } ]\n",
        );
        let result = check_executor(workspace.root()).expect("check runs");
        assert!(!result.conforms());
        let findings = result.findings.join("\n");
        assert!(
            findings.contains("no maintained publication recipe matches"),
            "{findings}"
        );
        assert!(
            findings.contains("does not exist") && findings.contains("is not a job"),
            "one run reports every finding it can observe: {findings}"
        );
        assert!(
            findings.contains("gate candidate_check is not a job"),
            "{findings}"
        );
        assert!(
            findings.contains("publish workflow .github/workflows/publish.yml does not exist"),
            "{findings}"
        );
    }
}
