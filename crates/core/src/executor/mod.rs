// ---
// relationships:
//   implements: github-release-executor
// ---

//! GitHub executor configuration, recipe selection, initialization, and conformance.

pub mod check;
pub mod init;
pub mod recipe;

pub use check::{check_executor, ExecutorCheck};
pub use init::{
    initialize_executor, CandidateKind, Choice, ExecutorCandidate, ExecutorInitPlan,
    ExecutorInitResult, ExecutorInitState, ACCEPT_CHOICE, DECLINE_CHOICE, EXECUTOR_INIT_PLAN_PATH,
    EXECUTOR_INIT_PLAN_SCHEMA,
};
pub use recipe::{
    capability_set, catalog, derive_capabilities, recipes_for, select_publications, Capability,
    CapabilityEvidence, Packager, Recipe, SelectedPublication, PRIMARY_TARGET,
};

#[cfg(test)]
pub(crate) mod fixture {
    use std::path::{Path, PathBuf};

    /// Self-deleting workspace directory used by executor tests.
    pub(crate) struct Workspace(PathBuf);

    impl Workspace {
        pub(crate) fn new(label: &str) -> Self {
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

        pub(crate) fn root(&self) -> &Path {
            &self.0
        }

        pub(crate) fn write(&self, relative: &str, contents: &str) -> &Self {
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
}
