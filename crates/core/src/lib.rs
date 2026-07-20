// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Core model and release operations for `intentional`.

pub mod adapters;
pub mod apply;
pub mod check;
pub mod config;
pub mod error;
pub mod init;
pub mod intent;
pub mod model;
pub mod plan;
pub mod stamp;
pub mod status;
pub mod tag;
pub mod version;

pub use apply::{ApplyResult, FileWrite};
pub use check::check_workspace;
pub use config::{Config, PackageConfig, Projection, Settings, CONFIG_PATH};
pub use error::{Error, Result};
pub use init::{discover_config, InitResult};
pub use intent::{Intent, IntentDraft, IntentWrite, INTENTS_PATH};
pub use model::{Adapter, Bump, ProjectionMode};
pub use plan::{
    canonical_json, render_changelog_section, ChangelogEntry, PlanPackage, ReleasePlan,
};
pub use stamp::StampResult;
pub use status::{Drift, PackageStatus, WorkspaceStatus};
pub use tag::TagResult;
pub use version::{
    aggregate_bumps, bump_version, effective_bumps, PackageVersion, VersionRepository,
};

/// Version of the core release model.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
