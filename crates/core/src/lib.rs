// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Core model and release operations for `itentional`.

pub mod config;
pub mod error;
pub mod init;
pub mod intent;
pub mod model;
pub mod plan;
pub mod status;
pub mod version;

pub use config::{Config, PackageConfig, Projection, Settings, CONFIG_PATH};
pub use error::{Error, Result};
pub use init::{discover_config, InitResult};
pub use intent::{Intent, IntentDraft, IntentWrite, INTENTS_PATH};
pub use model::{Adapter, Bump, ProjectionMode};
pub use plan::{
    canonical_json, render_changelog_section, ChangelogEntry, PlanPackage, ReleasePlan,
};
pub use status::{PackageStatus, WorkspaceStatus};
pub use version::{
    aggregate_bumps, bump_version, effective_bumps, PackageVersion, VersionRepository,
};

/// Version of the core release model.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn workspace_version_is_0_1_0() {
        assert_eq!(super::VERSION, "0.1.0");
    }
}
