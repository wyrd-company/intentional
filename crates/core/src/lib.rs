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
pub use config::{
    Config, DiscoveryConfig, ExcludedPathReceipt, ManagedPathReceipt, Projection,
    ReleaseUnitConfig, Settings, TagConfig, WorkspaceTagConfig, CONFIG_PATH, CURRENT_CONTRACT,
};
pub use error::{Error, Result};
pub use init::{
    discover_config, initialize, CandidateProjectionSuggestion, CandidateResolution,
    CandidateTagSuggestion, ConvertedIntent, DiscoveryCandidate, ExtractionDiagnostic,
    InitDiagnostic, InitPlan, InitResult, InitState, ParityReleaseUnit, ParityResult,
    RawVersionEvidence, SourceEvidence, INIT_PLAN_PATH,
};
pub use intent::{Intent, IntentDraft, IntentWrite, INTENTS_PATH};
pub use model::{
    Adapter, Bump, Pre1BumpMapping, ProjectionMode, ReleaseUnitDisposition, TagPhase, TagRole,
};
pub use plan::{
    canonical_json, render_changelog_section, ChangelogEntry, Generator, PlanReleaseUnit, PlanTag,
    ReleasePlan,
};
pub use stamp::StampResult;
pub use status::{
    Drift, ReleaseUnitStatus, WorkspaceStatus, MISSING_BASELINE_CODE, MISSING_BASELINE_NEXT_ACTION,
};
pub use tag::{tag_record_issues, PlannedTag, TagResult};
pub use version::{
    aggregate_bumps, bump_version, bump_version_with_mapping, effective_bumps, resolve_versions,
    ReleaseUnitVersion, VersionRepository,
};

/// Version of the core release model.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
