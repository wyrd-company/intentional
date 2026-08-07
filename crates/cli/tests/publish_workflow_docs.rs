// ---
// relationships:
//   tests: github-release-executor
// ---

use serde_yaml::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;

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

    assert_eq!(
        workflow.lines().count(),
        1_652,
        "the repository's derived publish workflow line count remains witnessed"
    );
    assert_eq!(
        jobs.len(),
        20,
        "the repository's derived publish workflow job count remains witnessed"
    );
    assert!(
        page.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .contains("derived publish workflow is 1,652 lines across 20 jobs"),
        "the page states the witnessed repository-scale values"
    );

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
fn publish_workflow_page_matches_derived_environment_boundaries() {
    let workflow = derived_repository_workflow();
    let document: Value = serde_yaml::from_str(&workflow).expect("derived workflow parses");
    let jobs = document["jobs"]
        .as_mapping()
        .expect("derived workflow has jobs");
    let protected = jobs
        .iter()
        .filter(|(_, body)| body.get("environment").is_some())
        .map(|(id, _)| job_kind(id.as_str().expect("job id is text")))
        .collect::<BTreeSet<_>>();
    let unprotected = jobs
        .iter()
        .filter(|(_, body)| body.get("environment").is_none())
        .map(|(id, _)| job_kind(id.as_str().expect("job id is text")))
        .collect::<BTreeSet<_>>();

    assert_eq!(
        protected,
        ["phase-before", "upload", "publisher", "close"]
            .into_iter()
            .collect(),
        "the protected job-kind population is derived from emitted jobs"
    );
    assert_eq!(
        unprotected,
        ["verify-tag", "build", "verifier", "assemble"]
            .into_iter()
            .collect(),
        "the unprotected job-kind population is derived from emitted jobs"
    );
}
