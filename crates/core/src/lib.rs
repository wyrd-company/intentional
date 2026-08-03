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
pub mod evidence;
pub mod executor;
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
    CargoPublisher, Config, DiscoveryConfig, DockerhubTarget, ExcludedPathReceipt, ExecutorPrefix,
    GhcrTarget, GithubConfig, GithubWorkflow, GithubWorkflows, HomebrewPublisher,
    ManagedPathReceipt, NpmAdditionalTargets, NpmGithubTarget, NpmPublisher, OciPublisher,
    PrefixNamespaces, Projection, ReleaseUnitConfig, Settings, SystemPackagePublisher, TagConfig,
    WorkflowRole, WorkspaceTagConfig, CONFIG_PATH, CURRENT_CONTRACT, DEFAULT_ENVVAR_PREFIX,
    DEFAULT_JOB_PREFIX, DEFAULT_PUBLISH_WORKFLOW, DEFAULT_RELEASE_WORKFLOW,
};
pub use error::{Error, Result};
pub use evidence::contribution::{
    artifact_name, contribute, ContributionAttachment, ContributionBundle, ContributionManifest,
    ContributionRequest, ATTACHMENTS_DIRECTORY, CONTRIBUTION_ARTIFACT_PREFIX,
    CONTRIBUTION_MANIFEST, CONTRIBUTION_SCHEMA, LOCAL_JOB,
};
pub use executor::check::{check_executor, ExecutorCheck};
pub use executor::init::{
    initialize_executor, ExecutorInitPlan, ExecutorInitResult, ExecutorInitState,
    EXECUTOR_INIT_PLAN_PATH,
};
pub use executor::recipe::{
    resolve_publications, select_publications, Capability, CapabilityEvidence, Packager,
    PublicationSelection, Recipe, SelectedPublication,
};
pub use init::{
    discover_config, initialize, CandidateProjectionSuggestion, CandidateResolution,
    CandidateTagSuggestion, ConvertedIntent, DiscoveryCandidate, ExtractionDiagnostic,
    InitDiagnostic, InitPlan, InitResult, InitState, ParityReleaseUnit, ParityResult,
    RawVersionEvidence, SourceEvidence, INIT_PLAN_PATH,
};
pub use intent::{Intent, IntentDraft, IntentWrite, INTENTS_PATH};
pub use model::{
    Adapter, AttachedComponent, Bump, Pre1BumpMapping, ProjectionMode, PublisherKind,
    ReleaseUnitDisposition, TagPhase, TagRole,
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
