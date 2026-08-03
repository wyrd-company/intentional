// ---
// relationships:
//   implements: github-release-executor
// ---

//! GitHub executor configuration, recipe selection, initialization, and conformance.

pub mod check;
pub mod init;
pub mod recipe;
mod steps;
pub mod workflow;

pub use check::{check_executor, ExecutorCheck};
pub use init::{
    initialize_executor, CandidateKind, Choice, ExecutorCandidate, ExecutorInitPlan,
    ExecutorInitResult, ExecutorInitState, ACCEPT_CHOICE, DECLINE_CHOICE, EXECUTOR_INIT_PLAN_PATH,
    EXECUTOR_INIT_PLAN_SCHEMA,
};
pub use workflow::{
    compare_configured_workflow, compare_workflow, ComparisonStatus, WorkflowComparison,
    WorkflowDiagnostic, OWNERSHIP_SENTINEL, WORKFLOW_CONTRACT, WORKFLOW_DIFF_SCHEMA,
};

pub use recipe::{
    capability_set, catalog, derive_capabilities, recipes_for, resolve_publications,
    select_publications, Capability, CapabilityEvidence, Packager, PublicationSelection, Recipe,
    SelectedPublication, PRIMARY_TARGET,
};

/// Workspace and workflow-derivation fixtures shared by executor tests.
///
/// Workflow derivation is exercised from three crates' tests: this crate's own
/// reconciliation tests, the command-line crate's invocation-binding gate, and
/// its end-to-end release-handoff test. All three need the same representative
/// workspace, and a second copy of it is a second thing to keep in step with
/// the contract, so the fixture is published under the `test-support` feature
/// rather than transcribed per crate. The feature is enabled only by
/// dev-dependencies, so nothing here reaches a released binary.
#[cfg(any(test, feature = "test-support"))]
pub mod fixture {
    use std::path::{Path, PathBuf};

    /// Self-deleting workspace directory used by executor tests.
    pub struct Workspace(PathBuf);

    impl Workspace {
        #[must_use]
        pub fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "intentional-executor-{label}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("system time")
                    .as_nanos()
            ));
            std::fs::create_dir_all(&path).expect("create workspace");
            Self(path)
        }

        #[must_use]
        pub fn root(&self) -> &Path {
            &self.0
        }

        pub fn write(&self, relative: &str, contents: &str) -> &Self {
            let path = self.0.join(relative);
            std::fs::create_dir_all(path.parent().expect("parent directory"))
                .expect("create parent directory");
            std::fs::write(&path, contents).expect("write fixture file");
            self
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Executor configuration that derives both managed workflows.
    ///
    /// One release unit with one publishable adapter is the smallest
    /// configuration that still derives every managed job: preparation, the
    /// authority transition, tag verification, one subject build, both phase
    /// tags, one publisher, evidence assembly, and closure.
    pub const MANAGED_CONFIG: &str = r#"$schema: https://intentional.foo/schemas/config.yml
contract: contract-1
workspace-tags:
  release:
    template: '{version}'
github:
  workflows:
    release: { path: .github/workflows/release.yml }
    publish: { path: .github/workflows/publish.yml }
release-units:
  component:
    path: component
    cargo: {}
    tags:
      primary: { role: primary, template: '{id}@{version}', require-phase: after-publication }
      staged: { role: projection, template: '{id}/staged@{version}', require-phase: before-publication }
"#;

    /// Repository-owned workflow the managed slices are spliced into.
    const REPOSITORY_WORKFLOW: &str = "name: managed\n\non:\n  workflow_dispatch:\n\njobs:\n  repository_job:\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n";

    /// A representative workspace both managed workflow roles derive from.
    #[must_use]
    pub fn managed_workspace(label: &str) -> Workspace {
        let workspace = Workspace::new(label);
        workspace
            .write(".intentional/config.yml", MANAGED_CONFIG)
            .write(
                "component/Cargo.toml",
                "[package]\nname = \"example-component\"\nversion = \"1.0.0\"\n",
            )
            .write(".github/workflows/release.yml", REPOSITORY_WORKFLOW)
            .write(".github/workflows/publish.yml", REPOSITORY_WORKFLOW);
        workspace
    }

    /// Reconcile that workspace and return what each role's workflow became.
    ///
    /// The workflows are derived rather than transcribed, so every caller reads
    /// exactly what a repository would receive from this build.
    #[must_use]
    pub fn derived_workflows(label: &str) -> Vec<(crate::config::WorkflowRole, String)> {
        let workspace = managed_workspace(label);
        let root = workspace.root();
        crate::config::WorkflowRole::ALL
            .into_iter()
            .map(|role| {
                let comparison =
                    super::compare_workflow(root, role, None).expect("comparison runs");
                assert_eq!(
                    comparison.status,
                    super::ComparisonStatus::Different,
                    "the {role} contract must derive: {:?}",
                    comparison.diagnostics
                );
                let applied = comparison.apply().expect("transformation applies");
                assert!(applied.applied);
                (
                    role,
                    std::fs::read_to_string(root.join(&applied.path)).expect("derived workflow"),
                )
            })
            .collect()
    }
}
