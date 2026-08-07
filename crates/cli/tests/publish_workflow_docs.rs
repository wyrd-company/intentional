// ---
// relationships:
//   tests: github-release-executor
// ---

use serde_yaml::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

use intentional_core::config::WorkflowRole;
use intentional_core::executor::fixture::{derived_workflows, publish_workflow_kind_mermaid};
use intentional_core::executor::OWNERSHIP_SENTINEL;

const WORKFLOW_SEED: &str =
    "name: Publish\n\non: {}\n\npermissions:\n  contents: read\n\njobs: {}\n";

fn derived_repository_workflow() -> String {
    let root = std::path::Path::new("../..");
    let mut workflow = tempfile::Builder::new()
        .prefix("intentional-publish-docs-")
        .suffix(".yml")
        .tempfile_in(root)
        .expect("temporary publish workflow can be created in the repository");
    workflow
        .write_all(WORKFLOW_SEED.as_bytes())
        .expect("workflow seed can be written");
    assert_cmd::cargo::cargo_bin_cmd!("intentional")
        .current_dir(root)
        .args([
            "executor",
            "diff",
            "publish",
            "--workflow",
            workflow.path().to_str().expect("workflow path is UTF-8"),
            "--apply",
        ])
        .assert()
        .success();
    std::fs::read_to_string(workflow.path()).expect("derived workflow is readable")
}

fn documented_job_kinds(page: &str) -> BTreeSet<String> {
    page.lines()
        .filter_map(|line| {
            line.split_once("<!-- intentional-job-kind: ")
                .and_then(|(_, tail)| tail.split_once(" -->"))
                .map(|(kind, _)| kind.to_owned())
        })
        .collect()
}

fn documented_environments(page: &str) -> BTreeMap<String, String> {
    page.lines()
        .filter_map(|line| {
            let kind = line
                .split_once("<!-- intentional-job-kind: ")?
                .1
                .split_once(" -->")?
                .0;
            let environment = line
                .split_once("<!-- intentional-environment: ")?
                .1
                .split_once(" -->")?
                .0;
            Some((kind.to_owned(), environment.to_owned()))
        })
        .collect()
}

fn feature_scenario_bindings(source: &str) -> BTreeSet<String> {
    source
        .lines()
        .filter_map(|line| {
            line.split_once("intentional-feature-scenario: ")
                .map(|(_, binding)| binding.trim().to_owned())
        })
        .collect()
}

fn executable_feature_scenario_bindings(source: &str) -> BTreeSet<String> {
    let lines = source.lines().collect::<Vec<_>>();
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            let binding = line.split_once("intentional-feature-scenario: ")?.1.trim();
            let test = binding
                .rsplit_once('=')
                .map(|(_, test)| test)
                .expect("feature scenario binding names its test function");
            assert!(
                lines
                    .iter()
                    .skip(index + 1)
                    .take(4)
                    .any(|line| line.trim_start().starts_with(&format!("fn {test}()"))),
                "feature scenario binding {binding} annotates its named test function"
            );
            Some(binding.to_owned())
        })
        .collect()
}

fn job_kind(id: &str) -> &'static str {
    if id.ends_with("verify_tag") {
        "verify-tag"
    } else if id.contains("_build_") {
        "build"
    } else if id.ends_with("tag_before_publication") {
        "phase-before"
    } else if id.ends_with("upload_deliverables") {
        "upload"
    } else if id.contains("_publish_") {
        "publisher"
    } else if id.contains("_verify_") || id.contains("_retrieve_") {
        "verifier"
    } else if id.ends_with("tag_after_publication") {
        "phase-after"
    } else if id.ends_with("assemble_evidence") {
        "assemble"
    } else if id.ends_with("close_release") {
        "close"
    } else {
        panic!("managed publish job {id} has no documented kind")
    }
}

fn environment_boundaries(workflow: &str) -> BTreeMap<String, String> {
    let document: Value = serde_yaml::from_str(workflow).expect("derived workflow parses");
    document["jobs"]
        .as_mapping()
        .expect("derived workflow has jobs")
        .iter()
        .filter_map(|(id, body)| {
            let id = id.as_str().expect("job id is text");
            if !id.starts_with("intentional_") {
                return None;
            }
            let kind = job_kind(id).to_owned();
            let environment = if body.get("environment").is_some() {
                "protected"
            } else {
                "none"
            };
            Some((kind, environment.to_owned()))
        })
        .collect()
}

fn diagram_derivations() -> Vec<String> {
    let mut workflows = vec![derived_repository_workflow()];
    workflows.extend(
        derived_workflows("publish-docs-conditional-jobs")
            .into_iter()
            .filter_map(|(role, workflow)| (role == WorkflowRole::Publish).then_some(workflow)),
    );
    workflows
}

fn managed_job_kinds(jobs: &serde_yaml::Mapping) -> BTreeMap<String, &'static str> {
    jobs.iter()
        .filter_map(|(id, body)| {
            let managed = body["steps"].as_sequence().is_some_and(|steps| {
                steps
                    .iter()
                    .any(|step| step["id"].as_str() == Some(OWNERSHIP_SENTINEL))
            });
            managed.then(|| {
                let id = id.as_str().expect("managed job id is text");
                (id.to_owned(), job_kind(id))
            })
        })
        .collect()
}

fn direct_needs(body: &Value) -> Vec<&str> {
    match body.get("needs") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::String(need)) => vec![need.as_str()],
        Some(Value::Sequence(needs)) => needs
            .iter()
            .map(|need| need.as_str().expect("job need is text"))
            .collect(),
        Some(other) => panic!("job has unsupported needs {other:?}"),
    }
}

fn mermaid_kind_id(kind: &str) -> &'static str {
    match kind {
        "verify-tag" => "verify_tag",
        "build" => "build",
        "phase-before" => "phase_before",
        "upload" => "upload",
        "publisher" => "publisher",
        "verifier" => "verifier",
        "phase-after" => "phase_after",
        "assemble" => "assemble",
        "close" => "close",
        other => panic!("documented publish kind {other} has no Mermaid id"),
    }
}

#[test]
fn publish_workflow_page_leads_with_the_optional_executor_layer() {
    let schema: Value = serde_yaml::from_str(
        &std::fs::read_to_string("../../schemas/config.yml").expect("config schema is readable"),
    )
    .expect("config schema parses");
    let required = schema["required"]
        .as_sequence()
        .expect("config schema has required fields");
    let page = std::fs::read_to_string("../../docs/publish-workflow.md")
        .expect("publish workflow page is readable");
    let introduction = page
        .split_once("## Publish job graph")
        .expect("page introduces the layer before its graph")
        .0
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    assert!(
        required
            .iter()
            .all(|field| field.as_str() != Some("github")),
        "the GitHub executor remains optional in the published config schema"
    );
    assert!(
        introduction.contains("The GitHub release executor is optional"),
        "a direct reader meets the optional-executor statement first"
    );
    for core_capability in [
        "intent-driven versioning",
        "format-preserving manifest projection",
        "deterministic release planning",
        "verifiable annotated release records",
    ] {
        assert!(
            introduction.contains(core_capability),
            "the page names the executor-free core capability {core_capability}"
        );
    }
    assert_eq!(
        page.lines()
            .filter(|line| line.starts_with("## "))
            .collect::<Vec<_>>(),
        [
            "## Publish job graph",
            "## Job separation",
            "## Authority and credentials",
            "## Generated workflow maintenance",
        ],
        "the publish page uses plain descriptive section titles"
    );
    let normalized_page = page.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        normalized_page.contains("You own the complete workflow document")
            && normalized_page
                .contains("Intentional generates the jobs marked by its sentinel step")
            && !page.contains("Intentional owns"),
        "the page assigns workflow ownership to the consumer and generation to Intentional"
    );
}

#[test]
fn publish_workflow_page_matches_the_repository_derivation() {
    let workflow = derived_repository_workflow();
    let document: Value = serde_yaml::from_str(&workflow).expect("derived workflow parses");
    let jobs = document["jobs"]
        .as_mapping()
        .expect("derived workflow has jobs");
    let page = std::fs::read_to_string("../../docs/publish-workflow.md")
        .expect("publish workflow page is readable");
    let kinds = documented_job_kinds(&page);
    let environments = documented_environments(&page);

    for (id, body) in jobs {
        let id = id.as_str().expect("job id is text");
        let kind = job_kind(id);
        assert!(
            kinds.contains(kind),
            "managed job {id} has documented kind {kind}"
        );
        let expected_environment = if body.get("environment").is_some() {
            "protected"
        } else {
            "none"
        };
        assert_eq!(
            environments.get(kind).map(String::as_str),
            Some(expected_environment),
            "managed job {id} has the documented environment boundary"
        );
    }
}

#[test]
fn publish_workflow_diagram_matches_every_derived_kind_and_need() {
    let workflows = diagram_derivations();
    let mut expected_kinds = BTreeSet::new();
    let mut expected_edges = BTreeSet::new();
    for workflow in &workflows {
        let document: Value = serde_yaml::from_str(workflow).expect("derived workflow parses");
        let jobs = document["jobs"]
            .as_mapping()
            .expect("derived workflow has jobs");
        let job_kinds = managed_job_kinds(jobs);
        expected_kinds.extend(job_kinds.values().map(|kind| mermaid_kind_id(kind)));
        for (id, kind) in &job_kinds {
            let body = jobs
                .get(Value::String(id.clone()))
                .expect("managed job remains in its derivation");
            let needs = direct_needs(body);
            if matches!(*kind, "publisher" | "verifier") {
                assert!(
                    needs
                        .iter()
                        .any(|need| job_kinds.get(*need) == Some(&"build")),
                    "derived {kind} job {id} directly needs its subject build"
                );
            }
            for need in needs {
                if let Some(need_kind) = job_kinds.get(need) {
                    expected_edges.insert((mermaid_kind_id(need_kind), mermaid_kind_id(kind)));
                }
            }
        }
    }
    let mermaid = std::fs::read_to_string("../../docs/assets/publish-workflow.mmd")
        .expect("publish workflow Mermaid source is readable");
    let actual_kinds = mermaid
        .lines()
        .filter_map(|line| line.trim().split_once("[\"").map(|(id, _)| id))
        .collect::<BTreeSet<_>>();
    let actual_edges = mermaid
        .lines()
        .filter_map(|line| line.trim().split_once(" --> "))
        .collect::<BTreeSet<_>>();

    assert_eq!(
        actual_kinds, expected_kinds,
        "the Mermaid graph carries exactly every derived publish job kind"
    );
    assert_eq!(actual_kinds.len(), 9, "the derivations emit nine job kinds");
    assert_eq!(
        actual_edges, expected_edges,
        "the Mermaid graph carries exactly every direct derived kind need"
    );
    assert_eq!(
        mermaid,
        publish_workflow_kind_mermaid(&workflows),
        "the checked-in Mermaid source is the generated kind projection"
    );
}

#[test]
fn publish_workflow_page_matches_derived_environment_boundaries() {
    let mut boundaries = environment_boundaries(&derived_repository_workflow());
    let conditional = derived_workflows("publish-docs-conditional-jobs")
        .into_iter()
        .find_map(|(role, workflow)| (role == WorkflowRole::Publish).then_some(workflow))
        .expect("managed fixture derives a publish workflow");
    for (kind, environment) in environment_boundaries(&conditional) {
        if let Some(existing) = boundaries.insert(kind.clone(), environment.clone()) {
            assert_eq!(existing, environment, "{kind} has one environment boundary");
        }
    }
    let page = std::fs::read_to_string("../../docs/publish-workflow.md")
        .expect("publish workflow page is readable");

    assert_eq!(
        documented_environments(&page),
        boundaries,
        "every documented environment boundary is witnessed by a repository or conditional derivation"
    );
}

#[test]
fn feature_alias_scenario_is_bound_to_both_packager_recipes() {
    let feature = std::fs::read_to_string("../../docs/features/github-release-publication.yml")
        .expect("publication feature is readable");
    let recipes = std::fs::read_to_string("../core/src/executor/workflow/tests/oci_recipes.rs")
        .expect("OCI recipe tests are readable");
    let documented = feature_scenario_bindings(&feature);
    let executable = executable_feature_scenario_bindings(&recipes);

    assert_eq!(
        documented,
        executable,
        "every documented packager branch names one executable witness and every witness names its scenario"
    );
    assert_eq!(
        documented.len(),
        2,
        "the prerelease alias scenario covers runnable images and Dev Container Features"
    );
}

#[test]
fn publish_workflow_page_states_the_readback_bound_and_both_remedies() {
    let page = std::fs::read_to_string("../../docs/publish-workflow.md")
        .expect("publish workflow page is readable");
    let workflow = std::fs::read_to_string("../core/src/executor/workflow.rs")
        .expect("workflow implementation is readable");
    let limit = workflow
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("const MAX_WORKFLOW_LINES: usize = ")
        })
        .and_then(|value| value.strip_suffix(';'))
        .expect("workflow implementation declares its line limit")
        .replace('_', ",");

    assert!(
        page.contains(&format!("at most {limit} lines")),
        "the adopter page states the implementation's workflow readback limit"
    );
    assert!(
        page.contains("Move unrelated repository jobs to another workflow"),
        "the adopter page gives a remedy for repository-owned workflow size"
    );
    assert!(
        page.contains("reduce configured publication destinations"),
        "the adopter page gives a remedy for managed publication size"
    );
}
