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
mod ownership;
pub mod plan;
pub mod projection_file;
pub mod stamp;
pub mod status;
pub mod tag;
pub mod version;
mod workspace_files;

pub use apply::{ApplyResult, FileWrite};
pub use check::check_workspace;
pub use config::{
    Config, DiscoveryConfig, ExcludedPathReceipt, ManagedPathReceipt, Projection, ProjectionFile,
    ReleaseUnitConfig, Settings, TagConfig, WorkspaceTagConfig, CONFIG_PATH, CONTRACT_1,
    CURRENT_CONTRACT,
};
pub use error::{Error, Result};
pub use init::{
    discover_config, initialize, initialize_with, CandidateProjectionSuggestion,
    CandidateResolution, CandidateTagSuggestion, ConvertedIntent, DiscoveryCandidate,
    ExtractionDiagnostic, InitDiagnostic, InitOptions, InitPlan, InitResult, InitState,
    ParityReleaseUnit, ParityResult, RawVersionEvidence, SourceEvidence, INIT_PLAN_PATH,
};
pub use intent::{Intent, IntentDraft, IntentWrite, INTENTS_PATH};
pub use model::{
    Adapter, Bump, BumpTranslation, ProjectionMode, ReleaseUnitDisposition, TagPhase, TagRole,
};
pub use plan::{
    canonical_json, render_changelog_section, sha256_digest, ChangelogEntry, Generator,
    PlanReleaseUnit, PlanTag, ReleasePlan,
};
pub use stamp::StampResult;
pub use status::{
    Drift, ReleaseUnitStatus, WorkspaceStatus, MISSING_BASELINE_CODE, MISSING_BASELINE_NEXT_ACTION,
};
pub use tag::{tag_record_issues, PlannedTag, TagResult};
pub use version::{
    aggregate_bumps, aggregate_graduations, bump_version, effective_bumps, resolve_versions,
    ReleaseUnitVersion, VersionRepository,
};

/// Version of the core release model.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
