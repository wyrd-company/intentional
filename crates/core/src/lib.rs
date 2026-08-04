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
pub mod publication;
pub mod release;
pub mod stamp;
pub mod status;
pub mod tag;
pub mod textdiff;
pub mod version;
pub mod yaml_edit;

pub use apply::{ApplyResult, FileWrite};
pub use check::check_workspace;
pub use config::{
    CargoPublisher, Config, DiscoveryConfig, DockerhubTarget, ExcludedPathReceipt, ExecutorPrefix,
    GhcrTarget, GithubConfig, GithubWorkflow, GithubWorkflows, HomebrewPublisher,
    ManagedPathReceipt, NpmAdditionalTargets, NpmGithubTarget, NpmPublisher, OciPublisher,
    PrefixNamespaces, Projection, ReleaseUnitConfig, Settings, SystemPackagePublisher, TagConfig,
    UnphasedTag, WorkflowRole, WorkspaceTagConfig, CONFIG_PATH, CURRENT_CONTRACT,
    DEFAULT_ENVVAR_PREFIX, DEFAULT_JOB_PREFIX, DEFAULT_PUBLISH_WORKFLOW, DEFAULT_RELEASE_WORKFLOW,
};
pub use error::{Error, Result};
pub use evidence::assemble::{
    assemble, AssembleRequest, Assembly, ContributionAttachmentRecord, PhaseTagEvidence,
    PublisherEvidence, ReleaseEvidence, ReleaseIdentity, WorkflowIdentity,
    PUBLISHER_EVIDENCE_SCHEMA, RELEASE_EVIDENCE_CONTRACT, RELEASE_EVIDENCE_FILE,
    RELEASE_EVIDENCE_SCHEMA,
};
pub use evidence::contribution::{
    artifact_name, contribute, ContributionAttachment, ContributionBundle, ContributionManifest,
    ContributionRequest, ATTACHMENTS_DIRECTORY, CONTRIBUTION_ARTIFACT_PREFIX,
    CONTRIBUTION_MANIFEST, CONTRIBUTION_SCHEMA, LOCAL_JOB,
};
pub use evidence::phase::{
    build_after_publication, build_before_publication, load_built_subjects,
    load_publisher_evidence, record_built_subject, BuiltSubject, PhaseBindings,
    BUILT_SUBJECT_CONTRACT, BUILT_SUBJECT_SCHEMA, PHASE_EVIDENCE_FIELD,
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
pub use executor::workflow::{
    compare_configured_workflow, compare_workflow, ComparisonStatus, WorkflowComparison,
    WorkflowDiagnostic, OWNERSHIP_SENTINEL, WORKFLOW_CONTRACT, WORKFLOW_DIFF_SCHEMA,
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
pub use publication::draft::{
    is_draft_dependent, retrieve_assets, verify_draft_handoff, write_handoff,
    AuthenticatedDraftRetrieval, DraftReleaseAssetHandoff, HandoffAsset, HandoffRequest,
    RetrievedAsset, VerifiedDraftHandoff, DRAFT_DEPENDENT_PUBLISHERS, DRAFT_HANDOFF_CONTRACT,
    DRAFT_HANDOFF_FILE, DRAFT_HANDOFF_SCHEMA,
};
pub use publication::observation::{
    observe, Clock, ConsistencyPolicy, ObservationState, PublicationObservation, SystemClock,
    PUBLICATION_OBSERVATION_CONTRACT, PUBLICATION_OBSERVATION_SCHEMA,
};
pub use publication::release::{
    verify_release, verify_release_observed, Attestation, DestinationObserver, GhReleaseSource,
    LiveObservation, ObservedPublications, ReleaseAsset, ReleaseRecord, ReleaseSource,
    ReleaseVerification,
};
pub use publication::verify::{
    verify_publication, CheckoutContext, PlannedRelease, PublicationContext, VerifiedPublication,
    VerifyPublicationRequest,
};
pub use release::build::{RELEASE_IDENTITY_EMAIL, RELEASE_IDENTITY_NAME};
pub use release::candidate::{
    BundleInventory, CandidateFile, CandidateReleaseIdentity, ChangeStatus, ChangedPath, GlobalTag,
    PlanInventory, ReleaseCandidate, SourceIdentity, BUNDLE_RELEASE_HEAD, BUNDLE_TAG_HEAD,
    CANDIDATE_TREE_DIRECTORY, IMPORTED_RELEASE_REF, LOCAL_GLOBAL_TAG_REF, MAX_BUNDLE_BYTES,
    MAX_CANDIDATE_FILES, MAX_CANDIDATE_FILE_BYTES, RELEASE_BUNDLE_FILE, RELEASE_CANDIDATE_CONTRACT,
    RELEASE_CANDIDATE_MANIFEST, RELEASE_CANDIDATE_SCHEMA, RELEASE_PLAN_FILE,
};
pub use release::prepare::{prepare_release, PreparedRelease};
pub use release::tag::{verify_release_tag, VerifiedReleaseTag};
pub use release::verify::{verify_handoff, VerifiedHandoff};
pub use stamp::StampResult;
pub use status::{
    Drift, ReleaseUnitStatus, WorkspaceStatus, MISSING_BASELINE_CODE, MISSING_BASELINE_NEXT_ACTION,
};
pub use tag::{release_tag_message, tag_record_issues, PlannedTag, TagResult};
pub use version::{
    aggregate_bumps, bump_version, bump_version_with_mapping, effective_bumps, resolve_versions,
    ReleaseUnitVersion, VersionRepository,
};

/// Version of the core release model.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
