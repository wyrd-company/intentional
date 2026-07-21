// ---
// relationships:
//   implements: intent-driven-polyglot-release
// ---

//! Workspace-aware initialization and explicit Changesets takeover.

use crate::config::{
    Config, PackageConfig, Projection, TagConfig, WorkspaceTagConfig, CONFIG_PATH,
};
use crate::error::{Error, Result};
use crate::model::{
    Adapter, Bump, PackageDisposition, Pre1BumpMapping, ProjectionMode, TagPhase, TagRole,
};
use crate::plan::canonical_json;
use crate::version::{bump_version_with_mapping, resolve_versions, PackageVersion};
use glob::glob;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

/// Transient initialization-plan location.
pub const INIT_PLAN_PATH: &str = ".intentional/init-plan.yml";

/// Published initialization-plan schema identifier.
pub const INIT_PLAN_SCHEMA: &str = "https://intentional.foo/schemas/init-plan.yml";

const CHANGESETS_CONFIG: &str = ".changeset/config.json";
const TRANSACTION_PATH: &str = ".intentional/.takeover-transaction";

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
pub struct ParityPackage {
    /// Logical package id.
    pub package: String,
    /// Version in source projections.
    pub current_version: String,
    /// Independently computed Changesets result, when the source releases this package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ParityRelease>,
    /// Proposed Intentional result, when the proposed contract releases this package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed: Option<ParityRelease>,
}

/// Deterministic parity comparison.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ParityResult {
    /// `equivalent` when all logical releases agree; otherwise `blocked`.
    pub status: String,
    /// Per-package release results.
    pub packages: Vec<ParityPackage>,
}

/// One pending Changesets file converted losslessly at takeover.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct ConvertedIntent {
    /// Changeset filename stem retained as the Intentional intent id.
    pub id: String,
    /// Logical package bumps.
    pub packages: BTreeMap<String, Bump>,
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
            serde_yaml::to_string(&self.packages)?,
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
    /// Serialize deterministically with compact, explained YAML enum choices.
    pub fn to_yaml(&self) -> Result<String> {
        let yaml = serde_yaml::to_string(self)?;
        Ok(annotate_choice_lines(&yaml))
    }

    /// Equivalent structured JSON for agent consumers.
    pub fn to_json(&self) -> Result<String> {
        canonical_json(self)
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
            if path.exists() && relative == Path::new(CONFIG_PATH) {
                return Err(Error::Validation(format!(
                    "configuration already exists at {}",
                    relative.display()
                )));
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
            }
            std::fs::write(&path, contents).map_err(|error| Error::io(&path, error))?;
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
    recover_interrupted_takeover(root)?;
    if root.join(CONFIG_PATH).exists() && !root.join(CHANGESETS_CONFIG).exists() {
        return Err(Error::Validation(format!(
            "configuration already exists at {CONFIG_PATH}"
        )));
    }
    if root.join(CHANGESETS_CONFIG).exists() {
        return changesets_plan(root, scan_all, take_over);
    }
    if take_over {
        return Err(Error::Validation(
            "--take-over requires an existing .changeset/config.json".to_owned(),
        ));
    }
    let discovery = discover(root, scan_all, &BTreeSet::new())?;
    let contents = discovery.config.to_yaml()?;
    Ok(InitResult {
        state: InitState::Success,
        path: PathBuf::from(CONFIG_PATH),
        operations: vec![
            format!("write {CONFIG_PATH}"),
            format!("create {}", crate::intent::INTENTS_PATH),
        ],
        contents: contents.clone(),
        plan: None,
        writes: vec![(PathBuf::from(CONFIG_PATH), contents)],
        deletes: Vec::new(),
        takeover: false,
        takeover_evidence: None,
    })
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
}

fn changesets_plan(root: &Path, scan_all: bool, take_over: bool) -> Result<InitResult> {
    let previous = load_previous_plan(root)?;
    let changesets_config_path = root.join(CHANGESETS_CONFIG);
    let changesets_text = std::fs::read_to_string(&changesets_config_path)
        .map_err(|error| Error::io(&changesets_config_path, error))?;
    let changesets: JsonValue = serde_json::from_str(&changesets_text)
        .map_err(|error| Error::Validation(format!("invalid Changesets config: {error}")))?;
    let converted_intents = load_changesets_intents(root)?;
    let mut referenced_names = converted_intents
        .iter()
        .flat_map(|intent| intent.packages.keys().cloned())
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
    discovery.config.settings.pre_1_0_bump_mapping = Pre1BumpMapping::Component;
    discovery.config.settings.internal_dependency_bump = changesets["updateInternalDependencies"]
        .as_str()
        .and_then(|value| value.parse().ok())
        .unwrap_or(Bump::Patch);
    discovery.config.fixed = parse_groups(&changesets["fixed"])?;
    discovery.config.linked = parse_groups(&changesets["linked"])?;
    merge_release_profile(root, &mut discovery)?;

    let mut diagnostics = Vec::new();
    let config_evidence = evidence(root, Path::new(CHANGESETS_CONFIG), Vec::new())?;
    for ignored in changesets["ignore"].as_array().into_iter().flatten() {
        let Some(package) = ignored.as_str() else {
            continue;
        };
        diagnostics.push(InitDiagnostic {
            id: format!("ignored-package-disposition:{package}"),
            code: "ignored-package-disposition".to_owned(),
            message: format!(
                "Choose whether Changesets-ignored package {package} is suspended, excluded, or managed."
            ),
            evidence: vec![config_evidence.clone()],
            choices: vec!["suspended".to_owned(), "excluded".to_owned(), "managed".to_owned()],
            recommended: Some("suspended".to_owned()),
            resolution: None,
            verified: false,
            invalidated_resolution: false,
        });
    }
    if let Some(private_packages) = changesets["privatePackages"].as_object() {
        let versions_private = private_packages
            .get("version")
            .and_then(JsonValue::as_bool)
            .unwrap_or(true);
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
        if private_packages.get("tag").and_then(JsonValue::as_bool) == Some(false) {
            diagnostics.push(InitDiagnostic {
                id: "private-package-tagging".to_owned(),
                code: "private-package-tagging".to_owned(),
                message: "Changesets suppresses private-package tags; Intentional creates annotated records for every managed logical release, independently from publication privacy.".to_owned(),
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
            message: "Intentional renders its contract-defined logical-package changelogs instead of invoking the configured Changesets changelog generator.".to_owned(),
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
    for package in discovery.config.packages.keys() {
        if referenced_names.contains(package) {
            continue;
        }
        let package_config = &discovery.config.packages[package];
        let manifest = package_config
            .projections
            .first()
            .map(|projection| package_config.path.join(&projection.file))
            .ok_or_else(|| {
                Error::Validation(format!(
                    "discovered package {package} has no manifest evidence"
                ))
            })?;
        diagnostics.push(InitDiagnostic {
            id: format!("unmapped-package-disposition:{package}"),
            code: "unmapped-package-disposition".to_owned(),
            message: format!(
                "Choose whether workspace package {package}, which is outside the Changesets release inventory, is excluded, suspended, or managed."
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
    exclude_packages(&mut source_config, &ignored);
    apply_disposition_resolutions(&mut discovery.config, &diagnostics)?;
    discovery.config.validate()?;

    let declared: BTreeMap<String, Bump> =
        converted_intents
            .iter()
            .fold(BTreeMap::new(), |mut aggregate, intent| {
                for (id, bump) in &intent.packages {
                    aggregate
                        .entry(id.clone())
                        .and_modify(|existing| *existing = (*existing).max(*bump))
                        .or_insert(*bump);
                }
                aggregate
            });
    let parity = parity_result(
        &source_config,
        &discovery.config,
        &discovery.versions,
        &declared,
    )?;
    let unresolved = diagnostics.iter().any(|diagnostic| {
        !diagnostic.choices.is_empty() && (diagnostic.resolution.is_none() || !diagnostic.verified)
    });
    let state = if unresolved || parity.status != "equivalent" {
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
        diagnostics,
        converted_intents,
        parity,
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
        let current_fingerprint = fingerprint(root, &evidence_paths)?;
        if current_fingerprint != plan.source_fingerprint {
            return Err(Error::Validation(
                "initialization plan source evidence became stale".to_owned(),
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
    let mut manifest_paths = if scan_all {
        all_manifest_paths(root)
    } else {
        workspace_manifest_paths(root)?
    };
    if !referenced_names.is_empty() {
        for path in all_manifest_paths(root) {
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
        if let Some(existing) = discovery.config.packages.get_mut(&id) {
            if existing.path != package_path {
                return Err(Error::Validation(format!(
                    "manifest-native package name {id} is declared in both {} and {}",
                    existing.path.display(),
                    package_path.display()
                )));
            }
            existing.projections.push(projection);
        } else {
            discovery.config.packages.insert(
                id.clone(),
                PackageConfig {
                    path: package_path,
                    disposition: PackageDisposition::Managed,
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
            discovery.versions.insert(id, Version::parse(&version)?);
        }
        discovery
            .evidence
            .insert(path.strip_prefix(root).unwrap_or(&path).to_owned());
    }
    if discovery.config.packages.is_empty() {
        return Err(Error::Validation(
            "no supported workspace manifests found".to_owned(),
        ));
    }
    derive_npm_dependencies(root, &mut discovery.config)?;
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
        for pattern in value["packages"].as_sequence().into_iter().flatten() {
            if let Some(pattern) = pattern.as_str() {
                expand_workspace_pattern(root, pattern, &mut directories)?;
            }
        }
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
            for pattern in workspaces.iter().filter_map(JsonValue::as_str) {
                expand_workspace_pattern(root, pattern, &mut directories)?;
            }
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
            for member in members.iter().filter_map(toml_edit::Value::as_str) {
                expand_workspace_pattern(root, member, &mut directories)?;
            }
        }
    }
    if !found_workspace {
        directories.insert(root.to_owned());
    }
    let mut paths = BTreeSet::new();
    for directory in directories {
        add_manifests_in_directory(&directory, &mut paths)?;
    }
    Ok(paths)
}

fn expand_workspace_pattern(
    root: &Path,
    pattern: &str,
    directories: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let absolute = root.join(pattern).to_string_lossy().to_string();
    for entry in glob(&absolute).map_err(|error| {
        Error::Validation(format!("invalid workspace pattern {pattern}: {error}"))
    })? {
        let path = entry
            .map_err(|error| Error::Validation(format!("workspace pattern failed: {error}")))?;
        if path.is_dir() {
            directories.insert(path);
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

fn all_manifest_paths(root: &Path) -> BTreeSet<PathBuf> {
    WalkDir::new(root)
        .into_iter()
        .filter_entry(should_visit)
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_type().is_file() && adapter_for(entry.path()).is_some())
        .map(|entry| entry.into_path())
        .collect()
}

fn should_visit(entry: &DirEntry) -> bool {
    !matches!(
        entry.file_name().to_str(),
        Some(".git" | ".intentional" | "node_modules" | "target")
    )
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

fn derive_npm_dependencies(root: &Path, config: &mut Config) -> Result<()> {
    let ids = config.packages.keys().cloned().collect::<BTreeSet<_>>();
    for package in config.packages.values_mut() {
        let npm = package
            .projections
            .iter()
            .find(|projection| projection.adapter == Adapter::Npm);
        let Some(npm) = npm else { continue };
        let path = root.join(&package.path).join(&npm.file);
        let text = std::fs::read_to_string(&path).map_err(|error| Error::io(&path, error))?;
        let value: JsonValue = serde_json::from_str(&text)
            .map_err(|error| Error::Validation(format!("invalid {}: {error}", path.display())))?;
        let mut dependencies = BTreeSet::new();
        for group in ["dependencies", "peerDependencies", "optionalDependencies"] {
            if let Some(entries) = value[group].as_object() {
                dependencies.extend(entries.keys().filter(|id| ids.contains(*id)).cloned());
            }
        }
        package.depends_on = dependencies.into_iter().collect();
    }
    Ok(())
}

fn load_changesets_intents(root: &Path) -> Result<Vec<ConvertedIntent>> {
    let directory = root.join(".changeset");
    let mut paths = std::fs::read_dir(&directory)
        .map_err(|error| Error::io(&directory, error))?
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "md"))
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
                packages,
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

fn merge_release_profile(root: &Path, discovery: &mut Discovery) -> Result<()> {
    let Some(profile) = load_release_profile(root)? else {
        return Ok(());
    };
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
        let Some(projected) = discovery.config.packages.remove(name) else {
            continue;
        };
        let source_package = discovery.config.packages.get_mut(source).ok_or_else(|| {
            Error::Validation(format!(
                "release profile versionSource {source} for {name} was not discovered"
            ))
        })?;
        for mut projection in projected.projections {
            let absolute = root.join(&projected.path).join(&projection.file);
            projection.file = absolute
                .strip_prefix(root.join(&source_package.path))
                .map_err(|_| {
                    Error::Validation(format!(
                        "projection {} is outside logical package {source}",
                        absolute.display()
                    ))
                })?
                .to_owned();
            source_package.projections.push(projection);
        }
        source_package.tags.insert(
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
    discovery.config.workspace_tags.insert(
        "release".to_owned(),
        WorkspaceTagConfig {
            template: "{version}".to_owned(),
            require_phase: None,
            tag_after: Vec::new(),
        },
    );
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
        diagnostic.code == "ignored-package-disposition"
            || diagnostic.code == "unmapped-package-disposition"
    }) {
        let package = diagnostic
            .id
            .split_once(':')
            .map(|(_, package)| package)
            .expect("diagnostic id prefix");
        match diagnostic.resolution.as_deref() {
            Some("suspended") => {
                if let Some(config) = config.packages.get_mut(package) {
                    config.disposition = PackageDisposition::Suspended;
                }
            }
            Some("excluded") => {
                config.packages.remove(package);
                excluded.insert(package.to_owned());
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
    exclude_packages(config, &excluded);
    Ok(())
}

fn exclude_packages(config: &mut Config, excluded: &BTreeSet<String>) {
    for id in excluded {
        config.packages.remove(id);
    }
    if excluded.is_empty() {
        return;
    }
    for package in config.packages.values_mut() {
        package.depends_on.retain(|id| !excluded.contains(id));
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
) -> Result<ParityResult> {
    let missing = source_config
        .packages
        .keys()
        .chain(proposed_config.packages.keys())
        .filter(|id| !current.contains_key(*id))
        .cloned()
        .collect::<BTreeSet<_>>();
    if !missing.is_empty() {
        return Ok(ParityResult {
            status: "blocked".to_owned(),
            packages: Vec::new(),
        });
    }
    let source = resolve_changesets_source(source_config, declared, current)?;
    let proposed = match resolve_versions(proposed_config, declared, current) {
        Ok(resolved) => resolved,
        Err(Error::Validation(_)) => BTreeMap::new(),
        Err(error) => return Err(error),
    };
    let release_ids = source
        .iter()
        .chain(&proposed)
        .filter(|(_, versions)| versions.bump != Bump::None)
        .map(|(id, _)| id.clone())
        .collect::<BTreeSet<_>>();
    let packages = release_ids
        .into_iter()
        .map(|package| ParityPackage {
            current_version: current[&package].to_string(),
            source: parity_release(source.get(&package)),
            proposed: parity_release(proposed.get(&package)),
            package,
        })
        .collect::<Vec<_>>();
    let equivalent = packages
        .iter()
        .all(|package| package.source == package.proposed);
    Ok(ParityResult {
        status: if equivalent { "equivalent" } else { "blocked" }.to_owned(),
        packages,
    })
}

fn parity_release(version: Option<&PackageVersion>) -> Option<ParityRelease> {
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
) -> Result<BTreeMap<String, PackageVersion>> {
    let mut effective = config
        .packages
        .keys()
        .map(|id| (id.clone(), declared.get(id).copied().unwrap_or(Bump::None)))
        .collect::<BTreeMap<_, _>>();
    loop {
        let before = effective.clone();
        for (id, package) in &config.packages {
            if package
                .depends_on
                .iter()
                .any(|dependency| effective[dependency] != Bump::None)
            {
                effective.entry(id.clone()).and_modify(|bump| {
                    *bump = (*bump).max(config.settings.internal_dependency_bump)
                });
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
                    effective.insert(id.clone(), bump);
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
        .packages
        .keys()
        .map(|id| {
            let current_version = current.get(id).ok_or_else(|| {
                Error::Validation(format!("missing current version for package {id}"))
            })?;
            Ok((
                id.clone(),
                PackageVersion::new_with_mapping(
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
                let releases = highest_bump != Bump::None && (fixed || effective[id] != Bump::None);
                resolved.insert(
                    id.clone(),
                    PackageVersion {
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

fn load_previous_plan(root: &Path) -> Result<Option<InitPlan>> {
    let path = root.join(INIT_PLAN_PATH);
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_yaml::from_str(&text).map(Some).map_err(Error::from),
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
    let result = (|| {
        for (relative, contents) in writes {
            let path = root.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| Error::io(parent, error))?;
            }
            let temporary = path.with_extension("intentional-tmp");
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
    std::fs::remove_dir_all(&transaction).map_err(|error| Error::io(&transaction, error))?;
    Ok(())
}

fn recover_interrupted_takeover(root: &Path) -> Result<()> {
    let transaction = root.join(TRANSACTION_PATH);
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
    std::fs::remove_dir_all(&transaction).map_err(|error| Error::io(&transaction, error))
}

fn annotate_choice_lines(yaml: &str) -> String {
    let comments = BTreeMap::from([
        ("suspended", "configured, but releases are blocked"),
        ("excluded", "outside Intentional's package inventory"),
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
        assert!(rendered.contains("- excluded # outside Intentional's package inventory"));
    }
}
