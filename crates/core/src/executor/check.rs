// ---
// relationships:
//   implements: github-release-executor
// ---

//! Locally observable GitHub executor conformance.

use crate::config::{Config, GithubConfig, WorkflowRole, CONFIG_PATH};
use crate::error::{Error, Result};
use crate::executor::goreleaser;
use crate::executor::recipe::{resolve_publications, Packager, SelectedPublication};
use crate::executor::workflow::{compare_configured_workflow, ComparisonStatus};
use crate::model::PublisherKind;
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
        if configured {
            findings.extend(native_packager_findings(root, unit, &publication)?);
        } else {
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
    findings.extend(global_tag_findings(&config));
    findings.extend(workflow_findings(root, &config, github)?);
    Ok(ExecutorCheck {
        publications,
        findings,
    })
}

/// Validate one release unit's native packager configuration against its recipe.
///
/// The presence of a packager's configuration file proves the packager is
/// configured, not that it is configured to produce what the selected recipe
/// promotes. A GoReleaser release unit that opts into Homebrew without declaring
/// the `brews` pipe builds a release whose publisher job has no formula to
/// promote, and the first place that is visible today is a release runner.
///
/// Every finding names a member of the packager's own configuration rather than
/// asking for a value in Intentional configuration. Native packager settings
/// stay native; conformance only states which of them the recipe depends on.
fn native_packager_findings(
    root: &Path,
    unit: &crate::config::ReleaseUnitConfig,
    publication: &SelectedPublication,
) -> Result<Vec<String>> {
    if publication.packager != Packager::GoReleaser {
        return Ok(Vec::new());
    }
    let directory = root.join(&unit.path);
    let Some(config) = goreleaser::read(&directory)? else {
        return Ok(Vec::new());
    };
    let identity = publication.identity();
    let file = unit.path.join(&config.path);
    let file = file.display();
    let mut findings = Vec::new();
    // The sealed subject identity is compared against what a publisher fragment
    // records, so it has to be the name GoReleaser gave the artifacts rather
    // than a name derived around it.
    if config.project_name.is_none() {
        findings.push(format!(
            "{identity} requires project_name in {file}; the maintained recipe seals that name as the subject identity every destination resolves"
        ));
    }
    if let Some(pipe) = goreleaser::pipe(publication.publisher) {
        if !config.pipes.iter().any(|declared| declared == pipe) {
            findings.push(format!(
                "{identity} promotes what the {pipe} pipe produces, but {file} declares no {pipe}"
            ));
        } else if let Some(format) = goreleaser::nfpm_format(publication.publisher) {
            if !config
                .nfpm_formats
                .iter()
                .any(|declared| declared == format)
            {
                findings.push(format!(
                    "{identity} distributes the {format} deliverable, but no {pipe} entry in {file} declares the {format} format"
                ));
            }
        }
    }
    // `brews` handles multiplicity by promoting every generated formula; `aur`
    // cannot. One publication reaches one Arch package, so a second entry is
    // built by the packager and then silently left unpublished. Saying so here
    // is the only place an author learns it, because the derivation resolves the
    // first entry and reports nothing about the rest.
    if publication.publisher == PublisherKind::Aur && config.aur_names.len() > 1 {
        findings.push(format!(
            "{identity} publishes the first of the {} aur entries {file} declares; a maintained Arch publication reaches one package, so the others are built and never published",
            config.aur_names.len()
        ));
    }
    if publication.publisher == PublisherKind::Aur
        && config
            .aur_names
            .first()
            .and_then(Option::as_deref)
            .is_some_and(|name| name.contains("{{"))
    {
        findings.push(format!("{identity} cannot publish templated aur[0].name in {file}; maintained Arch publication requires a literal package name"));
    }
    Ok(findings)
}

/// Report a tag configuration the release workflow could not publish.
///
/// The release workflow publishes exactly one annotated global release tag with
/// the release commit; every other configured tag is created later by the
/// publication workflow and declares the phase it belongs to. A configuration
/// with no unphased tag, or with more than one, only fails once the release
/// workflow has already accepted a source commit, so it is reported here where
/// it can still be fixed.
fn global_tag_findings(config: &Config) -> Vec<String> {
    let unphased = config
        .unphased_tags()
        .into_iter()
        .map(|tag| tag.id)
        .collect::<Vec<_>>();
    match unphased.len() {
        1 => Vec::new(),
        0 => vec![
            "the release workflow publishes one annotated global release tag, but no configured tag omits require-phase; leave exactly one tag unphased"
                .to_owned(),
        ],
        _ => vec![format!(
            "the release workflow publishes one annotated global release tag, but {} configured tags omit require-phase: {}; give all but one a require-phase declaration",
            unphased.len(),
            unphased.join(", ")
        )],
    }
}

/// Locally observable workflow conformance, using the same engine as diff and apply.
fn workflow_findings(root: &Path, config: &Config, github: &GithubConfig) -> Result<Vec<String>> {
    let mut findings = Vec::new();
    for role in WorkflowRole::ALL {
        let workflow = github.workflow(role);
        let comparison = compare_configured_workflow(root, config, role, None)?;
        match comparison.status {
            ComparisonStatus::Conformant => {}
            ComparisonStatus::Different => findings.push(format!(
                "Reserved workflow slice differs from the derived contract in {}; run intentional executor diff {role}",
                workflow.path.display()
            )),
            // Unresolved publications are already reported from recipe
            // selection, so the comparison does not repeat them.
            ComparisonStatus::Blocked => findings.extend(
                comparison
                    .diagnostics
                    .iter()
                    .filter(|diagnostic| diagnostic.code != "publication-unresolved")
                    .map(|diagnostic| format!("configured {role} workflow: {}", diagnostic.message)),
            ),
        }
        findings.extend(gate_findings(root, github, role)?);
    }
    Ok(findings)
}

/// Configured gates must name jobs the repository actually declares.
fn gate_findings(root: &Path, github: &GithubConfig, role: WorkflowRole) -> Result<Vec<String>> {
    let workflow = github.workflow(role);
    if workflow.gates.is_empty() {
        return Ok(Vec::new());
    }
    let Ok(text) = std::fs::read_to_string(root.join(&workflow.path)) else {
        return Ok(Vec::new());
    };
    let jobs = serde_yaml::from_str::<serde_yaml::Value>(&text)
        .ok()
        .and_then(|document| document.get("jobs").cloned())
        .and_then(|jobs| jobs.as_mapping().cloned())
        .map(|jobs| {
            jobs.keys()
                .filter_map(serde_yaml::Value::as_str)
                .map(str::to_owned)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    Ok(workflow
        .gates
        .iter()
        .filter(|gate| !jobs.contains(*gate))
        .map(|gate| {
            format!(
                "configured {role} workflow gate {gate} is not a job in {}",
                workflow.path.display()
            )
        })
        .collect())
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

    /// Bring both configured workflows up to their derived contract.
    fn converge(root: &Path) {
        for role in WorkflowRole::ALL {
            let comparison = crate::executor::workflow::compare_workflow(root, role, None)
                .expect("comparison runs");
            if comparison.changed() {
                comparison.apply().expect("transformation applies");
            }
        }
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
        converge(workspace.root());
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

    /// A Go release unit whose native configuration declares every pipe.
    const GORELEASER_CONFIG: &str = r#"version: 2
project_name: example-tool
builds:
  - main: ./cmd/example-tool
brews:
  - repository: { owner: example-org, name: homebrew-tap }
nfpms:
  - formats: [ rpm, deb ]
aur:
  - name: example-tool-bin
"#;

    fn go_workspace(label: &str, publisher: &str) -> Workspace {
        let workspace = workspace(label, publisher);
        workspace
            .write("component/go.mod", "module example.test/example-tool\n")
            .write(
                "component/cmd/example-tool/main.go",
                "package main\n\nfunc main() {}\n",
            )
            .write("component/.goreleaser.yaml", GORELEASER_CONFIG);
        workspace
    }

    /// Findings one workspace's native packager contract produces.
    fn packager_findings(workspace: &Workspace) -> Vec<String> {
        check_executor(workspace.root())
            .expect("check runs")
            .findings
            .into_iter()
            .filter(|finding| finding.starts_with("component/"))
            .collect()
    }

    #[test]
    fn accepts_native_goreleaser_configuration_that_declares_every_promoted_pipe() {
        let workspace = go_workspace(
            "check-goreleaser-conforming",
            "    homebrew: { repository: example-org/homebrew-tap }\n    rpm: {}\n    apt: {}\n    aur: {}\n",
        );
        assert!(
            packager_findings(&workspace).is_empty(),
            "{:?}",
            packager_findings(&workspace)
        );
    }

    #[test]
    fn reports_a_configured_publisher_whose_goreleaser_pipe_is_not_declared() {
        for (publisher, property, expected) in [
            (
                "brews",
                "    homebrew: { repository: example-org/homebrew-tap }\n",
                "component/homebrew/primary promotes what the brews pipe produces",
            ),
            (
                "nfpms",
                "    rpm: {}\n",
                "component/rpm/primary promotes what the nfpms pipe produces",
            ),
            (
                "aur",
                "    aur: {}\n",
                "component/aur/primary promotes what the aur pipe produces",
            ),
        ] {
            let workspace = go_workspace("check-goreleaser-pipe", property);
            workspace.write(
                "component/.goreleaser.yaml",
                &GORELEASER_CONFIG.replace(&format!("{publisher}:\n"), "unused:\n"),
            );
            let findings = packager_findings(&workspace);
            assert!(
                findings.iter().any(|finding| finding.contains(expected)),
                "an undeclared {publisher} pipe is reported: {findings:?}"
            );
        }
    }

    #[test]
    fn reports_a_system_package_format_no_nfpms_entry_declares() {
        for (property, format, identity) in [
            ("    rpm: {}\n", "rpm", "component/rpm/primary"),
            ("    apt: {}\n", "deb", "component/apt/primary"),
        ] {
            let workspace = go_workspace("check-goreleaser-format", property);
            workspace.write(
                "component/.goreleaser.yaml",
                &GORELEASER_CONFIG.replace("formats: [ rpm, deb ]", "formats: [ apk ]"),
            );
            let findings = packager_findings(&workspace);
            assert!(
                findings.iter().any(|finding| finding
                    .contains(&format!("{identity} distributes the {format} deliverable"))),
                "an nfpms declaration without the {format} format is reported: {findings:?}"
            );
        }
    }

    // A second entry is built by the packager and then never published, and the
    // derivation says nothing about it: it resolves the first entry and stops.
    // Conformance is the only place an author can learn this, so an author who
    // declares two Arch packages and sees one appear has to be told which.
    #[test]
    fn reports_an_arch_declaration_a_maintained_publication_cannot_reach() {
        let workspace = go_workspace("check-goreleaser-aur-multiple", "    aur: {}\n");
        workspace.write(
            "component/.goreleaser.yaml",
            &GORELEASER_CONFIG.replace(
                "  - name: example-tool-bin\n",
                "  - name: example-tool-bin\n  - name: example-other-bin\n",
            ),
        );
        let findings = packager_findings(&workspace);
        assert!(
            findings.iter().any(|finding| finding
                .contains("component/aur/primary publishes the first of the 2 aur entries")),
            "a second aur entry is reported: {findings:?}"
        );
    }

    #[test]
    fn reports_a_templated_arch_package_name() {
        let workspace = go_workspace("check-goreleaser-aur-template", "    aur: {}\n");
        workspace.write(
            "component/.goreleaser.yaml",
            &GORELEASER_CONFIG.replace(
                "  - name: example-tool-bin\n",
                "  - name: '{{ .ProjectName }}'\n",
            ),
        );
        let findings = packager_findings(&workspace);
        assert!(
            findings.iter().any(|finding| finding.contains(
                "component/aur/primary cannot publish templated aur[0].name in component/.goreleaser.yaml"
            )),
            "a templated aur name is reported: {findings:?}"
        );
    }

    #[test]
    fn reports_native_configuration_that_declares_no_project_name() {
        let workspace = go_workspace(
            "check-goreleaser-project",
            "    homebrew: { repository: example-org/homebrew-tap }\n",
        );
        workspace.write(
            "component/.goreleaser.yaml",
            &GORELEASER_CONFIG.replace("project_name: example-tool\n", ""),
        );
        let findings = packager_findings(&workspace);
        assert!(
            findings
                .iter()
                .any(|finding| finding
                    .contains("requires project_name in component/.goreleaser.yaml")),
            "{findings:?}"
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
            findings.contains("Reserved workflow slice differs from the derived contract"),
            "check reports managed drift through the comparison engine: {findings}"
        );
        assert!(
            findings.contains("publish workflow: .github/workflows/publish.yml does not exist"),
            "{findings}"
        );
    }

    #[test]
    fn accepts_exactly_one_unphased_global_release_tag() {
        let config = Config::from_yaml(CONFIG).expect("configuration");
        assert!(global_tag_findings(&config).is_empty());
    }

    #[test]
    fn reports_a_configuration_with_no_unphased_global_release_tag() {
        let config = Config::from_yaml(&CONFIG.replace(
            "primary: { role: primary, template: '{id}@{version}' }",
            "primary: { role: primary, template: '{id}@{version}', require-phase: after-publication }",
        ))
        .expect("configuration");
        let findings = global_tag_findings(&config);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].contains("no configured tag omits require-phase"));
    }

    #[test]
    fn reports_competing_unphased_global_release_tags_by_name() {
        let config = Config::from_yaml(&CONFIG.replace(
            "      primary: { role: primary, template: '{id}@{version}' }\n",
            "      primary: { role: primary, template: '{id}@{version}' }\n      mirror: { role: projection, template: 'v{id}-{version}' }\n",
        ))
        .expect("configuration");
        let findings = global_tag_findings(&config);
        assert_eq!(findings.len(), 1);
        assert!(findings[0].contains("release-unit/component/mirror"));
        assert!(findings[0].contains("release-unit/component/primary"));
    }
}
