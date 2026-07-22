// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Workspace-aware initialization and explicit Changesets takeover.

use crate::config::{
    validate_detector_id, validate_exact_discovery_path, validate_sha256, validate_tag_template,
    Config, ExcludedPathReceipt, ManagedPathReceipt, Projection, ReleaseUnitConfig, TagConfig,
    WorkspaceTagConfig, CONFIG_PATH,
};
use crate::error::{Error, Result};
use crate::model::{
    Adapter, Bump, Pre1BumpMapping, ProjectionMode, ReleaseUnitDisposition, TagPhase, TagRole,
};
use crate::plan::canonical_json;
use crate::version::{
    bump_version_with_mapping, effective_bumps, resolve_versions, ReleaseUnitVersion,
};
use glob::glob;
use node_semver::{Range as NodeRange, Version as NodeVersion};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use walkdir::{DirEntry, WalkDir};

/// Transient initialization-plan location.
pub const INIT_PLAN_PATH: &str = ".intentional/init-plan.yml";

/// Published initialization-plan schema identifier.
pub const INIT_PLAN_SCHEMA: &str = "https://intentional.foo/schemas/init-plan.yml";

const CHANGESETS_CONFIG: &str = ".changeset/config.json";
const TRANSACTION_PATH: &str = ".intentional/.takeover-transaction";
const TRANSACTION_STATE_PATH: &str = ".intentional/.takeover-state";

/// Process outcome for initialization.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum InitState {
    /// Canonical configuration was produced, or takeover completed.
    Success,
    /// A deterministic plan exists but needs finite resolutions or repository edits.
    NeedsInput,
    /// A Changesets plan is complete and awaits explicit takeover.
    Ready,
}

/// One source artifact used as initialization evidence.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SourceEvidence {
    /// Workspace-relative source path.
    pub path: PathBuf,
    /// SHA-256 of the complete file contents.
    pub digest: String,
    /// Relevant one-based source lines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<usize>,
}

/// Optional raw version text together with the evidence from which it was extracted.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct RawVersionEvidence {
    /// Uninterpreted version text supplied by the artifact.
    pub value: String,
    /// Exact evidence supporting the extracted text.
    pub evidence: Vec<SourceEvidence>,
}

/// A projection a detector can suggest when its required fields were extracted.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CandidateProjectionSuggestion {
    /// Adapter specialization.
    pub adapter: Adapter,
    /// Exact workspace-relative projection path.
    pub path: PathBuf,
    /// Projection materialization mode.
    pub mode: ProjectionMode,
    /// JSON Pointer or dotted TOML/YAML key path for generic formats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
}

/// A tag a detector can suggest without granting it version authority.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct CandidateTagSuggestion {
    /// Suggested tag id within the resolved release unit.
    pub id: String,
    /// Suggested primary or projection role.
    pub role: TagRole,
    /// Suggested template containing exactly one `{version}` placeholder.
    pub template: String,
}

/// Evidence-backed extraction problem that does not assert artifact invalidity.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ExtractionDiagnostic {
    /// Stable diagnostic identity within the candidate.
    pub id: String,
    /// Stable extraction category.
    pub code: String,
    /// Human-readable extraction problem.
    pub message: String,
    /// Exact evidence encountered by the detector.
    pub evidence: Vec<SourceEvidence>,
}

/// Explicit user or agent resolution for one discovery candidate.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum CandidateResolution {
    /// Create one independently versioned release unit from this candidate.
    Independent {
        /// Stable id for the new release unit.
        release_unit: String,
    },
    /// Add this candidate as a projection of a release unit.
    Projection {
        /// Final configured or planned release-unit id.
        release_unit: String,
        /// Planned candidate to follow; omitted when the release unit is already configured.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_candidate: Option<String>,
    },
    /// Keep this exact detector/path evidence outside the release-unit inventory.
    Excluded,
}

/// Artifact-neutral evidence emitted by any release-unit detector.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DiscoveryCandidate {
    /// Stable hash of detector and exact path identity.
    pub id: String,
    /// Stable detector id, independent from adapter or ecosystem names.
    pub detector: String,
    /// Exact workspace-relative artifact path.
    pub path: PathBuf,
    /// All evidence used to create this candidate.
    pub evidence: Vec<SourceEvidence>,
    /// Manifest-native identity, when extraction succeeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_identity: Option<String>,
    /// Uninterpreted version evidence, when present and readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_version: Option<RawVersionEvidence>,
    /// Projection suggestion, only when the detector has enough evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub projection: Option<CandidateProjectionSuggestion>,
    /// Tag suggestion, only when the detector has enough evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<CandidateTagSuggestion>,
    /// Narrow extraction diagnostics; these do not validate publication or artifact completeness.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<ExtractionDiagnostic>,
    /// Editable independent, projection, or excluded choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<CandidateResolution>,
}

impl DiscoveryCandidate {
    /// Derive the stable candidate id from detector and exact path identity.
    pub fn stable_id(detector: &str, path: &Path) -> Result<String> {
        validate_detector_id(detector)?;
        validate_exact_discovery_path(path, "discovery candidate path")?;
        let mut path_parts = Vec::new();
        for component in path.components() {
            if let std::path::Component::Normal(value) = component {
                path_parts.push(value.to_str().ok_or_else(|| {
                    Error::Validation("discovery candidate path must be valid UTF-8".to_owned())
                })?);
            }
        }
        let path = path_parts.join("/");
        let mut identity = Sha256::new();
        identity.update(detector.as_bytes());
        identity.update([0]);
        identity.update(path.as_bytes());
        Ok(format!("candidate:{:x}", identity.finalize()))
    }

    fn validate(&self) -> Result<()> {
        let expected = Self::stable_id(&self.detector, &self.path)?;
        if self.id != expected {
            return Err(Error::Validation(format!(
                "discovery candidate {} does not match detector {} and path {}",
                self.id,
                self.detector,
                self.path.display()
            )));
        }
        if self.evidence.is_empty() {
            return Err(Error::Validation(format!(
                "discovery candidate {} must contain evidence",
                self.id
            )));
        }
        validate_source_evidence(&self.evidence, &format!("discovery candidate {}", self.id))?;
        if !self.evidence.iter().any(|item| item.path == self.path) {
            return Err(Error::Validation(format!(
                "discovery candidate {} evidence must include its exact path {}",
                self.id,
                self.path.display()
            )));
        }
        if self.native_identity.as_deref().is_some_and(str::is_empty) {
            return Err(Error::Validation(format!(
                "discovery candidate {} native identity must not be empty",
                self.id
            )));
        }
        if let Some(version) = &self.raw_version {
            if version.value.is_empty() {
                return Err(Error::Validation(format!(
                    "discovery candidate {} raw version must not be empty",
                    self.id
                )));
            }
            validate_source_evidence(
                &version.evidence,
                &format!("discovery candidate {} raw version", self.id),
            )?;
            if !version
                .evidence
                .iter()
                .all(|item| self.evidence.contains(item))
            {
                return Err(Error::Validation(format!(
                    "discovery candidate {} raw version references evidence outside the candidate",
                    self.id
                )));
            }
        }
        if let Some(projection) = &self.projection {
            validate_exact_discovery_path(&projection.path, "candidate projection path")?;
            if !self
                .evidence
                .iter()
                .any(|item| item.path == projection.path)
            {
                return Err(Error::Validation(format!(
                    "discovery candidate {} projection path lacks candidate evidence",
                    self.id
                )));
            }
            if projection.adapter.requires_pointer()
                && projection.pointer.as_deref().is_none_or(str::is_empty)
            {
                return Err(Error::Validation(format!(
                    "discovery candidate {} generic projection requires a pointer",
                    self.id
                )));
            }
        }
        if let Some(tag) = &self.tag {
            if tag.id.is_empty()
                || !tag
                    .id
                    .chars()
                    .next()
                    .is_some_and(|character| character.is_ascii_alphanumeric())
                || !tag
                    .id
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
            {
                return Err(Error::Validation(format!(
                    "discovery candidate {} has invalid tag id {:?}",
                    self.id, tag.id
                )));
            }
            validate_tag_template(
                &format!("discovery candidate {} tag", self.id),
                &tag.template,
                true,
            )?;
        }
        let mut diagnostic_ids = BTreeSet::new();
        for diagnostic in &self.diagnostics {
            if diagnostic.id.is_empty()
                || diagnostic.code.is_empty()
                || diagnostic.message.is_empty()
            {
                return Err(Error::Validation(format!(
                    "discovery candidate {} has an incomplete extraction diagnostic",
                    self.id
                )));
            }
            if !diagnostic_ids.insert(&diagnostic.id) {
                return Err(Error::Validation(format!(
                    "discovery candidate {} repeats extraction diagnostic {}",
                    self.id, diagnostic.id
                )));
            }
            validate_source_evidence(
                &diagnostic.evidence,
                &format!(
                    "discovery candidate {} diagnostic {}",
                    self.id, diagnostic.id
                ),
            )?;
            if !diagnostic
                .evidence
                .iter()
                .all(|item| self.evidence.contains(item))
            {
                return Err(Error::Validation(format!(
                    "discovery candidate {} diagnostic {} references evidence outside the candidate",
                    self.id, diagnostic.id
                )));
            }
        }
        Ok(())
    }
}

/// One detector invocation before its candidates are flattened into an initialization plan.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct DetectorResult {
    /// Stable detector id.
    pub detector: String,
    /// Candidates emitted in stable path order.
    pub candidates: Vec<DiscoveryCandidate>,
}

impl DetectorResult {
    /// Validate detector identity and candidate ownership.
    pub fn validate(&self) -> Result<()> {
        validate_detector_id(&self.detector)?;
        let mut paths = BTreeSet::new();
        for candidate in &self.candidates {
            if candidate.detector != self.detector {
                return Err(Error::Validation(format!(
                    "detector {} returned candidate owned by {}",
                    self.detector, candidate.detector
                )));
            }
            candidate.validate()?;
            if !paths.insert(&candidate.path) {
                return Err(Error::Validation(format!(
                    "detector {} repeated candidate path {}",
                    self.detector,
                    candidate.path.display()
                )));
            }
        }
        Ok(())
    }

    /// Return candidates in deterministic path order for plan serialization.
    pub fn into_candidates(mut self) -> Result<Vec<DiscoveryCandidate>> {
        self.validate()?;
        self.candidates
            .sort_by(|left, right| left.path.cmp(&right.path));
        Ok(self.candidates)
    }
}

fn validate_source_evidence(evidence: &[SourceEvidence], description: &str) -> Result<()> {
    if evidence.is_empty() {
        return Err(Error::Validation(format!(
            "{description} must contain evidence"
        )));
    }
    let mut paths = BTreeSet::new();
    if evidence.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(Error::Validation(format!(
            "{description} evidence must be ordered by path"
        )));
    }
    for item in evidence {
        validate_exact_discovery_path(&item.path, &format!("{description} evidence path"))?;
        validate_sha256(&item.digest, &format!("{description} evidence digest"))?;
        if !paths.insert(&item.path) {
            return Err(Error::Validation(format!(
                "{description} repeats evidence path {}",
                item.path.display()
            )));
        }
        if item.lines.contains(&0) {
            return Err(Error::Validation(format!(
                "{description} evidence lines must be one-based"
            )));
        }
        let unique_lines = item.lines.iter().collect::<BTreeSet<_>>();
        if unique_lines.len() != item.lines.len() {
            return Err(Error::Validation(format!(
                "{description} repeats an evidence line for {}",
                item.path.display()
            )));
        }
        if item.lines.windows(2).any(|pair| pair[0] >= pair[1]) {
            return Err(Error::Validation(format!(
                "{description} evidence lines for {} must be ordered",
                item.path.display()
            )));
        }
    }
    Ok(())
}

/// One finite initialization decision or warning.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct InitDiagnostic {
    /// Stable diagnostic identity.
    pub id: String,
    /// Stable diagnostic category.
    pub code: String,
    /// Actionable human-readable explanation.
    pub message: String,
    /// Exact evidence supporting the diagnostic.
    pub evidence: Vec<SourceEvidence>,
    /// Finite resolution values. Empty means informational.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    /// Parity-preserving or boundary-preserving recommendation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended: Option<String>,
    /// Editable user/agent resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<String>,
    /// Whether repository state proves the selected resolution.
    #[serde(default)]
    pub verified: bool,
    /// Whether a prior resolution was discarded because its evidence became stale.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub invalidated_resolution: bool,
}

/// One side of a Changesets takeover parity comparison.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ParityRelease {
    /// Aggregate requested bump after dependency and group propagation.
    pub requested_bump: Bump,
    /// Resulting version under that authority's contract.
    pub next_version: String,
}

/// Changesets and proposed release results used for takeover parity.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ParityReleaseUnit {
    /// Release-unit id.
    pub release_unit: String,
    /// Version in source projections.
    pub current_version: String,
    /// Independently computed Changesets result, when the source releases this release unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ParityRelease>,
    /// Proposed Intentional result, when the proposed contract releases this release unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed: Option<ParityRelease>,
}

/// Deterministic parity comparison.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ParityResult {
    /// `equivalent` when all logical releases agree; otherwise `blocked`.
    pub status: String,
    /// Per-release-unit results.
    pub release_units: Vec<ParityReleaseUnit>,
}

/// One pending Changesets file converted losslessly at takeover.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ConvertedIntent {
    /// Changeset filename stem retained as the Intentional intent id.
    pub id: String,
    /// Release-unit bumps.
    pub release_units: BTreeMap<String, Bump>,
    /// Markdown body, excluding Changesets frontmatter delimiters.
    pub message: String,
    /// Original Changesets file.
    pub source: PathBuf,
    /// Intentional target file.
    pub target: PathBuf,
}

impl ConvertedIntent {
    fn contents(&self) -> Result<String> {
        Ok(format!(
            "---\n{}---\n\n{}\n",
            serde_yaml::to_string(&self.release_units)?,
            self.message.trim()
        ))
    }
}

/// Durable, editable handoff produced while Changesets still owns authority.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct InitPlan {
    /// Published schema location.
    #[serde(rename = "$schema")]
    pub schema: String,
    /// Computed state. Editing this field never bypasses validation.
    pub state: InitState,
    /// Existing authority kind.
    pub source_kind: String,
    /// Digest over all current source evidence.
    pub source_fingerprint: String,
    /// Complete confidently inferred canonical configuration.
    pub inferred_config: Config,
    /// Artifact-neutral candidates awaiting or carrying explicit resolutions.
    pub discovery_candidates: Vec<DiscoveryCandidate>,
    /// Finite choices, warnings, and verified repository integration outcomes.
    pub diagnostics: Vec<InitDiagnostic>,
    /// Pending source intents and their lossless targets.
    pub converted_intents: Vec<ConvertedIntent>,
    /// Independent source/proposed release comparison.
    pub parity: ParityResult,
    /// Exact operations an explicit takeover performs.
    pub planned_operations: Vec<String>,
    /// Required command after the takeover changes are committed.
    pub post_commit_action: String,
}

impl InitPlan {
    /// Validate candidate evidence and its projection resolution graph.
    pub fn validate(&self) -> Result<()> {
        if self.inferred_config.release_units.is_empty() {
            if self.state != InitState::NeedsInput || self.discovery_candidates.is_empty() {
                return Err(Error::Validation(
                    "an empty inferred config requires unresolved discovery candidates".to_owned(),
                ));
            }
        } else {
            self.inferred_config.validate()?;
        }
        let mut candidates = BTreeMap::new();
        for candidate in &self.discovery_candidates {
            candidate.validate()?;
            if candidates
                .insert(candidate.id.as_str(), candidate)
                .is_some()
            {
                return Err(Error::Validation(format!(
                    "duplicate discovery candidate {}",
                    candidate.id
                )));
            }
        }

        let mut creators = self
            .inferred_config
            .release_units
            .keys()
            .map(|id| (id.as_str(), "configured release unit"))
            .collect::<BTreeMap<_, _>>();
        let mut edges = BTreeMap::<&str, &str>::new();
        for candidate in &self.discovery_candidates {
            match &candidate.resolution {
                Some(CandidateResolution::Independent { release_unit }) => {
                    validate_resolution_release_unit(release_unit)?;
                    if let Some(previous) = creators.insert(release_unit, candidate.id.as_str()) {
                        return Err(Error::Validation(format!(
                            "duplicate creator for release unit {release_unit}: {previous} and {}",
                            candidate.id
                        )));
                    }
                }
                Some(CandidateResolution::Projection {
                    release_unit,
                    target_candidate,
                }) => {
                    validate_resolution_release_unit(release_unit)?;
                    if let Some(target) = target_candidate {
                        if !candidates.contains_key(target.as_str()) {
                            return Err(Error::Validation(format!(
                                "discovery candidate {} projects onto absent candidate {target}",
                                candidate.id
                            )));
                        }
                        edges.insert(candidate.id.as_str(), target.as_str());
                    } else if !self
                        .inferred_config
                        .release_units
                        .contains_key(release_unit)
                    {
                        return Err(Error::Validation(format!(
                            "discovery candidate {} projects onto absent configured release unit {release_unit}",
                            candidate.id
                        )));
                    }
                }
                Some(CandidateResolution::Excluded) | None => {}
            }
        }

        validate_candidate_graph_acyclic(&edges)?;
        for candidate in &self.discovery_candidates {
            let Some(CandidateResolution::Projection {
                release_unit,
                target_candidate: Some(target),
            }) = &candidate.resolution
            else {
                continue;
            };
            validate_projection_target(candidate, release_unit, target, &candidates)?;
        }
        Ok(())
    }

    /// Serialize deterministically with compact, explained YAML enum choices.
    pub fn to_yaml(&self) -> Result<String> {
        self.validate()?;
        let yaml = serde_yaml::to_string(self)?;
        Ok(annotate_choice_lines(&yaml))
    }

    /// Equivalent structured JSON for agent consumers.
    pub fn to_json(&self) -> Result<String> {
        self.validate()?;
        canonical_json(self)
    }
}

fn validate_resolution_release_unit(id: &str) -> Result<()> {
    if id.is_empty()
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.@/".contains(character))
    {
        return Err(Error::Validation(format!(
            "invalid candidate resolution release-unit id {id:?}"
        )));
    }
    Ok(())
}

fn validate_candidate_graph_acyclic(edges: &BTreeMap<&str, &str>) -> Result<()> {
    fn visit<'a>(
        id: &'a str,
        edges: &BTreeMap<&'a str, &'a str>,
        visiting: &mut BTreeSet<&'a str>,
        visited: &mut BTreeSet<&'a str>,
    ) -> Result<()> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id) {
            return Err(Error::Validation(format!(
                "discovery candidate projection cycle includes {id}"
            )));
        }
        if let Some(target) = edges.get(id) {
            visit(target, edges, visiting, visited)?;
        }
        visiting.remove(id);
        visited.insert(id);
        Ok(())
    }

    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for id in edges.keys() {
        visit(id, edges, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn validate_projection_target(
    source: &DiscoveryCandidate,
    release_unit: &str,
    target: &str,
    candidates: &BTreeMap<&str, &DiscoveryCandidate>,
) -> Result<()> {
    let target = candidates[target];
    match &target.resolution {
        Some(CandidateResolution::Independent {
            release_unit: target_release_unit,
        }) => {
            if target_release_unit != release_unit {
                return Err(Error::Validation(format!(
                    "discovery candidate {} resolves to {release_unit} but target {} creates {target_release_unit}",
                    source.id, target.id
                )));
            }
            Ok(())
        }
        Some(CandidateResolution::Projection {
            release_unit: _,
            target_candidate: _,
        }) => Err(Error::Validation(format!(
            "discovery candidate {} projects onto target {} that is not an independent creator",
            source.id, target.id
        ))),
        Some(CandidateResolution::Excluded) | None => Err(Error::Validation(format!(
            "discovery candidate {} projects onto target {} without an independent or configured resolution",
            source.id, target.id
        ))),
    }
}

/// Planned initialization or takeover output.
#[derive(Debug, Clone)]
pub struct InitResult {
    /// Computed process outcome.
    pub state: InitState,
    /// Primary output path.
    pub path: PathBuf,
    /// Human-readable/schema-backed file contents.
    pub contents: String,
    /// Exact operations.
    pub operations: Vec<String>,
    /// Structured plan when adopting Changesets.
    pub plan: Option<InitPlan>,
    writes: Vec<(PathBuf, String)>,
    deletes: Vec<PathBuf>,
    takeover: bool,
    takeover_evidence: Option<(String, BTreeSet<PathBuf>)>,
}

impl InitResult {
    /// Materialize initialization unless this is a dry run.
    pub fn apply(&self, root: &Path, dry_run: bool) -> Result<()> {
        if dry_run {
            return Ok(());
        }
        if self.takeover {
            let (expected_fingerprint, evidence_paths) = self
                .takeover_evidence
                .as_ref()
                .expect("takeover carries evidence");
            verify_takeover_preconditions(
                root,
                &self.writes,
                expected_fingerprint,
                evidence_paths,
            )?;
            return apply_takeover_transaction(root, &self.writes, &self.deletes);
        }
        for (relative, contents) in &self.writes {
            let path = root.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
            }
            std::fs::write(&path, contents).map_err(|error| Error::io(&path, error))?;
        }
        for relative in &self.deletes {
            let path = root.join(relative);
            if path.is_file() {
                std::fs::remove_file(&path).map_err(|error| Error::io(&path, error))?;
            }
        }
        if self.state == InitState::Success {
            std::fs::create_dir_all(root.join(crate::intent::INTENTS_PATH))
                .map_err(|error| Error::io(root.join(crate::intent::INTENTS_PATH), error))?;
        }
        Ok(())
    }

    /// Structured output matching the written configuration or initialization plan.
    pub fn to_json(&self) -> Result<String> {
        match &self.plan {
            Some(plan) => plan.to_json(),
            None => {
                let config = Config::from_yaml(&self.contents)?;
                canonical_json(&config)
            }
        }
    }
}

/// Discover or reconcile initialization, optionally executing explicit takeover.
pub fn initialize(root: &Path, scan_all: bool, take_over: bool) -> Result<InitResult> {
    let root = root
        .canonicalize()
        .map_err(|error| Error::io(root, error))?;
    let root = root.as_path();
    recover_interrupted_takeover(root)?;
    if root.join(CHANGESETS_CONFIG).exists() {
        return changesets_plan(root, scan_all, take_over);
    }
    if take_over {
        return Err(Error::Validation(
            "--take-over requires an existing .changeset/config.json".to_owned(),
        ));
    }
    ordinary_plan(root, scan_all)
}

/// Compatibility wrapper for callers performing ordinary initialization.
pub fn discover_config(root: &Path) -> Result<InitResult> {
    initialize(root, false, false)
}

#[derive(Default)]
struct Discovery {
    config: Config,
    versions: BTreeMap<String, Version>,
    evidence: BTreeSet<PathBuf>,
    workspace_packages: BTreeSet<String>,
    private_packages: BTreeSet<String>,
    npm_dependencies: Vec<NpmDependencyEdge>,
    candidates: Vec<DiscoveryCandidate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NpmDependencyKind {
    Dependency,
    Optional,
    Peer,
    Dev,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NpmDependencyEdge {
    dependent: String,
    dependency: String,
    kind: NpmDependencyKind,
    range: String,
}

struct ChangesetsSourceSemantics<'a> {
    private_packages: &'a BTreeSet<String>,
    suppress_private_versions: bool,
    npm_dependencies: &'a [NpmDependencyEdge],
    internal_dependency_bump: Bump,
    update_internal_dependents_always: bool,
    only_update_peer_dependents_when_out_of_range: bool,
    preflight_error: Option<String>,
}

fn ordinary_plan(root: &Path, scan_all: bool) -> Result<InitResult> {
    let previous = load_previous_plan(root)?;
    let configured = if root.join(CONFIG_PATH).is_file() {
        Config::load(root)?
    } else {
        Config::default()
    };
    let mut discovery = discover(root, scan_all, &BTreeSet::new())?;
    reconcile_candidate_resolutions(&mut discovery.candidates, previous.as_ref());
    let mut candidates = unresolved_by_receipts(&configured, discovery.candidates);

    if candidates.is_empty() {
        if configured.release_units.is_empty() {
            return Err(Error::Validation(
                "no supported workspace manifests found".to_owned(),
            ));
        }
        let contents = configured.to_yaml()?;
        return Ok(InitResult {
            state: InitState::Success,
            path: PathBuf::from(CONFIG_PATH),
            operations: Vec::new(),
            contents,
            plan: None,
            writes: Vec::new(),
            deletes: Vec::new(),
            takeover: false,
            takeover_evidence: None,
        });
    }

    candidates.sort_by(|left, right| left.id.cmp(&right.id));
    let all_resolved = candidates
        .iter()
        .all(|candidate| candidate.resolution.is_some());
    if all_resolved {
        let applied = apply_candidate_resolutions(root, configured, &candidates)?;
        let contents = applied.to_yaml()?;
        return Ok(InitResult {
            state: InitState::Success,
            path: PathBuf::from(CONFIG_PATH),
            operations: vec![
                format!("write {CONFIG_PATH}"),
                format!("create {}", crate::intent::INTENTS_PATH),
                format!("delete {INIT_PLAN_PATH}"),
            ],
            contents: contents.clone(),
            plan: None,
            writes: vec![(PathBuf::from(CONFIG_PATH), contents)],
            deletes: vec![PathBuf::from(INIT_PLAN_PATH)],
            takeover: false,
            takeover_evidence: None,
        });
    }

    let evidence_paths = candidates
        .iter()
        .flat_map(|candidate| candidate.evidence.iter().map(|item| item.path.clone()))
        .collect::<BTreeSet<_>>();
    let plan = InitPlan {
        schema: INIT_PLAN_SCHEMA.to_owned(),
        state: InitState::NeedsInput,
        source_kind: if root.join(CONFIG_PATH).is_file() {
            "intentional".to_owned()
        } else {
            "workspace".to_owned()
        },
        source_fingerprint: fingerprint(root, &evidence_paths)?,
        inferred_config: configured,
        discovery_candidates: candidates,
        diagnostics: Vec::new(),
        converted_intents: Vec::new(),
        parity: ParityResult {
            status: "equivalent".to_owned(),
            release_units: Vec::new(),
        },
        planned_operations: vec![
            format!("write {CONFIG_PATH}"),
            format!("create {}", crate::intent::INTENTS_PATH),
            format!("delete {INIT_PLAN_PATH}"),
        ],
        post_commit_action: String::new(),
    };
    let contents = plan.to_yaml()?;
    Ok(InitResult {
        state: InitState::NeedsInput,
        path: PathBuf::from(INIT_PLAN_PATH),
        operations: vec![format!("write {INIT_PLAN_PATH}")],
        contents: contents.clone(),
        plan: Some(plan),
        writes: vec![(PathBuf::from(INIT_PLAN_PATH), contents)],
        deletes: Vec::new(),
        takeover: false,
        takeover_evidence: None,
    })
}

fn reconcile_candidate_resolutions(
    candidates: &mut [DiscoveryCandidate],
    previous: Option<&InitPlan>,
) {
    let Some(previous) = previous else {
        return;
    };
    let old = previous
        .discovery_candidates
        .iter()
        .map(|candidate| (candidate.id.as_str(), candidate))
        .collect::<BTreeMap<_, _>>();
    for candidate in candidates {
        if let Some(previous) = old.get(candidate.id.as_str()) {
            if previous.evidence == candidate.evidence
                && previous.native_identity == candidate.native_identity
                && previous.projection == candidate.projection
                && previous.tag == candidate.tag
            {
                candidate.resolution = previous.resolution.clone();
            }
        }
    }
}

fn unresolved_by_receipts(
    config: &Config,
    candidates: Vec<DiscoveryCandidate>,
) -> Vec<DiscoveryCandidate> {
    let managed = config
        .discovery
        .managed_paths
        .iter()
        .map(|receipt| (&receipt.detector, &receipt.path))
        .collect::<BTreeSet<_>>();
    let excluded = config
        .discovery
        .excluded_paths
        .iter()
        .map(|receipt| ((&receipt.detector, &receipt.path), &receipt.evidence_digest))
        .collect::<BTreeMap<_, _>>();
    candidates
        .into_iter()
        .filter_map(|candidate| {
            if managed.contains(&(&candidate.detector, &candidate.path)) {
                return None;
            }
            if let Some(expected) = excluded.get(&(&candidate.detector, &candidate.path)) {
                let actual = candidate
                    .evidence
                    .iter()
                    .find(|item| item.path == candidate.path)
                    .map(|item| &item.digest);
                if actual == Some(*expected) {
                    return None;
                }
            }
            Some(candidate)
        })
        .collect()
}

fn apply_candidate_resolutions(
    root: &Path,
    mut config: Config,
    candidates: &[DiscoveryCandidate],
) -> Result<Config> {
    let validation_plan = InitPlan {
        schema: INIT_PLAN_SCHEMA.to_owned(),
        state: InitState::NeedsInput,
        source_kind: "workspace".to_owned(),
        source_fingerprint: format!("sha256:{}", "0".repeat(64)),
        inferred_config: config.clone(),
        discovery_candidates: candidates.to_vec(),
        diagnostics: Vec::new(),
        converted_intents: Vec::new(),
        parity: ParityResult {
            status: "equivalent".to_owned(),
            release_units: Vec::new(),
        },
        planned_operations: Vec::new(),
        post_commit_action: String::new(),
    };
    validation_plan.validate()?;

    for candidate in candidates {
        let CandidateResolution::Independent { release_unit } = candidate
            .resolution
            .as_ref()
            .expect("all candidate resolutions checked")
        else {
            continue;
        };
        let projection =
            candidate_projection(candidate, candidate.path.parent().unwrap_or(Path::new("")))?;
        let path = candidate
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let tag = candidate.tag.as_ref().ok_or_else(|| Error::Validation(format!(
            "discovery candidate {} cannot create an independent release unit without a tag suggestion",
            candidate.id
        )))?;
        config.release_units.insert(
            release_unit.clone(),
            ReleaseUnitConfig {
                path,
                disposition: ReleaseUnitDisposition::Managed,
                projections: candidate
                    .projection
                    .as_ref()
                    .map(|_| vec![projection])
                    .unwrap_or_default(),
                tags: BTreeMap::from([(
                    tag.id.clone(),
                    TagConfig {
                        role: tag.role,
                        template: tag.template.clone(),
                        require_phase: None,
                        tag_after: Vec::new(),
                    },
                )]),
                depends_on: Vec::new(),
            },
        );
    }
    for candidate in candidates {
        match candidate
            .resolution
            .as_ref()
            .expect("all candidate resolutions checked")
        {
            CandidateResolution::Independent { release_unit } => {
                config.discovery.managed_paths.push(ManagedPathReceipt {
                    detector: candidate.detector.clone(),
                    path: candidate.path.clone(),
                    release_unit: release_unit.clone(),
                });
            }
            CandidateResolution::Projection { release_unit, .. } => {
                let unit = config.release_units.get_mut(release_unit).ok_or_else(|| {
                    Error::Validation(format!(
                        "discovery candidate {} projects onto absent release unit {release_unit}",
                        candidate.id
                    ))
                })?;
                if candidate.projection.is_some() {
                    let projection = candidate_projection(candidate, &unit.path)?;
                    if !unit.projections.iter().any(|existing| {
                        existing.file == projection.file && existing.pointer == projection.pointer
                    }) {
                        unit.projections.push(projection);
                    }
                }
                config.discovery.managed_paths.push(ManagedPathReceipt {
                    detector: candidate.detector.clone(),
                    path: candidate.path.clone(),
                    release_unit: release_unit.clone(),
                });
            }
            CandidateResolution::Excluded => {
                let digest = candidate
                    .evidence
                    .iter()
                    .find(|item| item.path == candidate.path)
                    .expect("candidate path evidence validated")
                    .digest
                    .clone();
                config.discovery.excluded_paths.push(ExcludedPathReceipt {
                    detector: candidate.detector.clone(),
                    path: candidate.path.clone(),
                    evidence_digest: digest,
                });
            }
        }
    }
    config
        .discovery
        .managed_paths
        .sort_by(|a, b| (&a.detector, &a.path).cmp(&(&b.detector, &b.path)));
    config
        .discovery
        .excluded_paths
        .sort_by(|a, b| (&a.detector, &a.path).cmp(&(&b.detector, &b.path)));
    derive_npm_dependencies(root, &mut config)?;
    config.validate()?;
    Ok(config)
}

fn candidate_projection(candidate: &DiscoveryCandidate, unit_path: &Path) -> Result<Projection> {
    let suggestion = candidate.projection.as_ref().ok_or_else(|| {
        Error::Validation(format!(
            "discovery candidate {} has no projection suggestion",
            candidate.id
        ))
    })?;
    let file = suggestion
        .path
        .strip_prefix(unit_path)
        .map_err(|_| {
            Error::Validation(format!(
                "candidate projection {} is outside release-unit path {}",
                suggestion.path.display(),
                unit_path.display()
            ))
        })?
        .to_owned();
    Ok(Projection {
        adapter: suggestion.adapter,
        file,
        mode: suggestion.mode,
        pointer: suggestion.pointer.clone(),
    })
}

fn apply_changesets_candidate_receipts(
    config: &mut Config,
    candidates: &[DiscoveryCandidate],
) -> Result<()> {
    let mut removed_release_units = BTreeSet::new();
    for candidate in candidates {
        match &candidate.resolution {
            None => {}
            Some(CandidateResolution::Projection { release_unit, .. }) => {
                if !config.release_units.contains_key(release_unit) {
                    return Err(Error::Validation(format!(
                        "Changesets discovery candidate {} projects onto absent release unit {release_unit}",
                        candidate.id
                    )));
                }
                config.discovery.managed_paths.push(ManagedPathReceipt {
                    detector: candidate.detector.clone(),
                    path: candidate.path.clone(),
                    release_unit: release_unit.clone(),
                });
            }
            Some(CandidateResolution::Excluded) => {
                if let Some(native_identity) = &candidate.native_identity {
                    if let Some(unit) = config.release_units.get_mut(native_identity) {
                        unit.projections.retain(|projection| {
                            unit.path.join(&projection.file) != candidate.path
                        });
                        if unit.projections.is_empty() {
                            removed_release_units.insert(native_identity.clone());
                        }
                    }
                }
                let digest = candidate
                    .evidence
                    .iter()
                    .find(|item| item.path == candidate.path)
                    .expect("candidate path evidence validated")
                    .digest
                    .clone();
                config.discovery.excluded_paths.push(ExcludedPathReceipt {
                    detector: candidate.detector.clone(),
                    path: candidate.path.clone(),
                    evidence_digest: digest,
                });
            }
            Some(CandidateResolution::Independent { release_unit }) => {
                return Err(Error::Validation(format!(
                    "Changesets already establishes release unit {release_unit}; candidate {} must project onto it or be excluded",
                    candidate.id
                )));
            }
        }
    }
    for release_unit in &removed_release_units {
        config.release_units.remove(release_unit);
    }
    for unit in config.release_units.values_mut() {
        unit.depends_on
            .retain(|dependency| !removed_release_units.contains(dependency));
    }
    config
        .discovery
        .managed_paths
        .sort_by(|left, right| (&left.detector, &left.path).cmp(&(&right.detector, &right.path)));
    config
        .discovery
        .excluded_paths
        .sort_by(|left, right| (&left.detector, &left.path).cmp(&(&right.detector, &right.path)));
    Ok(())
}

fn changesets_plan(root: &Path, scan_all: bool, take_over: bool) -> Result<InitResult> {
    let previous = load_previous_plan(root)?;
    let changesets_config_path = root.join(CHANGESETS_CONFIG);
    let changesets_text = std::fs::read_to_string(&changesets_config_path)
        .map_err(|error| Error::io(&changesets_config_path, error))?;
    let changesets: JsonValue = serde_json::from_str(&changesets_text)
        .map_err(|error| Error::Validation(format!("invalid Changesets config: {error}")))?;
    let mut converted_intents = load_changesets_intents(root)?;
    let mut referenced_names = converted_intents
        .iter()
        .flat_map(|intent| intent.release_units.keys().cloned())
        .collect::<BTreeSet<_>>();
    for key in ["ignore", "fixed", "linked"] {
        collect_json_strings(&changesets[key], &mut referenced_names);
    }
    if let Some(profile) = load_release_profile(root)? {
        for package in profile["packages"].as_array().into_iter().flatten() {
            if let Some(name) = package["name"].as_str() {
                referenced_names.insert(name.to_owned());
            }
        }
    }
    let mut discovery = discover(root, scan_all, &referenced_names)?;
    reconcile_candidate_resolutions(&mut discovery.candidates, previous.as_ref());
    apply_changesets_candidate_receipts(&mut discovery.config, &discovery.candidates)?;
    discovery.config.settings.pre_1_0_bump_mapping = Pre1BumpMapping::Component;
    discovery.config.settings.internal_dependency_bump = changesets["updateInternalDependencies"]
        .as_str()
        .and_then(|value| value.parse().ok())
        .unwrap_or(Bump::Patch);
    discovery.config.fixed = parse_groups(&changesets["fixed"])?;
    discovery.config.linked = parse_groups(&changesets["linked"])?;
    let identity_map = merge_release_profile(root, &mut discovery)?;
    remap_converted_intents(&mut converted_intents, &identity_map, &discovery.config)?;

    let mut diagnostics = Vec::new();
    let config_evidence = evidence(root, Path::new(CHANGESETS_CONFIG), Vec::new())?;
    for ignored in changesets["ignore"].as_array().into_iter().flatten() {
        let Some(package) = ignored.as_str() else {
            continue;
        };
        diagnostics.push(InitDiagnostic {
            id: format!("ignored-release-unit-disposition:{package}"),
            code: "ignored-release-unit-disposition".to_owned(),
            message: format!(
                "Choose whether Changesets-ignored package {package} is suspended, excluded, or managed. Selecting managed requires removing it from Changesets ignore before takeover."
            ),
            evidence: vec![config_evidence.clone()],
            choices: vec!["suspended".to_owned(), "excluded".to_owned(), "managed".to_owned()],
            recommended: Some("suspended".to_owned()),
            resolution: None,
            verified: false,
            invalidated_resolution: false,
        });
    }
    if let Some((versions_private, tags_private)) =
        changesets_private_package_settings(&changesets["privatePackages"])
    {
        diagnostics.push(InitDiagnostic {
            id: "private-package-versioning".to_owned(),
            code: "private-package-versioning".to_owned(),
            message: if versions_private {
                "Changesets versions private packages; Intentional preserves that behavior because package privacy is independent from version management.".to_owned()
            } else {
                "Changesets suppresses private-package versions; Intentional manages versions independently from publication privacy, so accepting Intentional semantics is a deliberate contract change.".to_owned()
            },
            evidence: vec![config_evidence.clone()],
            choices: if versions_private {
                Vec::new()
            } else {
                vec!["intentional".to_owned()]
            },
            recommended: (!versions_private).then(|| "intentional".to_owned()),
            resolution: None,
            verified: versions_private,
            invalidated_resolution: false,
        });
        if !tags_private {
            diagnostics.push(InitDiagnostic {
                id: "private-package-tagging".to_owned(),
                code: "private-package-tagging".to_owned(),
                message: "Changesets suppresses private-package tags; Intentional creates annotated records for every managed release unit, independently from publication privacy.".to_owned(),
                evidence: vec![config_evidence.clone()],
                choices: vec!["intentional".to_owned()],
                recommended: Some("intentional".to_owned()),
                resolution: None,
                verified: false,
                invalidated_resolution: false,
            });
        }
    }
    if changesets["changelog"] != JsonValue::Bool(false) && !changesets["changelog"].is_null() {
        diagnostics.push(InitDiagnostic {
            id: "changesets-changelog".to_owned(),
            code: "changesets-changelog".to_owned(),
            message: "Intentional renders its contract-defined release-unit changelogs instead of invoking the configured Changesets changelog generator.".to_owned(),
            evidence: vec![config_evidence.clone()],
            choices: vec!["intentional".to_owned()],
            recommended: Some("intentional".to_owned()),
            resolution: None,
            verified: false,
            invalidated_resolution: false,
        });
    }
    if changesets["commit"] != JsonValue::Bool(false) && !changesets["commit"].is_null() {
        diagnostics.push(InitDiagnostic {
            id: "changesets-commit".to_owned(),
            code: "changesets-commit".to_owned(),
            message: "Intentional never creates commits; repository orchestration must retain the configured commit behavior externally.".to_owned(),
            evidence: vec![config_evidence.clone()],
            choices: vec!["external".to_owned()],
            recommended: Some("external".to_owned()),
            resolution: None,
            verified: false,
            invalidated_resolution: false,
        });
    }
    if changesets["___experimentalUnsafeOptions_WILL_CHANGE_IN_PATCH"]
        ["onlyUpdatePeerDependentsWhenOutOfRange"]
        .as_bool()
        == Some(true)
    {
        diagnostics.push(InitDiagnostic {
            id: "changesets-peer-dependent-policy".to_owned(),
            code: "changesets-peer-dependent-policy".to_owned(),
            message: "Changesets conditionally updates peer dependents from npm ranges; Intentional applies explicit depends-on edges uniformly. Accept Intentional's dependency contract, and resolve any current release divergence shown by parity before takeover.".to_owned(),
            evidence: vec![config_evidence.clone()],
            choices: vec!["intentional".to_owned()],
            recommended: Some("intentional".to_owned()),
            resolution: None,
            verified: false,
            invalidated_resolution: false,
        });
    }
    let dev_dependents = discovery
        .npm_dependencies
        .iter()
        .filter(|edge| edge.kind == NpmDependencyKind::Dev)
        .map(|edge| edge.dependent.clone())
        .collect::<BTreeSet<_>>();
    if !dev_dependents.is_empty() {
        let evidence = dev_dependents
            .iter()
            .filter_map(|id| {
                let release_unit = &discovery.config.release_units[id];
                release_unit
                    .projections
                    .iter()
                    .find(|projection| projection.adapter == Adapter::Npm)
                    .map(|projection| release_unit.path.join(&projection.file))
            })
            .map(|path| evidence(root, &path, Vec::new()))
            .collect::<Result<Vec<_>>>()?;
        diagnostics.push(InitDiagnostic {
            id: "changesets-dev-dependency-policy".to_owned(),
            code: "changesets-dev-dependency-policy".to_owned(),
            message: "Changesets propagates releases through internal devDependencies; Intentional reserves depends-on for durable release dependencies. Accept Intentional's dependency contract, and resolve any current release divergence shown by parity before takeover.".to_owned(),
            evidence,
            choices: vec!["intentional".to_owned()],
            recommended: Some("intentional".to_owned()),
            resolution: None,
            verified: false,
            invalidated_resolution: false,
        });
    }
    let mut unmapped_release_units = BTreeSet::new();
    for release_unit in discovery.config.release_units.keys() {
        if referenced_names.contains(release_unit)
            || discovery.workspace_packages.contains(release_unit)
        {
            continue;
        }
        unmapped_release_units.insert(release_unit.clone());
        let release_unit_config = &discovery.config.release_units[release_unit];
        let manifest = release_unit_config
            .projections
            .first()
            .map(|projection| release_unit_config.path.join(&projection.file))
            .ok_or_else(|| {
                Error::Validation(format!(
                    "discovered release unit {release_unit} has no manifest evidence"
                ))
            })?;
        diagnostics.push(InitDiagnostic {
            id: format!("unmapped-release-unit-disposition:{release_unit}"),
            code: "unmapped-release-unit-disposition".to_owned(),
            message: format!(
                "Choose whether workspace package {release_unit}, which is outside the Changesets release inventory, is excluded, suspended, or managed."
            ),
            evidence: vec![evidence(root, &manifest, Vec::new())?],
            choices: vec!["excluded".to_owned(), "suspended".to_owned(), "managed".to_owned()],
            recommended: Some("excluded".to_owned()),
            resolution: None,
            verified: false,
            invalidated_resolution: false,
        });
    }
    if let Some(profile_path) = existing_release_profile(root) {
        let profile_evidence = evidence(root, &profile_path, Vec::new())?;
        let text = std::fs::read_to_string(root.join(&profile_path))
            .map_err(|error| Error::io(root.join(&profile_path), error))?;
        if text.contains("publishAfter") || text.contains("publicationOrder") {
            diagnostics.push(InitDiagnostic {
                id: "repository-publication-sequencing".to_owned(),
                code: "repository-publication-sequencing".to_owned(),
                message: "Keep repository publication sequencing in the external release executor; Intentional imports only observable tag-after prerequisites.".to_owned(),
                evidence: vec![profile_evidence],
                choices: vec!["external".to_owned()],
                recommended: Some("external".to_owned()),
                resolution: None,
                verified: false,
                invalidated_resolution: false,
            });
        }
    }
    let current_integrations = scan_changesets_integrations(root)?;
    for item in &current_integrations {
        diagnostics.push(InitDiagnostic {
            id: format!("repository-integration:{}", item.path.display()),
            code: "repository-integration".to_owned(),
            message: format!(
                "Remove or replace executable Changesets references in {} and rerun init for verification.",
                item.path.display()
            ),
            evidence: vec![item.clone()],
            choices: vec!["removed".to_owned()],
            recommended: Some("removed".to_owned()),
            resolution: None,
            verified: false,
            invalidated_resolution: false,
        });
    }
    reconcile_diagnostics(root, &mut diagnostics, previous.as_ref())?;
    let mut source_config = discovery.config.clone();
    let ignored = changesets["ignore"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(JsonValue::as_str)
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let source_excluded = ignored.union(&unmapped_release_units).cloned().collect();
    exclude_release_units(&mut source_config, &source_excluded);
    apply_disposition_resolutions(&mut discovery.config, &diagnostics)?;
    discovery.config.validate()?;

    let declared: BTreeMap<String, Bump> =
        converted_intents
            .iter()
            .fold(BTreeMap::new(), |mut aggregate, intent| {
                for (id, bump) in &intent.release_units {
                    aggregate
                        .entry(id.clone())
                        .and_modify(|existing| *existing = (*existing).max(*bump))
                        .or_insert(*bump);
                }
                aggregate
            });
    let suppress_private_versions =
        changesets_private_package_settings(&changesets["privatePackages"])
            .is_some_and(|(versions_private, _)| !versions_private);
    let mut skipped_release_units = ignored
        .iter()
        .map(|id| identity_map.get(id).cloned().unwrap_or_else(|| id.clone()))
        .collect::<BTreeSet<_>>();
    if suppress_private_versions {
        skipped_release_units.extend(discovery.private_packages.iter().cloned());
    }
    skipped_release_units.extend(
        discovery
            .config
            .release_units
            .keys()
            .filter(|id| !discovery.versions.contains_key(*id))
            .cloned(),
    );
    let source_semantics = ChangesetsSourceSemantics {
        private_packages: &discovery.private_packages,
        suppress_private_versions,
        npm_dependencies: &discovery.npm_dependencies,
        internal_dependency_bump: discovery.config.settings.internal_dependency_bump,
        update_internal_dependents_always: changesets
            ["___experimentalUnsafeOptions_WILL_CHANGE_IN_PATCH"]["updateInternalDependents"]
            .as_str()
            == Some("always"),
        only_update_peer_dependents_when_out_of_range: changesets
            ["___experimentalUnsafeOptions_WILL_CHANGE_IN_PATCH"]
            ["onlyUpdatePeerDependentsWhenOutOfRange"]
            .as_bool()
            .unwrap_or(false),
        preflight_error: mixed_skipped_changeset(&converted_intents, &skipped_release_units),
    };
    let parity = parity_result(
        &source_config,
        &discovery.config,
        &discovery.versions,
        &declared,
        &source_semantics,
    )?;
    if let Some(message) = &parity.source_error {
        diagnostics.push(InitDiagnostic {
            id: "changesets-release-invalid".to_owned(),
            code: "changesets-release-invalid".to_owned(),
            message: format!("The source Changesets release is invalid: {message}"),
            evidence: vec![config_evidence.clone()],
            choices: Vec::new(),
            recommended: None,
            resolution: None,
            verified: false,
            invalidated_resolution: false,
        });
    }
    if let Some(message) = &parity.proposed_error {
        diagnostics.push(InitDiagnostic {
            id: "proposed-release-invalid".to_owned(),
            code: "proposed-release-invalid".to_owned(),
            message: format!("The proposed Intentional release is invalid: {message}"),
            evidence: vec![config_evidence.clone()],
            choices: Vec::new(),
            recommended: None,
            resolution: None,
            verified: false,
            invalidated_resolution: false,
        });
        diagnostics.sort_by(|left, right| left.id.cmp(&right.id));
    }
    let unresolved = discovery
        .candidates
        .iter()
        .any(|candidate| candidate.resolution.is_none())
        || diagnostics.iter().any(|diagnostic| {
            !diagnostic.choices.is_empty()
                && (diagnostic.resolution.is_none() || !diagnostic.verified)
        });
    let state = if unresolved || parity.result.status != "equivalent" {
        InitState::NeedsInput
    } else {
        InitState::Ready
    };

    let mut evidence_paths = discovery.evidence;
    evidence_paths.insert(PathBuf::from(CHANGESETS_CONFIG));
    for intent in &converted_intents {
        evidence_paths.insert(intent.source.clone());
    }
    for diagnostic in &diagnostics {
        for item in &diagnostic.evidence {
            evidence_paths.insert(item.path.clone());
        }
    }
    let source_fingerprint = fingerprint(root, &evidence_paths)?;
    let planned_operations = takeover_operations(root, &converted_intents);
    let mut plan = InitPlan {
        schema: INIT_PLAN_SCHEMA.to_owned(),
        state,
        source_kind: "changesets".to_owned(),
        source_fingerprint,
        inferred_config: discovery.config,
        discovery_candidates: discovery.candidates,
        diagnostics,
        converted_intents,
        parity: parity.result,
        planned_operations: planned_operations.clone(),
        post_commit_action: "intentional tag --baseline".to_owned(),
    };
    // The state is always recomputed after reading editable resolutions.
    plan.state = state;
    let contents = plan.to_yaml()?;

    if take_over {
        if state != InitState::Ready {
            return Err(Error::Validation(
                "takeover requires a ready initialization plan with verified resolutions and parity"
                    .to_owned(),
            ));
        }
        let mut writes = vec![(PathBuf::from(CONFIG_PATH), plan.inferred_config.to_yaml()?)];
        for intent in &plan.converted_intents {
            writes.push((intent.target.clone(), intent.contents()?));
        }
        let deletes = takeover_deletes(root);
        verify_takeover_preconditions(root, &writes, &plan.source_fingerprint, &evidence_paths)?;
        let takeover_evidence = Some((plan.source_fingerprint.clone(), evidence_paths));
        return Ok(InitResult {
            state: InitState::Success,
            path: PathBuf::from(CONFIG_PATH),
            contents: plan.inferred_config.to_yaml()?,
            operations: planned_operations,
            plan: Some(plan),
            writes,
            deletes,
            takeover: true,
            takeover_evidence,
        });
    }

    Ok(InitResult {
        state,
        path: PathBuf::from(INIT_PLAN_PATH),
        operations: vec![format!("write {INIT_PLAN_PATH}")],
        contents: contents.clone(),
        plan: Some(plan),
        writes: vec![(PathBuf::from(INIT_PLAN_PATH), contents)],
        deletes: Vec::new(),
        takeover: false,
        takeover_evidence: None,
    })
}

fn discover(root: &Path, scan_all: bool, referenced_names: &BTreeSet<String>) -> Result<Discovery> {
    let workspace_paths = workspace_manifest_paths(root)?;
    let mut manifest_paths = if scan_all {
        all_manifest_paths(root)?
    } else {
        workspace_paths.clone()
    };
    if !referenced_names.is_empty() {
        for path in all_manifest_paths(root)? {
            let Some(adapter) = adapter_for(&path) else {
                continue;
            };
            if let Ok((name, _)) = manifest_identity(&path, adapter) {
                if referenced_names.contains(&name) {
                    manifest_paths.insert(path);
                }
            }
        }
    }
    let mut discovery = Discovery {
        config: Config::default(),
        ..Discovery::default()
    };
    for path in manifest_paths {
        let Some(adapter) = adapter_for(&path) else {
            continue;
        };
        if !is_project_manifest(&path, adapter)? {
            continue;
        }
        let (id, version) = manifest_identity(&path, adapter)?;
        let relative_manifest = path
            .strip_prefix(root)
            .map_err(|error| Error::Validation(format!("manifest is outside workspace: {error}")))?
            .to_owned();
        let manifest_evidence = evidence(root, &relative_manifest, Vec::new())?;
        let detector = detector_for(adapter).to_owned();
        discovery.candidates.push(DiscoveryCandidate {
            id: DiscoveryCandidate::stable_id(&detector, &relative_manifest)?,
            detector,
            path: relative_manifest.clone(),
            evidence: vec![manifest_evidence.clone()],
            native_identity: Some(id.clone()),
            raw_version: version.as_ref().map(|value| RawVersionEvidence {
                value: value.clone(),
                evidence: vec![manifest_evidence],
            }),
            projection: Some(CandidateProjectionSuggestion {
                adapter,
                path: relative_manifest.clone(),
                mode: if adapter == Adapter::Go {
                    ProjectionMode::None
                } else {
                    ProjectionMode::Committed
                },
                pointer: None,
            }),
            tag: Some(CandidateTagSuggestion {
                id: "primary".to_owned(),
                role: TagRole::Primary,
                template: "{id}@{version}".to_owned(),
            }),
            diagnostics: Vec::new(),
            resolution: None,
        });
        let workspace_package = workspace_paths.contains(&path);
        let private_package = adapter == Adapter::Npm && npm_manifest_is_private(&path)?;
        let directory = path.parent().expect("manifest has parent");
        let relative_directory = directory.strip_prefix(root).map_err(|error| {
            Error::Validation(format!("manifest is outside workspace: {error}"))
        })?;
        let package_path = if relative_directory.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            relative_directory.to_owned()
        };
        let projection = Projection {
            adapter,
            file: path
                .file_name()
                .map(PathBuf::from)
                .expect("manifest has filename"),
            mode: if adapter == Adapter::Go {
                ProjectionMode::None
            } else {
                ProjectionMode::Committed
            },
            pointer: None,
        };
        if let Some(existing) = discovery.config.release_units.get_mut(&id) {
            if existing.path != package_path {
                return Err(Error::Validation(format!(
                    "manifest-native package name {id} is declared in both {} and {}",
                    existing.path.display(),
                    package_path.display()
                )));
            }
            existing.projections.push(projection);
        } else {
            discovery.config.release_units.insert(
                id.clone(),
                ReleaseUnitConfig {
                    path: package_path,
                    disposition: ReleaseUnitDisposition::Managed,
                    projections: vec![projection],
                    tags: BTreeMap::from([(
                        "primary".to_owned(),
                        TagConfig {
                            role: TagRole::Primary,
                            template: "{id}@{version}".to_owned(),
                            require_phase: None,
                            tag_after: Vec::new(),
                        },
                    )]),
                    depends_on: Vec::new(),
                },
            );
        }
        if let Some(version) = version {
            if let Ok(version) = Version::parse(&version) {
                discovery.versions.insert(id.clone(), version);
            }
        }
        if workspace_package {
            discovery.workspace_packages.insert(id.clone());
        }
        if private_package {
            discovery.private_packages.insert(id);
        }
        discovery
            .evidence
            .insert(path.strip_prefix(root).unwrap_or(&path).to_owned());
    }
    discovery.npm_dependencies = derive_npm_dependencies(root, &mut discovery.config)?;
    discovery
        .candidates
        .sort_by(|left, right| left.path.cmp(&right.path));
    Ok(discovery)
}

fn workspace_manifest_paths(root: &Path) -> Result<BTreeSet<PathBuf>> {
    let mut directories = BTreeSet::new();
    let mut found_workspace = false;
    let pnpm = root.join("pnpm-workspace.yaml");
    if pnpm.exists() {
        found_workspace = true;
        let text = std::fs::read_to_string(&pnpm).map_err(|error| Error::io(&pnpm, error))?;
        let value: serde_yaml::Value = serde_yaml::from_str(&text)?;
        expand_workspace_patterns(
            root,
            value["packages"]
                .as_sequence()
                .into_iter()
                .flatten()
                .filter_map(serde_yaml::Value::as_str),
            &mut directories,
        )?;
    }
    let package_json = root.join("package.json");
    if package_json.exists() {
        let text = std::fs::read_to_string(&package_json)
            .map_err(|error| Error::io(&package_json, error))?;
        let value: JsonValue = serde_json::from_str(&text)
            .map_err(|error| Error::Validation(format!("invalid package.json: {error}")))?;
        let workspaces = value["workspaces"]
            .as_array()
            .or_else(|| value["workspaces"]["packages"].as_array());
        if let Some(workspaces) = workspaces {
            found_workspace = true;
            expand_workspace_patterns(
                root,
                workspaces.iter().filter_map(JsonValue::as_str),
                &mut directories,
            )?;
        }
    }
    let cargo = root.join("Cargo.toml");
    if cargo.exists() {
        let text = std::fs::read_to_string(&cargo).map_err(|error| Error::io(&cargo, error))?;
        let document = text
            .parse::<toml_edit::DocumentMut>()
            .map_err(|error| Error::Validation(format!("invalid Cargo.toml: {error}")))?;
        if let Some(members) = document
            .get("workspace")
            .and_then(|workspace| workspace.get("members"))
            .and_then(toml_edit::Item::as_array)
        {
            found_workspace = true;
            expand_workspace_patterns(
                root,
                members.iter().filter_map(toml_edit::Value::as_str),
                &mut directories,
            )?;
        }
    }
    if !found_workspace {
        directories.insert(root.to_owned());
    }
    let mut paths = BTreeSet::new();
    for directory in directories {
        add_manifests_in_directory(&directory, &mut paths)?;
    }
    paths.retain(|path| !git_ignored(root, path));
    Ok(paths)
}

fn expand_workspace_pattern(
    root: &Path,
    pattern: &str,
    directories: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let (excluded, pattern) = pattern
        .strip_prefix('!')
        .map_or((false, pattern), |pattern| (true, pattern));
    let absolute = root.join(pattern).to_string_lossy().to_string();
    for entry in glob(&absolute).map_err(|error| {
        Error::Validation(format!("invalid workspace pattern {pattern}: {error}"))
    })? {
        let path = entry
            .map_err(|error| Error::Validation(format!("workspace pattern failed: {error}")))?;
        if path.is_dir() {
            if excluded {
                directories.remove(&path);
            } else {
                directories.insert(path);
            }
        }
    }
    Ok(())
}

fn expand_workspace_patterns<'a>(
    root: &Path,
    patterns: impl IntoIterator<Item = &'a str>,
    directories: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let patterns = patterns.into_iter().collect::<Vec<_>>();
    for excluded in [false, true] {
        for pattern in &patterns {
            if pattern.starts_with('!') == excluded {
                expand_workspace_pattern(root, pattern, directories)?;
            }
        }
    }
    Ok(())
}

fn add_manifests_in_directory(directory: &Path, paths: &mut BTreeSet<PathBuf>) -> Result<()> {
    for name in [
        "package.json",
        "Cargo.toml",
        "pubspec.yaml",
        "pyproject.toml",
        "go.mod",
    ] {
        let path = directory.join(name);
        if path.is_file() {
            paths.insert(path);
        }
    }
    for entry in std::fs::read_dir(directory).map_err(|error| Error::io(directory, error))? {
        let path = entry.map_err(|error| Error::io(directory, error))?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "csproj")
        {
            paths.insert(path);
        }
    }
    Ok(())
}

fn all_manifest_paths(root: &Path) -> Result<BTreeSet<PathBuf>> {
    Ok(WalkDir::new(root)
        .into_iter()
        .filter_entry(should_visit)
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file() && adapter_for(entry.path()).is_some())
        .filter(|entry| !git_ignored(root, entry.path()))
        .map(|entry| entry.into_path())
        .collect())
}

fn should_visit(entry: &DirEntry) -> bool {
    !entry.file_type().is_dir()
        || !matches!(
            entry.file_name().to_str(),
            Some(
                ".git"
                    | ".intentional"
                    | "node_modules"
                    | "target"
                    | ".pnpm-store"
                    | ".yarn"
                    | ".npm"
                    | ".cache"
                    | ".venv"
                    | "venv"
                    | "__pycache__"
                    | ".dart_tool"
                    | ".pub-cache"
            )
        )
}

fn git_ignored(root: &Path, path: &Path) -> bool {
    Command::new("git")
        .args(["check-ignore", "-q", "--"])
        .arg(path)
        .current_dir(root)
        .status()
        .is_ok_and(|status| status.success())
}

fn detector_for(adapter: Adapter) -> &'static str {
    match adapter {
        Adapter::Npm => "npm-package",
        Adapter::Cargo => "cargo-package",
        Adapter::Go => "go-module",
        Adapter::Python => "python-project",
        Adapter::Msbuild => "msbuild-project",
        Adapter::Pub => "dart-package",
        Adapter::Json | Adapter::Toml | Adapter::Yaml => {
            unreachable!("generic adapters are not discovered")
        }
    }
}

fn adapter_for(path: &Path) -> Option<Adapter> {
    let name = path.file_name()?.to_str()?;
    match name {
        "package.json" => Some(Adapter::Npm),
        "Cargo.toml" => Some(Adapter::Cargo),
        "pubspec.yaml" => Some(Adapter::Pub),
        "pyproject.toml" => Some(Adapter::Python),
        "go.mod" => Some(Adapter::Go),
        name if name.ends_with(".csproj") => Some(Adapter::Msbuild),
        _ => None,
    }
}

fn is_project_manifest(path: &Path, adapter: Adapter) -> Result<bool> {
    if adapter != Adapter::Cargo {
        return Ok(true);
    }
    let text = std::fs::read_to_string(path).map_err(|error| Error::io(path, error))?;
    let document = text
        .parse::<toml_edit::DocumentMut>()
        .map_err(|error| Error::Validation(format!("invalid Cargo.toml: {error}")))?;
    Ok(document.get("package").is_some())
}

fn manifest_identity(path: &Path, adapter: Adapter) -> Result<(String, Option<String>)> {
    let text = std::fs::read_to_string(path).map_err(|error| Error::io(path, error))?;
    match adapter {
        Adapter::Npm => {
            let value: JsonValue = serde_json::from_str(&text).map_err(|error| {
                Error::Validation(format!("invalid {}: {error}", path.display()))
            })?;
            Ok((
                required_string(&value["name"], path, "name")?,
                value["version"].as_str().map(str::to_owned),
            ))
        }
        Adapter::Cargo => {
            let document = text.parse::<toml_edit::DocumentMut>().map_err(|error| {
                Error::Validation(format!("invalid {}: {error}", path.display()))
            })?;
            let package = document.get("package").ok_or_else(|| {
                Error::Validation(format!("{} has no package table", path.display()))
            })?;
            let name = package
                .get("name")
                .and_then(toml_edit::Item::as_str)
                .ok_or_else(|| {
                    Error::Validation(format!("{} has no package.name", path.display()))
                })?;
            let version = package
                .get("version")
                .and_then(toml_edit::Item::as_str)
                .map(str::to_owned);
            Ok((name.to_owned(), version))
        }
        Adapter::Pub => {
            let value: serde_yaml::Value = serde_yaml::from_str(&text)?;
            let name = value["name"]
                .as_str()
                .ok_or_else(|| Error::Validation(format!("{} has no name", path.display())))?;
            Ok((
                name.to_owned(),
                value["version"].as_str().map(str::to_owned),
            ))
        }
        Adapter::Python => {
            let document = text.parse::<toml_edit::DocumentMut>().map_err(|error| {
                Error::Validation(format!("invalid {}: {error}", path.display()))
            })?;
            let project = document.get("project").ok_or_else(|| {
                Error::Validation(format!("{} has no project table", path.display()))
            })?;
            let name = project
                .get("name")
                .and_then(toml_edit::Item::as_str)
                .ok_or_else(|| {
                    Error::Validation(format!("{} has no project.name", path.display()))
                })?;
            let version = project
                .get("version")
                .and_then(toml_edit::Item::as_str)
                .map(str::to_owned);
            Ok((name.to_owned(), version))
        }
        Adapter::Go => {
            let name = text
                .lines()
                .find_map(|line| line.trim().strip_prefix("module "))
                .ok_or_else(|| {
                    Error::Validation(format!("{} has no module directive", path.display()))
                })?;
            Ok((name.to_owned(), None))
        }
        Adapter::Msbuild => {
            let name = xml_element(&text, "PackageId")
                .or_else(|| xml_element(&text, "AssemblyName"))
                .or_else(|| {
                    path.file_stem()
                        .and_then(|value| value.to_str())
                        .map(str::to_owned)
                })
                .ok_or_else(|| {
                    Error::Validation(format!("{} has no package identity", path.display()))
                })?;
            Ok((name, xml_element(&text, "Version")))
        }
        Adapter::Json | Adapter::Toml | Adapter::Yaml => {
            unreachable!("generic adapters are not discovered")
        }
    }
}

fn npm_manifest_is_private(path: &Path) -> Result<bool> {
    let text = std::fs::read_to_string(path).map_err(|error| Error::io(path, error))?;
    let value: JsonValue = serde_json::from_str(&text)
        .map_err(|error| Error::Validation(format!("invalid {}: {error}", path.display())))?;
    Ok(value["private"].as_bool() == Some(true))
}

fn required_string(value: &JsonValue, path: &Path, field: &str) -> Result<String> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Error::Validation(format!("{} has no string {field}", path.display())))
}

fn xml_element(text: &str, name: &str) -> Option<String> {
    let start = format!("<{name}>");
    let end = format!("</{name}>");
    text.split_once(&start)?
        .1
        .split_once(&end)
        .map(|(value, _)| value.trim().to_owned())
}

fn derive_npm_dependencies(root: &Path, config: &mut Config) -> Result<Vec<NpmDependencyEdge>> {
    let ids = config
        .release_units
        .keys()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut edges = Vec::new();
    for (id, release_unit) in &mut config.release_units {
        let npm = release_unit
            .projections
            .iter()
            .find(|projection| projection.adapter == Adapter::Npm);
        let Some(npm) = npm else { continue };
        let path = root.join(&release_unit.path).join(&npm.file);
        let text = std::fs::read_to_string(&path).map_err(|error| Error::io(&path, error))?;
        let value: JsonValue = serde_json::from_str(&text)
            .map_err(|error| Error::Validation(format!("invalid {}: {error}", path.display())))?;
        let mut dependencies = BTreeSet::new();
        for (group, kind) in [
            ("dependencies", NpmDependencyKind::Dependency),
            ("optionalDependencies", NpmDependencyKind::Optional),
            ("peerDependencies", NpmDependencyKind::Peer),
            ("devDependencies", NpmDependencyKind::Dev),
        ] {
            if let Some(entries) = value[group].as_object() {
                for (dependency, range) in entries {
                    if !ids.contains(dependency) {
                        continue;
                    }
                    if kind != NpmDependencyKind::Dev {
                        dependencies.insert(dependency.clone());
                    }
                    edges.push(NpmDependencyEdge {
                        dependent: id.clone(),
                        dependency: dependency.clone(),
                        kind,
                        range: range.as_str().unwrap_or_default().to_owned(),
                    });
                }
            }
        }
        release_unit.depends_on = dependencies.into_iter().collect();
    }
    edges.sort_by(|left, right| {
        (
            &left.dependent,
            &left.dependency,
            left.kind as u8,
            &left.range,
        )
            .cmp(&(
                &right.dependent,
                &right.dependency,
                right.kind as u8,
                &right.range,
            ))
    });
    Ok(edges)
}

fn load_changesets_intents(root: &Path) -> Result<Vec<ConvertedIntent>> {
    let directory = root.join(".changeset");
    let mut paths = std::fs::read_dir(&directory)
        .map_err(|error| Error::io(&directory, error))?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
        .filter(|path| path.file_name().is_none_or(|name| name != "README.md"))
        .collect::<Vec<_>>();
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).map_err(|error| Error::io(&path, error))?;
            let rest = text.strip_prefix("---\n").ok_or_else(|| {
                Error::Validation(format!("{} has no Changesets frontmatter", path.display()))
            })?;
            let (frontmatter, message) = rest.split_once("\n---\n").ok_or_else(|| {
                Error::Validation(format!(
                    "{} has unterminated Changesets frontmatter",
                    path.display()
                ))
            })?;
            let packages = serde_yaml::from_str(frontmatter)?;
            let id = path
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or_else(|| {
                    Error::Validation(format!("invalid Changesets filename {}", path.display()))
                })?
                .to_owned();
            Ok(ConvertedIntent {
                target: PathBuf::from(crate::intent::INTENTS_PATH).join(format!("{id}.md")),
                source: path.strip_prefix(root).unwrap_or(&path).to_owned(),
                id,
                release_units: packages,
                message: message.trim().to_owned(),
            })
        })
        .collect()
}

fn parse_groups(value: &JsonValue) -> Result<Vec<Vec<String>>> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .map(|group| {
            group
                .as_array()
                .ok_or_else(|| {
                    Error::Validation("Changesets release group must be an array".to_owned())
                })?
                .iter()
                .map(|member| {
                    member.as_str().map(str::to_owned).ok_or_else(|| {
                        Error::Validation("Changesets group member must be a string".to_owned())
                    })
                })
                .collect()
        })
        .collect()
}

fn collect_json_strings(value: &JsonValue, output: &mut BTreeSet<String>) {
    match value {
        JsonValue::String(value) => {
            output.insert(value.clone());
        }
        JsonValue::Array(values) => {
            for value in values {
                collect_json_strings(value, output);
            }
        }
        _ => {}
    }
}

fn changesets_private_package_settings(value: &JsonValue) -> Option<(bool, bool)> {
    match value {
        JsonValue::Bool(enabled) => Some((*enabled, *enabled)),
        JsonValue::Object(settings) => Some((
            settings
                .get("version")
                .and_then(JsonValue::as_bool)
                .unwrap_or(true),
            settings
                .get("tag")
                .and_then(JsonValue::as_bool)
                .unwrap_or(true),
        )),
        _ => None,
    }
}

fn existing_release_profile(root: &Path) -> Option<PathBuf> {
    let path = PathBuf::from("scripts/release-contract-profile.json");
    root.join(&path).is_file().then_some(path)
}

fn load_release_profile(root: &Path) -> Result<Option<JsonValue>> {
    let Some(path) = existing_release_profile(root) else {
        return Ok(None);
    };
    let text = std::fs::read_to_string(root.join(&path))
        .map_err(|error| Error::io(root.join(&path), error))?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|error| Error::Validation(format!("invalid {}: {error}", path.display())))
}

fn merge_release_profile(
    root: &Path,
    discovery: &mut Discovery,
) -> Result<BTreeMap<String, String>> {
    let Some(profile) = load_release_profile(root)? else {
        return Ok(BTreeMap::new());
    };
    let mut identity_map = BTreeMap::new();
    let profile_path = existing_release_profile(root).expect("loaded profile path");
    discovery.evidence.insert(profile_path);
    let entries = profile["packages"].as_array().cloned().unwrap_or_default();
    for entry in entries {
        let Some(name) = entry["name"].as_str() else {
            continue;
        };
        let source = entry["versionSource"].as_str().unwrap_or(name);
        if source == name {
            continue;
        }
        identity_map.insert(name.to_owned(), source.to_owned());
        let Some(projected) = discovery.config.release_units.remove(name) else {
            continue;
        };
        if discovery.workspace_packages.remove(name) {
            discovery.workspace_packages.insert(source.to_owned());
        }
        if discovery.private_packages.remove(name) {
            discovery.private_packages.insert(source.to_owned());
        }
        let source_release_unit =
            discovery
                .config
                .release_units
                .get_mut(source)
                .ok_or_else(|| {
                    Error::Validation(format!(
                        "release profile versionSource {source} for {name} was not discovered"
                    ))
                })?;
        for mut projection in projected.projections {
            let absolute = root.join(&projected.path).join(&projection.file);
            projection.file = absolute
                .strip_prefix(root.join(&source_release_unit.path))
                .map_err(|_| {
                    Error::Validation(format!(
                        "projection {} is outside release unit {source}",
                        absolute.display()
                    ))
                })?
                .to_owned();
            source_release_unit.projections.push(projection);
        }
        source_release_unit.tags.insert(
            name.to_owned(),
            TagConfig {
                role: TagRole::Projection,
                template: format!("{name}@{{version}}"),
                require_phase: entry["tagBeforePublish"]
                    .as_bool()
                    .and_then(|before| before.then_some(TagPhase::BeforePublication)),
                tag_after: Vec::new(),
            },
        );
        discovery.versions.remove(name);
    }
    for release_unit in discovery.config.release_units.values_mut() {
        for dependency in &mut release_unit.depends_on {
            if let Some(source) = identity_map.get(dependency) {
                *dependency = source.clone();
            }
        }
        release_unit.depends_on.sort();
        release_unit.depends_on.dedup();
    }
    for edge in &mut discovery.npm_dependencies {
        if let Some(source) = identity_map.get(&edge.dependent) {
            edge.dependent = source.clone();
        }
        if let Some(source) = identity_map.get(&edge.dependency) {
            edge.dependency = source.clone();
        }
    }
    discovery.npm_dependencies.retain(|edge| {
        edge.dependent != edge.dependency
            && discovery.config.release_units.contains_key(&edge.dependent)
            && discovery
                .config
                .release_units
                .contains_key(&edge.dependency)
    });
    discovery.config.workspace_tags.insert(
        "release".to_owned(),
        WorkspaceTagConfig {
            template: "{version}".to_owned(),
            require_phase: None,
            tag_after: Vec::new(),
        },
    );
    Ok(identity_map)
}

fn remap_converted_intents(
    intents: &mut [ConvertedIntent],
    identity_map: &BTreeMap<String, String>,
    config: &Config,
) -> Result<()> {
    for intent in intents {
        let mut release_units = BTreeMap::<String, Bump>::new();
        for (id, bump) in std::mem::take(&mut intent.release_units) {
            let logical_id = identity_map.get(&id).cloned().unwrap_or(id);
            if !config.release_units.contains_key(&logical_id) {
                return Err(Error::Validation(format!(
                    "Changesets intent {} references release unit {logical_id}, which has no Intentional release-unit identity",
                    intent.id
                )));
            }
            release_units
                .entry(logical_id)
                .and_modify(|existing| *existing = (*existing).max(bump))
                .or_insert(bump);
        }
        intent.release_units = release_units;
    }
    Ok(())
}

fn scan_changesets_integrations(root: &Path) -> Result<Vec<SourceEvidence>> {
    let mut findings = Vec::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(should_visit)
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry.path();
        let relative = path.strip_prefix(root).unwrap_or(path);
        if relative.starts_with(".changeset") || relative.starts_with(".intentional") {
            continue;
        }
        let relevant = relative == Path::new("package.json")
            || relative == Path::new("pnpm-lock.yaml")
            || relative.starts_with(".github")
            || relative.starts_with("scripts")
            || relative
                .components()
                .any(|component| component.as_os_str().to_string_lossy().contains("test"));
        if !relevant {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let lines = text
            .lines()
            .enumerate()
            .filter(|(_, line)| line.to_ascii_lowercase().contains("changeset"))
            .map(|(index, _)| index + 1)
            .collect::<Vec<_>>();
        if !lines.is_empty() {
            findings.push(evidence(root, relative, lines)?);
        }
    }
    findings.sort();
    Ok(findings)
}

fn reconcile_diagnostics(
    root: &Path,
    current: &mut Vec<InitDiagnostic>,
    previous: Option<&InitPlan>,
) -> Result<()> {
    let Some(previous) = previous else {
        return Ok(());
    };
    let previous_by_id = previous
        .diagnostics
        .iter()
        .map(|diagnostic| (diagnostic.id.as_str(), diagnostic))
        .collect::<BTreeMap<_, _>>();
    for diagnostic in current.iter_mut() {
        let Some(old) = previous_by_id.get(diagnostic.id.as_str()) else {
            continue;
        };
        if old.evidence == diagnostic.evidence {
            diagnostic.resolution = old.resolution.clone();
            diagnostic.verified =
                diagnostic.code != "repository-integration" && diagnostic.resolution.is_some();
        } else if old.resolution.is_some() {
            diagnostic.invalidated_resolution = true;
        }
    }
    let current_ids = current
        .iter()
        .map(|diagnostic| diagnostic.id.clone())
        .collect::<BTreeSet<_>>();
    for old in &previous.diagnostics {
        if old.code != "repository-integration"
            || current_ids.contains(&old.id)
            || old.resolution.as_deref() != Some("removed")
        {
            continue;
        }
        let Some(path) = old.evidence.first().map(|evidence| evidence.path.clone()) else {
            continue;
        };
        let full = root.join(&path);
        if !full.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&full).map_err(|error| Error::io(&full, error))?;
        if text.to_ascii_lowercase().contains("changeset") {
            continue;
        }
        current.push(InitDiagnostic {
            id: old.id.clone(),
            code: old.code.clone(),
            message: old.message.clone(),
            evidence: vec![evidence(root, &path, Vec::new())?],
            choices: old.choices.clone(),
            recommended: old.recommended.clone(),
            resolution: old.resolution.clone(),
            verified: true,
            invalidated_resolution: false,
        });
    }
    current.sort_by(|left, right| left.id.cmp(&right.id));
    Ok(())
}

fn apply_disposition_resolutions(
    config: &mut Config,
    diagnostics: &[InitDiagnostic],
) -> Result<()> {
    let mut excluded = BTreeSet::new();
    for diagnostic in diagnostics.iter().filter(|diagnostic| {
        diagnostic.code == "ignored-release-unit-disposition"
            || diagnostic.code == "unmapped-release-unit-disposition"
    }) {
        let release_unit = diagnostic
            .id
            .split_once(':')
            .map(|(_, release_unit)| release_unit)
            .expect("diagnostic id prefix");
        match diagnostic.resolution.as_deref() {
            Some("suspended") => {
                if let Some(config) = config.release_units.get_mut(release_unit) {
                    config.disposition = ReleaseUnitDisposition::Suspended;
                }
            }
            Some("excluded") => {
                config.release_units.remove(release_unit);
                excluded.insert(release_unit.to_owned());
            }
            Some("managed") | None => {}
            Some(value) => {
                return Err(Error::Validation(format!(
                    "invalid resolution {value} for {}",
                    diagnostic.id
                )))
            }
        }
    }
    exclude_release_units(config, &excluded);
    Ok(())
}

fn exclude_release_units(config: &mut Config, excluded: &BTreeSet<String>) {
    for id in excluded {
        config.release_units.remove(id);
    }
    if excluded.is_empty() {
        return;
    }
    for release_unit in config.release_units.values_mut() {
        release_unit.depends_on.retain(|id| !excluded.contains(id));
    }
    for groups in [&mut config.fixed, &mut config.linked] {
        for group in groups.iter_mut() {
            group.retain(|id| !excluded.contains(id));
        }
        groups.retain(|group| group.len() >= 2);
    }
}

fn parity_result(
    source_config: &Config,
    proposed_config: &Config,
    current: &BTreeMap<String, Version>,
    declared: &BTreeMap<String, Bump>,
    source_semantics: &ChangesetsSourceSemantics<'_>,
) -> Result<ParityComputation> {
    let mut source_config = source_config.clone();
    let mut proposed_config = proposed_config.clone();
    let effective = effective_bumps(&proposed_config, declared);
    let non_releasing_without_versions = proposed_config
        .release_units
        .keys()
        .filter(|id| !current.contains_key(*id) && effective[*id] == Bump::None)
        .cloned()
        .collect::<BTreeSet<_>>();
    exclude_release_units(&mut source_config, &non_releasing_without_versions);
    exclude_release_units(&mut proposed_config, &non_releasing_without_versions);
    let excluded_pending = declared
        .keys()
        .filter(|id| !proposed_config.release_units.contains_key(*id))
        .cloned()
        .collect::<Vec<_>>();
    let proposed_preflight_error = (!excluded_pending.is_empty()).then(|| {
        format!(
            "converted intents reference release units excluded from the proposed inventory: {}",
            excluded_pending.join(", ")
        )
    });
    if source_semantics.preflight_error.is_some() || proposed_preflight_error.is_some() {
        return Ok(ParityComputation {
            result: ParityResult {
                status: "blocked".to_owned(),
                release_units: Vec::new(),
            },
            source_error: source_semantics.preflight_error.clone(),
            proposed_error: proposed_preflight_error,
        });
    }
    let missing = source_config
        .release_units
        .keys()
        .chain(proposed_config.release_units.keys())
        .filter(|id| !current.contains_key(*id))
        .cloned()
        .collect::<BTreeSet<_>>();
    if !missing.is_empty() {
        return Ok(ParityComputation {
            result: ParityResult {
                status: "blocked".to_owned(),
                release_units: Vec::new(),
            },
            source_error: None,
            proposed_error: Some(format!(
                "missing current versions for release units: {}",
                missing.into_iter().collect::<Vec<_>>().join(", ")
            )),
        });
    }
    let (source, source_error) =
        match resolve_changesets_source(&source_config, declared, current, source_semantics) {
            Ok(resolved) => (resolved, None),
            Err(Error::Validation(message)) => (BTreeMap::new(), Some(message)),
            Err(error) => return Err(error),
        };
    let (proposed, proposed_error) = match resolve_versions(&proposed_config, declared, current) {
        Ok(resolved) => (resolved, None),
        Err(Error::Validation(message)) => (BTreeMap::new(), Some(message)),
        Err(error) => return Err(error),
    };
    let release_ids = source
        .iter()
        .chain(&proposed)
        .filter(|(_, versions)| versions.bump != Bump::None)
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    let release_units = release_ids
        .into_iter()
        .map(|release_unit| ParityReleaseUnit {
            current_version: current[&release_unit].to_string(),
            source: parity_release(source.get(&release_unit)),
            proposed: parity_release(proposed.get(&release_unit)),
            release_unit,
        })
        .collect::<Vec<_>>();
    let equivalent = source_error.is_none()
        && proposed_error.is_none()
        && release_units
            .iter()
            .all(|release_unit| release_unit.source == release_unit.proposed);
    Ok(ParityComputation {
        result: ParityResult {
            status: if equivalent { "equivalent" } else { "blocked" }.to_owned(),
            release_units,
        },
        source_error,
        proposed_error,
    })
}

fn mixed_skipped_changeset(
    intents: &[ConvertedIntent],
    skipped_release_units: &BTreeSet<String>,
) -> Option<String> {
    intents.iter().find_map(|intent| {
        let has_skipped = intent
            .release_units
            .keys()
            .any(|id| skipped_release_units.contains(id));
        let has_managed = intent
            .release_units
            .keys()
            .any(|id| !skipped_release_units.contains(id));
        (has_skipped && has_managed).then(|| {
            format!(
                "Changesets intent {} mixes skipped and managed release units; split or revise the source changeset before takeover",
                intent.id
            )
        })
    })
}

struct ParityComputation {
    result: ParityResult,
    source_error: Option<String>,
    proposed_error: Option<String>,
}

fn parity_release(version: Option<&ReleaseUnitVersion>) -> Option<ParityRelease> {
    version
        .filter(|version| version.bump != Bump::None)
        .map(|version| ParityRelease {
            requested_bump: version.bump,
            next_version: version.next.to_string(),
        })
}

/// Compute the source Changesets result without invoking Intentional's resolver.
///
/// This deliberately duplicates the small release-propagation algorithm so the
/// takeover gate compares two computations rather than comparing a result with
/// itself.
fn resolve_changesets_source(
    config: &Config,
    declared: &BTreeMap<String, Bump>,
    current: &BTreeMap<String, Version>,
    semantics: &ChangesetsSourceSemantics<'_>,
) -> Result<BTreeMap<String, ReleaseUnitVersion>> {
    let mut effective = config
        .release_units
        .keys()
        .map(|id| {
            let suppressed =
                semantics.suppress_private_versions && semantics.private_packages.contains(id);
            (
                id.clone(),
                if suppressed {
                    Bump::None
                } else {
                    declared.get(id).copied().unwrap_or(Bump::None)
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    loop {
        let before = effective.clone();
        for edge in semantics.npm_dependencies {
            if !config.release_units.contains_key(&edge.dependent)
                || !config.release_units.contains_key(&edge.dependency)
                || (semantics.suppress_private_versions
                    && semantics.private_packages.contains(&edge.dependent))
            {
                continue;
            }
            let dependency_bump = effective[&edge.dependency];
            if dependency_bump == Bump::None {
                continue;
            }
            let next = changesets_dependency_next(config, &edge.dependency, &effective, current);
            let in_range = npm_range_satisfies(&edge.range, &current[&edge.dependency], &next)?;
            let required = if edge.kind == NpmDependencyKind::Peer
                && dependency_bump >= Bump::Minor
                && (!semantics.only_update_peer_dependents_when_out_of_range || !in_range)
            {
                Bump::Major
            } else if semantics.update_internal_dependents_always || !in_range {
                semantics.internal_dependency_bump
            } else {
                Bump::None
            };
            if required != Bump::None {
                effective
                    .entry(edge.dependent.clone())
                    .and_modify(|bump| *bump = (*bump).max(required));
            }
        }
        for group in &config.fixed {
            let bump = group
                .iter()
                .map(|id| effective[id])
                .max()
                .unwrap_or_default();
            if bump != Bump::None {
                for id in group {
                    if !semantics.suppress_private_versions
                        || !semantics.private_packages.contains(id)
                    {
                        effective.insert(id.clone(), bump);
                    }
                }
            }
        }
        for group in &config.linked {
            let bump = group
                .iter()
                .map(|id| effective[id])
                .max()
                .unwrap_or_default();
            if bump != Bump::None {
                for id in group {
                    if effective[id] != Bump::None {
                        effective.insert(id.clone(), bump);
                    }
                }
            }
        }
        if effective == before {
            break;
        }
    }

    let mut resolved = config
        .release_units
        .keys()
        .map(|id| {
            let current_version = current.get(id).ok_or_else(|| {
                Error::Validation(format!("missing current version for release unit {id}"))
            })?;
            Ok((
                id.clone(),
                ReleaseUnitVersion::new_with_mapping(
                    current_version.clone(),
                    effective[id],
                    Pre1BumpMapping::Component,
                ),
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    for (fixed, groups) in [(true, &config.fixed), (false, &config.linked)] {
        for group in groups {
            let highest_current = group
                .iter()
                .map(|id| &current[id])
                .max()
                .expect("validated release group")
                .clone();
            let highest_bump = group
                .iter()
                .map(|id| effective[id])
                .max()
                .unwrap_or_default();
            let next = bump_version_with_mapping(
                &highest_current,
                highest_bump,
                Pre1BumpMapping::Component,
            );
            for id in group {
                let suppressed =
                    semantics.suppress_private_versions && semantics.private_packages.contains(id);
                let releases = !suppressed
                    && highest_bump != Bump::None
                    && (fixed || effective[id] != Bump::None);
                resolved.insert(
                    id.clone(),
                    ReleaseUnitVersion {
                        current: current[id].clone(),
                        next: if releases {
                            next.clone()
                        } else {
                            current[id].clone()
                        },
                        bump: if releases { highest_bump } else { Bump::None },
                    },
                );
            }
        }
    }
    Ok(resolved)
}

fn changesets_dependency_next(
    config: &Config,
    id: &str,
    effective: &BTreeMap<String, Bump>,
    current: &BTreeMap<String, Version>,
) -> Version {
    for group in config.fixed.iter().chain(&config.linked) {
        if group.iter().any(|member| member == id) && effective[id] != Bump::None {
            let highest_current = group
                .iter()
                .map(|member| &current[member])
                .max()
                .expect("validated group");
            let highest_bump = group
                .iter()
                .map(|member| effective[member])
                .max()
                .unwrap_or_default();
            return bump_version_with_mapping(
                highest_current,
                highest_bump,
                Pre1BumpMapping::Component,
            );
        }
    }
    bump_version_with_mapping(&current[id], effective[id], Pre1BumpMapping::Component)
}

fn npm_range_satisfies(range: &str, current: &Version, next: &Version) -> Result<bool> {
    let normalized = match range.strip_prefix("workspace:") {
        Some("*") => current.to_string(),
        Some("^") => format!("^{current}"),
        Some("~") => format!("~{current}"),
        Some(range) => range.to_owned(),
        None => range.to_owned(),
    };
    let Ok(range) = NodeRange::parse(&normalized) else {
        return Ok(false);
    };
    let next = NodeVersion::parse(next.to_string())
        .map_err(|error| Error::Validation(format!("invalid next npm version: {error}")))?;
    Ok(range.satisfies(&next))
}

fn load_previous_plan(root: &Path) -> Result<Option<InitPlan>> {
    let path = root.join(INIT_PLAN_PATH);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let plan: InitPlan = serde_yaml::from_str(&text)?;
            plan.validate()?;
            Ok(Some(plan))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error::io(path, error)),
    }
}

fn evidence(root: &Path, relative: &Path, lines: Vec<usize>) -> Result<SourceEvidence> {
    let path = root.join(relative);
    let bytes = std::fs::read(&path).map_err(|error| Error::io(&path, error))?;
    Ok(SourceEvidence {
        path: relative.to_owned(),
        digest: format!("sha256:{:x}", Sha256::digest(&bytes)),
        lines,
    })
}

fn fingerprint(root: &Path, paths: &BTreeSet<PathBuf>) -> Result<String> {
    let mut source = BTreeMap::new();
    for path in paths {
        if root.join(path).is_file() {
            source.insert(
                path.to_string_lossy().to_string(),
                evidence(root, path, Vec::new())?.digest,
            );
        }
    }
    for path in ["package.json", "pnpm-workspace.yaml", "Cargo.toml"] {
        let path = PathBuf::from(path);
        if root.join(&path).is_file() {
            source.insert(
                path.to_string_lossy().to_string(),
                evidence(root, &path, Vec::new())?.digest,
            );
        }
    }
    for entry in WalkDir::new(root.join(".changeset"))
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry
            .path()
            .strip_prefix(root)
            .unwrap_or(entry.path())
            .to_owned();
        source.insert(
            path.to_string_lossy().to_string(),
            evidence(root, &path, Vec::new())?.digest,
        );
    }
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(canonical_json(&source)?.as_bytes())
    ))
}

fn verify_takeover_preconditions(
    root: &Path,
    writes: &[(PathBuf, String)],
    expected_fingerprint: &str,
    evidence_paths: &BTreeSet<PathBuf>,
) -> Result<()> {
    let actual_fingerprint = fingerprint(root, evidence_paths)?;
    if actual_fingerprint != expected_fingerprint {
        return Err(Error::Validation(
            "initialization plan source evidence became stale; rerun intentional init".to_owned(),
        ));
    }
    for (relative, _) in writes {
        if root.join(relative).exists() {
            return Err(Error::Validation(format!(
                "takeover target {} already exists; resolve the competing Intentional state first",
                relative.display()
            )));
        }
    }
    Ok(())
}

fn takeover_operations(root: &Path, intents: &[ConvertedIntent]) -> Vec<String> {
    let mut operations = vec![format!("write {CONFIG_PATH}")];
    operations.extend(intents.iter().map(|intent| {
        format!(
            "write {} from {}",
            intent.target.display(),
            intent.source.display()
        )
    }));
    operations.extend(
        takeover_deletes(root)
            .iter()
            .map(|path| format!("delete {}", path.display())),
    );
    operations.push("after commit: intentional tag --baseline".to_owned());
    operations
}

fn takeover_deletes(root: &Path) -> Vec<PathBuf> {
    let mut paths = WalkDir::new(root.join(".changeset"))
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| {
            entry
                .path()
                .strip_prefix(root)
                .unwrap_or(entry.path())
                .to_owned()
        })
        .collect::<Vec<_>>();
    paths.push(PathBuf::from(INIT_PLAN_PATH));
    paths.sort();
    paths
}

fn apply_takeover_transaction(
    root: &Path,
    writes: &[(PathBuf, String)],
    deletes: &[PathBuf],
) -> Result<()> {
    let transaction = root.join(TRANSACTION_PATH);
    if transaction.exists() {
        recover_interrupted_takeover(root)?;
    }
    let setup = (|| {
        std::fs::create_dir_all(transaction.join("original"))
            .map_err(|error| Error::io(&transaction, error))?;
        let affected = writes
            .iter()
            .map(|(path, _)| path.clone())
            .chain(deletes.iter().cloned())
            .collect::<BTreeSet<_>>();
        for relative in &affected {
            let source = root.join(relative);
            if source.is_file() {
                let backup = transaction.join("original").join(relative);
                if let Some(parent) = backup.parent() {
                    std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
                }
                std::fs::copy(&source, &backup).map_err(|error| Error::io(&source, error))?;
            }
        }
        let manifest = serde_yaml::to_string(
            &affected
                .iter()
                .map(|path| path.to_string_lossy().to_string())
                .collect::<Vec<_>>(),
        )?;
        let manifest_path = transaction.join("manifest.yml");
        let manifest_temporary = transaction.join("manifest-tmp.yml");
        std::fs::write(&manifest_temporary, manifest)
            .map_err(|error| Error::io(&manifest_temporary, error))?;
        std::fs::rename(&manifest_temporary, &manifest_path)
            .map_err(|error| Error::io(&manifest_path, error))?;
        Ok::<(), Error>(())
    })();
    match setup {
        Ok(()) => {}
        Err(error) => {
            if transaction.exists() {
                std::fs::remove_dir_all(&transaction)
                    .map_err(|cleanup| Error::io(&transaction, cleanup))?;
            }
            return Err(error);
        }
    }
    let result = (|| {
        for (relative, contents) in writes {
            let path = root.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
            }
            let temporary = transaction.join("staged").join(relative);
            if let Some(parent) = temporary.parent() {
                std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
            }
            std::fs::write(&temporary, contents).map_err(|error| Error::io(&temporary, error))?;
            std::fs::rename(&temporary, &path).map_err(|error| Error::io(&path, error))?;
        }
        for relative in deletes {
            let path = root.join(relative);
            if path.is_file() {
                std::fs::remove_file(&path).map_err(|error| Error::io(&path, error))?;
            }
        }
        let changeset_directory = root.join(".changeset");
        if changeset_directory.is_dir()
            && std::fs::read_dir(&changeset_directory)
                .map_err(|error| Error::io(&changeset_directory, error))?
                .next()
                .is_none()
        {
            std::fs::remove_dir(&changeset_directory)
                .map_err(|error| Error::io(&changeset_directory, error))?;
        }
        Ok(())
    })();
    if result.is_err() {
        recover_interrupted_takeover(root)?;
        return result;
    }
    if let Err(error) = write_transaction_state(root, "committed") {
        recover_interrupted_takeover(root)?;
        return Err(error);
    }
    finish_transaction_cleanup(root)
}

fn recover_interrupted_takeover(root: &Path) -> Result<()> {
    let transaction = root.join(TRANSACTION_PATH);
    let state_path = root.join(TRANSACTION_STATE_PATH);
    if state_path.is_file() {
        return finish_transaction_cleanup(root);
    }
    if !transaction.exists() {
        return Ok(());
    }
    let manifest_path = transaction.join("manifest.yml");
    if !manifest_path.is_file() {
        return std::fs::remove_dir_all(&transaction)
            .map_err(|error| Error::io(&transaction, error));
    }
    let manifest = std::fs::read_to_string(&manifest_path)
        .map_err(|error| Error::io(&manifest_path, error))?;
    let affected: Vec<String> = serde_yaml::from_str(&manifest)?;
    for relative in affected {
        let relative = PathBuf::from(relative);
        let target = root.join(&relative);
        let backup = transaction.join("original").join(&relative);
        if backup.is_file() {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
            }
            std::fs::copy(&backup, &target).map_err(|error| Error::io(&target, error))?;
        } else if target.is_file() {
            std::fs::remove_file(&target).map_err(|error| Error::io(&target, error))?;
        }
    }
    write_transaction_state(root, "rolled-back")?;
    finish_transaction_cleanup(root)
}

fn write_transaction_state(root: &Path, state: &str) -> Result<()> {
    let path = root.join(TRANSACTION_STATE_PATH);
    let temporary = root.join(".intentional/.takeover-state-tmp");
    std::fs::write(&temporary, state).map_err(|error| Error::io(&temporary, error))?;
    std::fs::rename(&temporary, &path).map_err(|error| Error::io(&path, error))
}

fn finish_transaction_cleanup(root: &Path) -> Result<()> {
    let transaction = root.join(TRANSACTION_PATH);
    if transaction.exists() {
        std::fs::remove_dir_all(&transaction).map_err(|error| Error::io(&transaction, error))?;
    }
    for path in [
        root.join(TRANSACTION_STATE_PATH),
        root.join(".intentional/.takeover-state-tmp"),
    ] {
        if path.is_file() {
            std::fs::remove_file(&path).map_err(|error| Error::io(&path, error))?;
        }
    }
    Ok(())
}

fn annotate_choice_lines(yaml: &str) -> String {
    let comments = BTreeMap::from([
        ("suspended", "configured, but releases are blocked"),
        ("excluded", "outside Intentional's release-unit inventory"),
        ("managed", "configured and eligible for release"),
        (
            "removed",
            "reference removed or replaced and verified from the file",
        ),
        (
            "external",
            "publication sequencing remains with the external executor",
        ),
        ("intentional", "Intentional's contract owns this behavior"),
    ]);
    let mut output = String::new();
    let mut choices_indent = None;
    for line in yaml.lines() {
        let indent = line.len() - line.trim_start().len();
        if line.trim() == "choices:" {
            choices_indent = Some(indent);
            output.push_str(line);
            output.push('\n');
            continue;
        }
        if let Some(base) = choices_indent {
            if let Some(value) = line.trim().strip_prefix("- ") {
                if let Some(comment) = comments.get(value) {
                    output.push_str(line);
                    output.push_str(" # ");
                    output.push_str(comment);
                    output.push('\n');
                    continue;
                }
            } else if indent <= base {
                choices_indent = None;
            }
        }
        output.push_str(line);
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "sha256:0000000000000000000000000000000000000000000000000000000000000000";

    fn candidate(path: &str, resolution: Option<CandidateResolution>) -> DiscoveryCandidate {
        let path = PathBuf::from(path);
        let evidence = SourceEvidence {
            path: path.clone(),
            digest: DIGEST.to_owned(),
            lines: vec![1],
        };
        DiscoveryCandidate {
            id: DiscoveryCandidate::stable_id("sample-detector", &path).expect("stable id"),
            detector: "sample-detector".to_owned(),
            path: path.clone(),
            evidence: vec![evidence.clone()],
            native_identity: Some("sample-native-id".to_owned()),
            raw_version: Some(RawVersionEvidence {
                value: "raw-version-text".to_owned(),
                evidence: vec![evidence.clone()],
            }),
            projection: Some(CandidateProjectionSuggestion {
                adapter: Adapter::Npm,
                path,
                mode: ProjectionMode::Committed,
                pointer: None,
            }),
            tag: Some(CandidateTagSuggestion {
                id: "primary".to_owned(),
                role: TagRole::Primary,
                template: "{id}@{version}".to_owned(),
            }),
            diagnostics: vec![ExtractionDiagnostic {
                id: "sample-diagnostic".to_owned(),
                code: "sample-extraction".to_owned(),
                message: "Sample extraction evidence is incomplete.".to_owned(),
                evidence: vec![evidence],
            }],
            resolution,
        }
    }

    fn candidate_plan(candidates: Vec<DiscoveryCandidate>) -> InitPlan {
        let config = Config::from_yaml(
            "contract: contract-1\nrelease-units:\n  configured:\n    path: configured\n    tags:\n      primary: { role: primary, template: '{id}@{version}' }\n",
        )
        .expect("candidate test config");
        InitPlan {
            schema: INIT_PLAN_SCHEMA.to_owned(),
            state: InitState::NeedsInput,
            source_kind: "changesets".to_owned(),
            source_fingerprint: DIGEST.to_owned(),
            inferred_config: config,
            discovery_candidates: candidates,
            diagnostics: Vec::new(),
            converted_intents: Vec::new(),
            parity: ParityResult {
                status: "equivalent".to_owned(),
                release_units: Vec::new(),
            },
            planned_operations: Vec::new(),
            post_commit_action: "intentional tag --baseline".to_owned(),
        }
    }

    #[test]
    fn recognizes_supported_manifests() {
        assert_eq!(adapter_for(Path::new("package.json")), Some(Adapter::Npm));
        assert_eq!(
            adapter_for(Path::new("module.csproj")),
            Some(Adapter::Msbuild)
        );
        assert_eq!(adapter_for(Path::new("README.md")), None);
    }

    #[test]
    fn choice_rendering_is_compact_and_explained() {
        let yaml = "choices:\n- suspended\n- excluded\nresolution: null\n";
        let rendered = annotate_choice_lines(yaml);
        assert!(rendered.contains("- suspended # configured, but releases are blocked"));
        assert!(rendered.contains("- excluded # outside Intentional's release-unit inventory"));
    }

    #[test]
    fn candidate_identity_and_detector_results_are_stable() {
        let first = candidate(
            "examples/first.json",
            Some(CandidateResolution::Independent {
                release_unit: "sample-unit".to_owned(),
            }),
        );
        let second = candidate("examples/second.json", Some(CandidateResolution::Excluded));
        assert_eq!(
            first.id,
            DiscoveryCandidate::stable_id("sample-detector", Path::new("examples/first.json"))
                .expect("same identity")
        );
        assert_ne!(first.id, second.id);
        assert_ne!(
            first.id,
            DiscoveryCandidate::stable_id("other-detector", Path::new("examples/first.json"))
                .expect("different detector")
        );
        assert!(DiscoveryCandidate::stable_id(
            "-invalid-detector",
            Path::new("examples/first.json")
        )
        .is_err());

        let result = DetectorResult {
            detector: "sample-detector".to_owned(),
            candidates: vec![second.clone(), first.clone()],
        };
        let ordered = result.into_candidates().expect("detector result valid");
        assert!(ordered[0].path < ordered[1].path);

        let mut tampered = first.clone();
        tampered.id = format!("candidate:{}", "0".repeat(64));
        assert!(candidate_plan(vec![tampered])
            .validate()
            .expect_err("tampered candidate id rejected")
            .to_string()
            .contains("does not match detector"));

        let foreign = DetectorResult {
            detector: "other-detector".to_owned(),
            candidates: vec![first.clone()],
        };
        assert!(foreign
            .validate()
            .expect_err("foreign detector candidate rejected")
            .to_string()
            .contains("returned candidate owned by"));

        let yaml = candidate_plan(vec![first, second])
            .to_yaml()
            .expect("candidate plan serializes");
        assert!(yaml.contains("kind: independent"));
        assert!(yaml.contains("kind: excluded"));
        assert!(yaml.contains("raw-version:"));
        assert!(yaml.contains("diagnostics:"));
    }

    #[test]
    fn rejects_plan_without_required_discovery_candidates() {
        let yaml = candidate_plan(Vec::new())
            .to_yaml()
            .expect("complete plan serializes");
        assert!(yaml.contains("discovery-candidates: []"));
        let incomplete = yaml.replace("discovery-candidates: []\n", "");
        let error = serde_yaml::from_str::<InitPlan>(&incomplete)
            .expect_err("missing discovery candidate collection rejected");
        assert!(error
            .to_string()
            .contains("missing field `discovery-candidates`"));
    }

    #[test]
    fn candidate_tag_templates_use_the_shared_contract() {
        for template in ["v{version}", "{version}-{version}", "{id}-{id}@{version}"] {
            let mut invalid = candidate("examples/template.json", None);
            invalid.tag.as_mut().expect("tag suggestion").template = template.to_owned();
            assert!(candidate_plan(vec![invalid])
                .validate()
                .expect_err("invalid candidate tag template rejected")
                .to_string()
                .contains("tag template"));
        }
    }

    #[test]
    fn published_init_schema_carries_candidate_resolution_variants() {
        let schema: serde_yaml::Value =
            serde_yaml::from_str(include_str!("../../../schemas/init-plan.yml"))
                .expect("initialization-plan schema parses");
        assert!(schema["required"]
            .as_sequence()
            .expect("required plan fields")
            .iter()
            .any(|field| field.as_str() == Some("discovery-candidates")));
        let variants = schema["$defs"]["candidate-resolution"]["oneOf"]
            .as_sequence()
            .expect("candidate resolution variants");
        assert_eq!(variants.len(), 3);
        let kinds = variants
            .iter()
            .map(|variant| {
                variant["properties"]["kind"]["const"]
                    .as_str()
                    .expect("resolution kind")
            })
            .collect::<BTreeSet<_>>();
        assert_eq!(
            kinds,
            BTreeSet::from(["excluded", "independent", "projection"])
        );
        assert_eq!(
            schema["$defs"]["discovery-candidate"]["properties"]["tag"]["properties"]["template"]
                ["pattern"]
                .as_str(),
            Some(
                r"^(?![\s\S]*v\{version\})(?![\s\S]*\{version\}[\s\S]*\{version\})(?![\s\S]*\{id\}[\s\S]*\{id\})(?=[\s\S]*\{version\})[\s\S]*$"
            )
        );
    }

    #[test]
    fn accepts_configured_and_same_plan_projection_targets() {
        let configured = candidate(
            "examples/configured.json",
            Some(CandidateResolution::Projection {
                release_unit: "configured".to_owned(),
                target_candidate: None,
            }),
        );
        let independent = candidate(
            "examples/independent.json",
            Some(CandidateResolution::Independent {
                release_unit: "planned".to_owned(),
            }),
        );
        let projection = candidate(
            "examples/projection.json",
            Some(CandidateResolution::Projection {
                release_unit: "planned".to_owned(),
                target_candidate: Some(independent.id.clone()),
            }),
        );
        candidate_plan(vec![configured, independent, projection])
            .validate()
            .expect("both projection target kinds accepted");
    }

    #[test]
    fn rejects_duplicate_creators_and_absent_projection_targets() {
        let first = candidate(
            "examples/first.json",
            Some(CandidateResolution::Independent {
                release_unit: "duplicate".to_owned(),
            }),
        );
        let second = candidate(
            "examples/second.json",
            Some(CandidateResolution::Independent {
                release_unit: "duplicate".to_owned(),
            }),
        );
        assert!(candidate_plan(vec![first, second])
            .validate()
            .expect_err("duplicate creators rejected")
            .to_string()
            .contains("duplicate creator"));

        let absent = candidate(
            "examples/absent.json",
            Some(CandidateResolution::Projection {
                release_unit: "missing".to_owned(),
                target_candidate: None,
            }),
        );
        assert!(candidate_plan(vec![absent])
            .validate()
            .expect_err("absent configured target rejected")
            .to_string()
            .contains("absent configured release unit"));
    }

    #[test]
    fn rejects_projection_cycles_and_release_unit_mismatches() {
        let mut first = candidate("examples/first.json", None);
        let mut second = candidate("examples/second.json", None);
        first.resolution = Some(CandidateResolution::Projection {
            release_unit: "planned".to_owned(),
            target_candidate: Some(second.id.clone()),
        });
        second.resolution = Some(CandidateResolution::Projection {
            release_unit: "planned".to_owned(),
            target_candidate: Some(first.id.clone()),
        });
        assert!(candidate_plan(vec![first, second])
            .validate()
            .expect_err("candidate cycle rejected")
            .to_string()
            .contains("projection cycle"));

        let independent = candidate(
            "examples/independent.json",
            Some(CandidateResolution::Independent {
                release_unit: "planned".to_owned(),
            }),
        );
        let mismatched = candidate(
            "examples/mismatched.json",
            Some(CandidateResolution::Projection {
                release_unit: "other".to_owned(),
                target_candidate: Some(independent.id.clone()),
            }),
        );
        assert!(candidate_plan(vec![independent, mismatched])
            .validate()
            .expect_err("mismatched target rejected")
            .to_string()
            .contains("target"));

        let independent = candidate(
            "examples/independent.json",
            Some(CandidateResolution::Independent {
                release_unit: "planned".to_owned(),
            }),
        );
        let intermediate = candidate(
            "examples/intermediate.json",
            Some(CandidateResolution::Projection {
                release_unit: "planned".to_owned(),
                target_candidate: Some(independent.id.clone()),
            }),
        );
        let chained = candidate(
            "examples/chained.json",
            Some(CandidateResolution::Projection {
                release_unit: "planned".to_owned(),
                target_candidate: Some(intermediate.id.clone()),
            }),
        );
        assert!(candidate_plan(vec![independent, intermediate, chained])
            .validate()
            .expect_err("projection chain rejected")
            .to_string()
            .contains("not an independent creator"));
    }
}
